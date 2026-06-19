use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose, SanType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::debug;
use url::Url;

/// Bind localhost (HTTP or HTTPS) and wait for an OAuth redirect carrying `code=`.
///
/// Browsers often fail the first TLS handshake before the user accepts the self-signed
/// certificate, so we keep accepting connections until timeout.
pub async fn capture_redirect_code(redirect_uri: String, timeout: Duration) -> Result<Option<String>> {
    let (use_tls, port) = parse_local_redirect(&redirect_uri)?;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("Could not bind 127.0.0.1:{port} for OAuth callback"))?;

    let acceptor = if use_tls {
        crate::tls::install_crypto_provider();
        Some(TlsAcceptor::from(build_tls_config()?))
    } else {
        None
    };

    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }

        let (stream, _) = match tokio::time::timeout(remaining, listener.accept()).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => return Ok(None),
        };

        let code = if let Some(acceptor) = acceptor.clone() {
            match acceptor.accept(stream).await {
                Ok(tls) => handle_oauth_request(tls).await?,
                Err(err) => {
                    debug!(%err, "TLS handshake failed; waiting for browser retry after cert acceptance");
                    continue;
                }
            }
        } else {
            handle_oauth_request(stream).await?
        };

        if code.is_some() {
            return Ok(code);
        }
    }
}

async fn handle_oauth_request<S>(mut stream: S) -> Result<Option<String>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(None);
    }

    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    let code = Url::parse(&format!("https://127.0.0.1{path}"))
        .ok()
        .and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "code")
                .map(|(_, v)| v.to_string())
        });

    let body = "Schwab OAuth complete. You can close this tab and return to the terminal.";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;

    Ok(code)
}

fn build_tls_config() -> Result<Arc<ServerConfig>> {
    let key_pair = KeyPair::generate().context("Failed to generate TLS key pair")?;
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, "127.0.0.1");
    params.subject_alt_names = vec![
        SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        SanType::DnsName(
            "localhost"
                .try_into()
                .context("Invalid localhost DNS SAN")?,
        ),
    ];
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    let cert = params
        .self_signed(&key_pair)
        .context("Failed to sign localhost certificate")?;
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(key_pair.serialize_der().into());

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .context("Failed to build TLS server config")?;

    Ok(Arc::new(config))
}

fn parse_local_redirect(redirect_uri: &str) -> Result<(bool, u16)> {
    let url = Url::parse(redirect_uri).context("Invalid SCHWAB_REDIRECT_URI")?;
    let host = url.host_str().unwrap_or("");
    if host != "127.0.0.1" && host != "localhost" {
        anyhow::bail!("Auto-capture only supports localhost redirect URIs");
    }
    let use_tls = url.scheme() == "https";
    let port = url.port().unwrap_or(if use_tls { 443 } else { 80 });
    Ok((use_tls, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_local_redirect() {
        let (tls, port) = parse_local_redirect("https://127.0.0.1:8182").unwrap();
        assert!(tls);
        assert_eq!(port, 8182);
    }

    #[test]
    fn parses_http_local_redirect() {
        let (tls, port) = parse_local_redirect("http://127.0.0.1:8182/").unwrap();
        assert!(!tls);
        assert_eq!(port, 8182);
    }
}
