use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::order_builder::{
    build_equity_order, parse_duration, parse_session, parse_trade_order_type, TradeOrderType,
    TradeSide,
};
use crate::portfolio::account_equity;
use crate::safety::SafetyContext;

pub const PLAN_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradePlan {
    pub version: u32,
    pub plan_id: String,
    pub title: String,
    pub account_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(default)]
    pub assumptions: PlanAssumptions,
    #[serde(default)]
    pub execution: PlanExecution,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanAssumptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub limit_prices: std::collections::HashMap<String, f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PlanWaitUntil {
    #[default]
    Accepted,
    Filled,
    Terminal,
}

impl From<PlanWaitUntil> for crate::order_status::WaitCondition {
    fn from(value: PlanWaitUntil) -> Self {
        match value {
            PlanWaitUntil::Accepted => Self::Accepted,
            PlanWaitUntil::Filled => Self::Filled,
            PlanWaitUntil::Terminal => Self::Terminal,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanExecution {
    pub stop_on_error: bool,
    pub pause_seconds_between_steps: u64,
    /// Default wait policy for steps that do not set `wait_until`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_wait_until: Option<PlanWaitUntil>,
    /// Shorthand: set default_wait_until to filled when true.
    pub wait_for_fill: bool,
    pub fill_timeout_seconds: u64,
    pub poll_interval_seconds: u64,
    pub proceed_on_partial_fill: bool,
}

impl Default for PlanExecution {
    fn default() -> Self {
        Self {
            stop_on_error: true,
            pause_seconds_between_steps: 0,
            default_wait_until: None,
            wait_for_fill: false,
            fill_timeout_seconds: 3600,
            poll_interval_seconds: 5,
            proceed_on_partial_fill: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub side: PlanSide,
    pub symbol: String,
    pub quantity: f64,
    #[serde(default = "default_order_type")]
    pub order_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Wait for order status before advancing to the next plan step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_until: Option<PlanWaitUntil>,
}

fn default_order_type() -> String {
    "limit".into()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlanSide {
    Buy,
    Sell,
}

impl PlanSide {
    pub fn as_trade_side(self) -> TradeSide {
        match self {
            PlanSide::Buy => TradeSide::Buy,
            PlanSide::Sell => TradeSide::Sell,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StepValidation {
    pub id: String,
    pub side: PlanSide,
    pub symbol: String,
    pub quantity: f64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanValidationReport {
    pub plan_id: String,
    pub step_count: usize,
    pub all_ok: bool,
    pub steps: Vec<StepValidation>,
}

pub fn load_plan(path: &Path) -> Result<TradePlan> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read plan file {}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let plan: TradePlan = match ext.as_str() {
        "yaml" | "yml" => serde_yaml::from_str(&raw)
            .with_context(|| format!("Invalid YAML in {}", path.display()))?,
        "json" => serde_json::from_str(&raw)
            .with_context(|| format!("Invalid JSON in {}", path.display()))?,
        _ => serde_json::from_str(&raw)
            .or_else(|_| serde_yaml::from_str(&raw))
            .with_context(|| {
                format!(
                    "Could not parse {} as JSON or YAML (use .json, .yaml, or .yml)",
                    path.display()
                )
            })?,
    };

    plan.validate_structure()?;
    Ok(plan)
}

impl TradePlan {
    pub fn validate_structure(&self) -> Result<()> {
        anyhow::ensure!(
            self.version == PLAN_VERSION,
            "Unsupported plan version {} (expected {PLAN_VERSION})",
            self.version
        );
        anyhow::ensure!(!self.plan_id.trim().is_empty(), "plan_id is required");
        anyhow::ensure!(
            !self.account_hash.trim().is_empty(),
            "account_hash is required"
        );
        anyhow::ensure!(!self.steps.is_empty(), "plan must include at least one step");

        let mut ids = std::collections::HashSet::new();
        for step in &self.steps {
            anyhow::ensure!(!step.id.trim().is_empty(), "each step needs a non-empty id");
            anyhow::ensure!(
                ids.insert(step.id.clone()),
                "duplicate step id `{}`",
                step.id
            );
            anyhow::ensure!(step.quantity > 0.0, "step `{}` quantity must be positive", step.id);
            let ot = parse_trade_order_type(&step.order_type)?;
            if ot == TradeOrderType::Limit && step.limit_price.is_none() {
                anyhow::bail!("step `{}` is limit order but limit_price is missing", step.id);
            }
        }
        Ok(())
    }

    pub fn step_order_json(&self, step: &PlanStep) -> Result<Value> {
        let order_type = parse_trade_order_type(&step.order_type)?;
        build_equity_order(
            step.side.as_trade_side(),
            &step.symbol,
            step.quantity,
            order_type,
            step.limit_price,
            parse_duration(step.duration.as_deref())?,
            parse_session(step.session.as_deref())?,
        )
    }

    pub fn validate_against_safety(
        &self,
        safety: &SafetyContext,
        account_equity: Option<f64>,
    ) -> PlanValidationReport {
        let mut steps = Vec::new();
        let mut all_ok = true;

        for step in &self.steps {
            match self.step_order_json(step) {
                Ok(order) => match safety.validate_order(&order, None, account_equity) {
                    Ok(()) => steps.push(StepValidation {
                        id: step.id.clone(),
                        side: step.side,
                        symbol: step.symbol.clone(),
                        quantity: step.quantity,
                        ok: true,
                        error: None,
                    }),
                    Err(e) => {
                        all_ok = false;
                        steps.push(StepValidation {
                            id: step.id.clone(),
                            side: step.side,
                            symbol: step.symbol.clone(),
                            quantity: step.quantity,
                            ok: false,
                            error: Some(e.to_string()),
                        });
                    }
                },
                Err(e) => {
                    all_ok = false;
                    steps.push(StepValidation {
                        id: step.id.clone(),
                        side: step.side,
                        symbol: step.symbol.clone(),
                        quantity: step.quantity,
                        ok: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        PlanValidationReport {
            plan_id: self.plan_id.clone(),
            step_count: self.steps.len(),
            all_ok,
            steps,
        }
    }

    pub async fn validate_with_api(
        &self,
        safety: &SafetyContext,
        api: &schwab_api::TraderApi,
    ) -> Result<PlanValidationReport> {
        let equity = account_equity(api, &self.account_hash).await.ok().flatten();
        Ok(self.validate_against_safety(safety, equity))
    }

    pub fn step_wait_condition(&self, step: &PlanStep) -> crate::order_status::WaitCondition {
        if let Some(w) = step.wait_until {
            return w.into();
        }
        if self.execution.wait_for_fill {
            return PlanWaitUntil::Filled.into();
        }
        if let Some(w) = self.execution.default_wait_until {
            return w.into();
        }
        crate::order_status::WaitCondition::Accepted
    }

    pub fn wait_options_for_step(&self, step: &PlanStep) -> crate::order_status::WaitOptions {
        crate::order_status::WaitOptions {
            condition: self.step_wait_condition(step),
            timeout: std::time::Duration::from_secs(self.execution.fill_timeout_seconds),
            interval: std::time::Duration::from_secs(self.execution.poll_interval_seconds),
            proceed_on_partial_fill: self.execution.proceed_on_partial_fill,
            requested_quantity: Some(step.quantity),
        }
    }

    pub fn steps_filtered<'a>(
        &'a self,
        step_id: Option<&str>,
        from_step: Option<&str>,
    ) -> Result<Vec<&'a PlanStep>> {
        if let Some(id) = step_id {
            let step = self
                .steps
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(|| anyhow::anyhow!("Step `{id}` not found in plan"))?;
            return Ok(vec![step]);
        }

        if let Some(from) = from_step {
            let idx = self
                .steps
                .iter()
                .position(|s| s.id == from)
                .ok_or_else(|| anyhow::anyhow!("Step `{from}` not found in plan"))?;
            return Ok(self.steps[idx..].iter().collect());
        }

        Ok(self.steps.iter().collect())
    }
}

pub fn json_schema() -> Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Schwab Trade Plan",
        "description": "Machine-readable multi-step trade plan for schwab plan run",
        "type": "object",
        "required": ["version", "plan_id", "title", "account_hash", "created_at", "steps"],
        "properties": {
            "version": { "type": "integer", "const": PLAN_VERSION },
            "plan_id": { "type": "string", "pattern": "^[a-z0-9][a-z0-9._-]+$" },
            "title": { "type": "string" },
            "account_hash": { "type": "string", "minLength": 16 },
            "account_label": { "type": "string" },
            "created_at": { "type": "string", "format": "date-time" },
            "author": { "type": "string" },
            "rationale": { "type": "string" },
            "assumptions": {
                "type": "object",
                "properties": {
                    "notes": { "type": "string" },
                    "limit_prices": {
                        "type": "object",
                        "additionalProperties": { "type": "number" }
                    }
                }
            },
            "execution": {
                "type": "object",
                "properties": {
                    "stop_on_error": { "type": "boolean", "default": true },
                    "pause_seconds_between_steps": { "type": "integer", "minimum": 0, "default": 0 },
                    "default_wait_until": { "enum": ["accepted", "filled", "terminal"] },
                    "wait_for_fill": { "type": "boolean", "default": false },
                    "fill_timeout_seconds": { "type": "integer", "minimum": 1, "default": 3600 },
                    "poll_interval_seconds": { "type": "integer", "minimum": 1, "default": 5 },
                    "proceed_on_partial_fill": { "type": "boolean", "default": false }
                }
            },
            "steps": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "required": ["id", "side", "symbol", "quantity"],
                    "properties": {
                        "id": { "type": "string" },
                        "side": { "enum": ["buy", "sell"] },
                        "symbol": { "type": "string" },
                        "quantity": { "type": "number", "exclusiveMinimum": 0 },
                        "order_type": { "enum": ["market", "limit"], "default": "limit" },
                        "limit_price": { "type": "number" },
                        "duration": { "enum": ["day", "gtc", "fok"] },
                        "session": { "enum": ["normal", "am", "pm", "seamless"] },
                        "note": { "type": "string" },
                        "wait_until": { "enum": ["accepted", "filled", "terminal"] }
                    }
                }
            }
        }
    })
}

pub fn llm_prompt() -> Value {
    serde_json::json!({
        "purpose": "Generate a trade plan file for `schwab plan validate` and `schwab plan run`",
        "workflow": [
            "1. Run `schwab portfolio summary --json` and `schwab safety show --json`",
            "2. Run `schwab accounts numbers --json` to obtain account_hash values",
            "3. Split large trades into steps that respect safety.json (max_trade_value_usd, max_shares_per_order, max_trade_pct_of_equity)",
            "4. Prefer limit orders with explicit limit_price; use sells before buys when rotating cash",
            "5. Write plan as YAML or JSON matching `schwab plan schema --json`",
            "6. Validate with `schwab plan validate <file>` before `schwab plan run <file> --dry-run`",
            "7. For limit orders set execution.wait_for_fill or step wait_until: filled so plan run waits before the next step",
            "8. Live execution requires user-approved `schwab plan run <file> --trust --yes`"
        ],
        "rules": [
            "Never omit account_hash — use hashValue from accounts numbers, not plain account number",
            "Each step must have a unique id (e.g. step-01-sell-sgov)",
            "Use side: buy or sell (lowercase)",
            "Limit orders must include limit_price",
            "Keep step sizes under safety.json limits — the CLI validates every step",
            "Do not include options, short sales, or blocked symbols",
            "Include rationale explaining the rebalance thesis"
        ],
        "example_command_sequence": [
            "schwab plan schema --json",
            "schwab plan validate plans/my-plan.yaml",
            "schwab plan run plans/my-plan.yaml --dry-run --json",
            "schwab plan run plans/my-plan.yaml --trust --yes --json"
        ],
        "schema_command": "schwab plan schema --json",
        "template_yaml": PLAN_TEMPLATE_YAML,
    })
}

pub const PLAN_TEMPLATE_YAML: &str = r#"version: 1
plan_id: example-rebalance-2026-06-19
title: Example partial rebalance
account_hash: "<hash-from-schwab-accounts-numbers>"
account_label: "My brokerage (...1234)"
created_at: "2026-06-19T12:00:00Z"
author: "llm-agent"
rationale: |
  Brief thesis for the rebalance.
assumptions:
  notes: "Limit prices based on last close; adjust before live run"
  limit_prices:
    SGOV: 100.55
    JPST: 50.50
execution:
  stop_on_error: true
  pause_seconds_between_steps: 2
  wait_for_fill: true
  fill_timeout_seconds: 3600
  poll_interval_seconds: 10
steps:
  - id: step-01-sell-sgov
    side: sell
    symbol: SGOV
    quantity: 14
    order_type: limit
    limit_price: 100.55
    wait_until: filled
    note: "Batch 1 — stay under safety limits"
  - id: step-02-buy-jpst
    side: buy
    symbol: JPST
    quantity: 28
    order_type: limit
    limit_price: 50.50
    note: "Batch 1 buy"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_plan() -> TradePlan {
        TradePlan {
            version: PLAN_VERSION,
            plan_id: "test-plan".into(),
            title: "Test".into(),
            account_hash: "ABC123".into(),
            account_label: None,
            created_at: Utc::now(),
            author: None,
            rationale: None,
            assumptions: PlanAssumptions::default(),
            execution: PlanExecution::default(),
            steps: vec![PlanStep {
                id: "s1".into(),
                side: PlanSide::Sell,
                symbol: "SGOV".into(),
                quantity: 10.0,
                order_type: "limit".into(),
                limit_price: Some(100.0),
                duration: None,
                session: None,
                note: None,
                wait_until: None,
            }],
        }
    }

    #[test]
    fn validates_structure() {
        sample_plan().validate_structure().unwrap();
    }

    #[test]
    fn parses_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.yaml");
        let plan = sample_plan();
        fs::write(&path, serde_yaml::to_string(&plan).unwrap()).unwrap();
        let loaded = load_plan(&path).unwrap();
        assert_eq!(loaded.plan_id, "test-plan");
    }
}
