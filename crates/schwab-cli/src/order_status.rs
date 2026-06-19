use std::time::Duration;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Condition to wait for when polling order status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WaitCondition {
    /// Stop after broker accepts the order (HTTP 201); no polling.
    #[default]
    Accepted,
    /// Stop when status is `FILLED` (fails on reject/cancel/expire).
    Filled,
    /// Stop on any terminal status (`FILLED`, `CANCELED`, `REJECTED`, `EXPIRED`, …).
    Terminal,
}

impl WaitCondition {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "accepted" | "accept" | "submitted" => Ok(Self::Accepted),
            "filled" | "fill" => Ok(Self::Filled),
            "terminal" | "done" | "complete" => Ok(Self::Terminal),
            other => bail!("Unknown wait condition `{other}` (use accepted, filled, or terminal)"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Filled => "filled",
            Self::Terminal => "terminal",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderWaitResult {
    pub order_id: String,
    pub condition: WaitCondition,
    pub met: bool,
    pub final_status: Option<String>,
    pub polls: u32,
    pub elapsed_seconds: u64,
    pub order: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Parse order id from Schwab `Location` header after place/replace.
/// Example: `https://api.schwabapi.com/trader/v1/accounts/{hash}/orders/{orderId}`
pub fn parse_order_id_from_location(location: &str) -> Option<String> {
    let marker = "/orders/";
    let idx = location.rfind(marker)?;
    let id = location[idx + marker.len()..].trim();
    let id = id.split('?').next()?.trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

pub fn order_status(order: &Value) -> Option<String> {
    order
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.to_ascii_uppercase())
}

pub fn order_filled_quantity(order: &Value) -> Option<f64> {
    order
        .get("filledQuantity")
        .and_then(parse_number)
        .or_else(|| {
            order
                .get("orderActivityCollection")
                .and_then(|v| v.as_array())
                .and_then(|acts| acts.first())
                .and_then(|a| a.get("quantity"))
                .and_then(parse_number)
        })
}

fn parse_number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .or_else(|| v.as_i64().map(|n| n as f64))
}

pub fn is_terminal_status(status: &str) -> bool {
    matches!(
        status,
        "FILLED" | "CANCELED" | "CANCELLED" | "REJECTED" | "EXPIRED" | "REPLACED"
    )
}

pub fn is_failure_status(status: &str) -> bool {
    matches!(
        status,
        "CANCELED" | "CANCELLED" | "REJECTED" | "EXPIRED"
    )
}

pub fn condition_met(
    status: &str,
    condition: WaitCondition,
    filled_qty: Option<f64>,
    requested_qty: Option<f64>,
    proceed_on_partial_fill: bool,
) -> bool {
    match condition {
        WaitCondition::Accepted => true,
        WaitCondition::Terminal => is_terminal_status(status),
        WaitCondition::Filled => {
            if status == "FILLED" {
                return true;
            }
            if proceed_on_partial_fill {
                if let (Some(filled), Some(requested)) = (filled_qty, requested_qty) {
                    if filled >= requested {
                        return true;
                    }
                }
            }
            false
        }
    }
}

pub fn condition_satisfied_or_failed(
    status: &str,
    condition: WaitCondition,
) -> Option<Result<()>> {
    match condition {
        WaitCondition::Accepted => Some(Ok(())),
        WaitCondition::Terminal if is_terminal_status(status) => {
            if status == "FILLED" {
                Some(Ok(()))
            } else {
                Some(Err(anyhow::anyhow!(
                    "Order reached terminal status `{status}` before condition was satisfied"
                )))
            }
        }
        WaitCondition::Filled if is_failure_status(status) => Some(Err(anyhow::anyhow!(
            "Order failed with status `{status}` while waiting for fill"
        ))),
        _ => None,
    }
}

pub struct WaitOptions {
    pub condition: WaitCondition,
    pub timeout: Duration,
    pub interval: Duration,
    pub proceed_on_partial_fill: bool,
    pub requested_quantity: Option<f64>,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            condition: WaitCondition::Filled,
            timeout: Duration::from_secs(3600),
            interval: Duration::from_secs(5),
            proceed_on_partial_fill: false,
            requested_quantity: None,
        }
    }
}

#[allow(unused_assignments)]
pub async fn wait_for_order(
    api: &schwab_api::TraderApi,
    account_hash: &str,
    order_id: &str,
    opts: WaitOptions,
) -> Result<OrderWaitResult> {
    if opts.condition == WaitCondition::Accepted {
        return Ok(OrderWaitResult {
            order_id: order_id.to_string(),
            condition: opts.condition,
            met: true,
            final_status: None,
            polls: 0,
            elapsed_seconds: 0,
            order: Value::Null,
            error: None,
        });
    }

    let started = std::time::Instant::now();
    let mut polls = 0u32;
    let mut last_order = None::<Value>;
    let mut last_status = None::<String>;

    loop {
        polls += 1;
        let order = api.orders().get(account_hash, order_id).await?;
        last_status = order_status(&order);
        let filled_qty = order_filled_quantity(&order);
        last_order = Some(order);

        if let Some(status) = last_status.as_deref() {
            if condition_met(
                status,
                opts.condition,
                filled_qty,
                opts.requested_quantity,
                opts.proceed_on_partial_fill,
            ) {
                return Ok(OrderWaitResult {
                    order_id: order_id.to_string(),
                    condition: opts.condition,
                    met: true,
                    final_status: last_status,
                    polls,
                    elapsed_seconds: started.elapsed().as_secs(),
                    order: last_order.clone().unwrap_or(Value::Null),
                    error: None,
                });
            }

            if let Some(result) = condition_satisfied_or_failed(status, opts.condition) {
                result?;
            }
        }

        if started.elapsed() >= opts.timeout {
            return Ok(OrderWaitResult {
                order_id: order_id.to_string(),
                condition: opts.condition,
                met: false,
                final_status: last_status,
                polls,
                elapsed_seconds: started.elapsed().as_secs(),
                order: last_order.clone().unwrap_or(Value::Null),
                error: Some(format!(
                    "Timed out after {}s waiting for {:?}",
                    opts.timeout.as_secs(),
                    opts.condition
                )),
            });
        }

        tokio::time::sleep(opts.interval).await;
    }
}

pub fn wait_result_json(result: &OrderWaitResult) -> Value {
    json!({
        "order_id": result.order_id,
        "condition": result.condition,
        "met": result.met,
        "final_status": result.final_status,
        "polls": result.polls,
        "elapsed_seconds": result.elapsed_seconds,
        "error": result.error,
        "order": result.order,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_order_id_from_location() {
        let loc = "https://api.schwabapi.com/trader/v1/accounts/ABC/orders/1234567890";
        assert_eq!(
            parse_order_id_from_location(loc).as_deref(),
            Some("1234567890")
        );
    }

    #[test]
    fn filled_condition() {
        assert!(condition_met("FILLED", WaitCondition::Filled, None, Some(10.0), false));
        assert!(!condition_met("WORKING", WaitCondition::Filled, None, Some(10.0), false));
    }

    #[test]
    fn terminal_condition() {
        assert!(condition_met("CANCELED", WaitCondition::Terminal, None, None, false));
    }
}
