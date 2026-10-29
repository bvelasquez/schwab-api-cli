//! Fail-closed startup self-test (plan §4, L4).
//!
//! The report is written to the journal as `paper_gate_ok` only when every
//! check passes. A failed check means the process must not start.

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::config::ArmConfig;

#[derive(Debug, Clone)]
pub struct GateInputs<'a> {
    pub config: &'a ArmConfig,
    /// Broker names present in the registry, e.g. `["public"]`.
    pub registry_brokers: Vec<String>,
    /// True when the Public broker is wrapped in `PaperOnlyBroker`.
    pub public_paper_wrapped: bool,
    /// True when the Public HTTP client refuses trading writes.
    pub public_read_only: bool,
    /// True when `PUBLIC_PERSONAL_SECRET` is set in the environment.
    pub personal_secret_set: bool,
    /// Optional hash of the running binary, recorded for audit.
    pub binary_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateReport {
    pub ok: bool,
    pub arm_id: String,
    pub generated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_hash: Option<String>,
    pub checks: Vec<GateCheck>,
}

impl GateReport {
    pub fn failures(&self) -> Vec<&GateCheck> {
        self.checks.iter().filter(|check| !check.passed).collect()
    }
}

pub fn run_self_test(input: &GateInputs<'_>) -> GateReport {
    let mut checks = Vec::new();
    let mut push = |name: &str, passed: bool, detail: String| {
        checks.push(GateCheck {
            name: name.into(),
            passed,
            detail,
        });
    };

    let brokers_lower: Vec<String> = input
        .registry_brokers
        .iter()
        .map(|b| b.to_ascii_lowercase())
        .collect();
    let has_schwab = brokers_lower.iter().any(|b| b == "schwab");
    push(
        "no_schwab_backend",
        !has_schwab,
        format!("registry brokers = {:?}", input.registry_brokers),
    );

    push(
        "public_paper_wrapped",
        input.public_paper_wrapped,
        "Public broker must be wrapped in PaperOnlyBroker".into(),
    );

    push(
        "public_read_only",
        input.public_read_only,
        "Public HTTP client must refuse trading writes".into(),
    );

    let account_ok = match input.config.account_address() {
        Ok(address) => address.to_ascii_lowercase().starts_with("public:"),
        Err(_) => false,
    };
    push(
        "account_public",
        account_ok,
        "account must be `public:<id>` (bare hashes map to Schwab)".into(),
    );

    push(
        "mode_paper",
        input.config.mode.eq_ignore_ascii_case("paper"),
        format!("mode = `{}`", input.config.mode),
    );

    push(
        "live_auto_apply_false",
        !input.config.adaptation.live_auto_apply,
        "adaptation.live_auto_apply must be false".into(),
    );

    push(
        "personal_secret_unset",
        !input.personal_secret_set,
        "PUBLIC_PERSONAL_SECRET must be unset for the research arm".into(),
    );

    let config_ok = input.config.validate().is_ok();
    push(
        "arm_config_valid",
        config_ok,
        input
            .config
            .validate()
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "arm config valid".into()),
    );

    let ok = checks.iter().all(|check| check.passed);
    GateReport {
        ok,
        arm_id: input.config.arm_id.clone(),
        generated_at: Utc::now().to_rfc3339(),
        binary_hash: input.binary_hash.clone(),
        checks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ArmConfig {
        ArmConfig {
            arm_id: "research-megacaps".into(),
            account: Some("public:ACC".into()),
            universe: vec!["MSFT".into()],
            ..Default::default()
        }
    }

    fn clean(config: &ArmConfig) -> GateInputs<'_> {
        GateInputs {
            config,
            registry_brokers: vec!["public".into()],
            public_paper_wrapped: true,
            public_read_only: true,
            personal_secret_set: false,
            binary_hash: Some("abc".into()),
        }
    }

    #[test]
    fn clean_inputs_pass() {
        let config = config();
        let report = run_self_test(&clean(&config));
        assert!(report.ok, "failures: {:?}", report.failures());
    }

    #[test]
    fn schwab_backend_fails() {
        let config = config();
        let mut input = clean(&config);
        input.registry_brokers = vec!["public".into(), "schwab".into()];
        let report = run_self_test(&input);
        assert!(!report.ok);
        assert!(report
            .failures()
            .iter()
            .any(|check| check.name == "no_schwab_backend"));
    }

    #[test]
    fn personal_secret_fails() {
        let config = config();
        let mut input = clean(&config);
        input.personal_secret_set = true;
        let report = run_self_test(&input);
        assert!(!report.ok);
    }

    #[test]
    fn unwrapped_public_fails() {
        let config = config();
        let mut input = clean(&config);
        input.public_paper_wrapped = false;
        let report = run_self_test(&input);
        assert!(!report.ok);
    }

    #[test]
    fn live_auto_apply_fails() {
        let mut config = config();
        config.adaptation.live_auto_apply = true;
        let report = run_self_test(&clean(&config));
        assert!(!report.ok);
    }
}
