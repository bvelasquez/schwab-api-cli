use anyhow::{Context, Result};
use console::Style;
use schwab_api::ClientConfig;
use serde_json::json;
use std::time::Duration;

use crate::auth_callback::capture_redirect_code;
use crate::auth_reminder::assess_refresh_token;
use crate::cli::AuthCommands;
use crate::config::RuntimeConfig;
use crate::output::ResponseEnvelope;

pub async fn run(runtime: &RuntimeConfig, command: AuthCommands) -> Result<()> {
    match command {
        AuthCommands::Login { code } => login(runtime, code).await,
        AuthCommands::Status => status(runtime).await,
        AuthCommands::Refresh => refresh(runtime).await,
        AuthCommands::Logout => logout(runtime).await,
    }
}

async fn login(runtime: &RuntimeConfig, code: Option<String>) -> Result<()> {
    if runtime.dry_run {
        let envelope = ResponseEnvelope::ok(
            "auth login",
            json!({ "dry_run": true, "message": "Would run OAuth login flow" }),
        );
        runtime.emit(envelope);
        return Ok(());
    }

    let config = ClientConfig::from_env().context("Missing SCHWAB_APP_KEY / SCHWAB_APP_SECRET")?;
    let oauth = schwab_api::OAuthClient::new(config.clone());
    let authorize_url = oauth.authorize_url();

    let auth_code = if let Some(c) = code {
        c
    } else {
        let redirect_uri = config.redirect_uri.clone();
        let use_https = redirect_uri.starts_with("https://");

        println!(
            "{}",
            Style::new()
                .cyan()
                .apply_to("Starting local OAuth callback listener…")
        );
        if use_https {
            println!(
                "{}",
                Style::new().yellow().apply_to(
                    "Listening on https://127.0.0.1:8182. \
                     When redirected, accept the self-signed certificate (Advanced → Proceed)."
                )
            );
        }

        let capture = tokio::spawn(capture_redirect_code(
            redirect_uri,
            Duration::from_secs(120),
        ));

        if runtime.is_tty() {
            inquire::Confirm::new("Ready to open Schwab login in your browser?")
                .with_default(true)
                .with_help_message("Authorization codes expire in ~30 seconds after redirect")
                .prompt()?;
        }

        let _ = webbrowser::open(&authorize_url);
        println!(
            "{}",
            Style::new().dim().apply_to(
                "Complete login in the browser. The CLI will capture the redirect automatically."
            )
        );

        match capture.await {
            Ok(Ok(Some(code))) => code,
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) if runtime.is_tty() => {
                println!(
                    "{}",
                    Style::new().yellow().apply_to(
                        "Auto-capture did not finish. After accepting the certificate in the browser, \
                         paste the redirect URL immediately (codes expire in ~30 seconds)."
                    )
                );
                inquire::Text::new("Paste redirect URL or authorization code")
                    .with_help_message("From https://127.0.0.1:8182/?code=… in the address bar")
                    .prompt()?
            }
            Ok(Ok(None)) => anyhow::bail!(
                "OAuth callback timed out. Run: schwab auth login --code '<redirect-url>'"
            ),
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(e.into()),
        }
    };

    let parsed_code = extract_auth_code(&auth_code);
    let tokens = oauth.exchange_code(&parsed_code).await.map_err(|e| {
        anyhow::anyhow!(
            "Token exchange failed: {e}. \
             Authorization codes expire in ~30 seconds — run `schwab auth login` again and complete quickly. \
             Also verify SCHWAB_REDIRECT_URI is exactly https://127.0.0.1:8182"
        )
    })?;

    let envelope = ResponseEnvelope::ok(
        "auth login",
        json!({
            "authenticated": true,
            "expires_at": tokens.expires_at,
            "expires_in_seconds": tokens.expires_in_seconds(),
            "token_path": oauth.store().path(),
        }),
    )
    .with_next_actions(vec![
        "schwab auth status --json".into(),
        "schwab accounts numbers --json".into(),
    ]);
    runtime.emit(envelope);
    Ok(())
}

async fn status(runtime: &RuntimeConfig) -> Result<()> {
    let config = ClientConfig::from_env().context("Missing Schwab app credentials")?;
    let oauth = schwab_api::OAuthClient::new(config);
    let tokens = oauth.status().await?;

    let data = match tokens {
        Some(t) => {
            let reminder = assess_refresh_token(&t);
            json!({
                "authenticated": true,
                "expires_at": t.expires_at,
                "expires_in_seconds": t.expires_in_seconds(),
                "expired": t.is_expired(),
                "obtained_at": t.obtained_at,
                "refresh_expires_in_seconds": t.refresh_expires_in_seconds(),
                "auth_reminder": {
                    "level": reminder.level.as_str(),
                    "message": reminder.message,
                },
                "token_path": oauth.store().path(),
            })
        }
        None => json!({
            "authenticated": false,
            "token_path": oauth.store().path(),
            "hint": "Browser login alone is not enough — run `schwab auth login` and paste the redirect URL containing code="
        }),
    };

    let mut envelope = ResponseEnvelope::ok("auth status", data);
    let auth_needs_login = !envelope.data["authenticated"].as_bool().unwrap_or(false)
        || envelope.data["auth_reminder"]["level"]
            .as_str()
            .is_some_and(|l| l == "urgent" || l == "expired");
    if auth_needs_login {
        envelope.next_actions = vec!["schwab auth login".into()];
    }
    runtime.emit(envelope);
    Ok(())
}

async fn refresh(runtime: &RuntimeConfig) -> Result<()> {
    use crate::safety::require_mutation_approval;
    require_mutation_approval(runtime, "auth refresh", "Refresh OAuth access token.")?;
    if runtime.dry_run {
        runtime.emit(ResponseEnvelope::ok(
            "auth refresh",
            json!({ "dry_run": true }),
        ));
        return Ok(());
    }

    let config = ClientConfig::from_env()?;
    let oauth = schwab_api::OAuthClient::new(config);
    let tokens = oauth.refresh().await?;

    runtime.emit(ResponseEnvelope::ok(
        "auth refresh",
        json!({
            "expires_at": tokens.expires_at,
            "expires_in_seconds": tokens.expires_in_seconds(),
        }),
    ));
    Ok(())
}

async fn logout(runtime: &RuntimeConfig) -> Result<()> {
    use crate::safety::require_mutation_approval;
    require_mutation_approval(runtime, "auth logout", "Delete stored OAuth tokens.")?;
    if runtime.dry_run {
        runtime.emit(ResponseEnvelope::ok(
            "auth logout",
            json!({ "dry_run": true }),
        ));
        return Ok(());
    }

    let config = ClientConfig::from_env()?;
    let oauth = schwab_api::OAuthClient::new(config);
    oauth.logout().await?;
    runtime.emit(ResponseEnvelope::ok(
        "auth logout",
        json!({ "cleared": true, "token_path": oauth.store().path() }),
    ));
    Ok(())
}

fn extract_auth_code(raw: &str) -> String {
    let normalized: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let code = if let Some(idx) = normalized.find("code=") {
        let rest = &normalized[idx + 5..];
        rest.split('&').next().unwrap_or(rest).trim_end_matches('/')
    } else {
        normalized.as_str()
    };
    urlencoding::decode(code)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| code.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_code_from_redirect_url() {
        let url = "https://127.0.0.1:8182/?code=ABC123&session=xyz";
        assert_eq!(extract_auth_code(url), "ABC123");
    }

    #[test]
    fn extracts_code_with_line_breaks() {
        let url = "https://127.0.0.1:8182/?code=ABC%40123&session=abc-\ndef";
        assert_eq!(extract_auth_code(url), "ABC@123");
    }
}
