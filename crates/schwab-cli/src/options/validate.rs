use anyhow::{bail, Result};
use schwab_api::TraderApi;
use serde_json::Value;

use crate::options::strategies::{iron_condor_max_loss, vertical_max_loss};
use crate::options::types::{IronCondorParams, StrategyKind, VerticalParams};
use crate::portfolio::{account_buying_power, ensure_sufficient_buying_power, BuyingPower};
use crate::rules::AccountType;

pub fn validate_account_for_strategy(account_type: AccountType, kind: StrategyKind) -> Result<()> {
    match kind {
        StrategyKind::Vertical | StrategyKind::IronCondor => {
            // Defined-risk strategies allowed on all account types in v1.
            let _ = account_type;
            Ok(())
        }
    }
}

pub async fn ensure_option_buying_power(
    api: &TraderApi,
    account_hash: &str,
    required_margin: f64,
) -> Result<BuyingPower> {
    let bp = account_buying_power(api, account_hash).await?;
    ensure_sufficient_buying_power(&bp, required_margin)?;
    Ok(bp)
}

pub fn estimate_vertical_margin(params: &VerticalParams) -> Result<f64> {
    vertical_max_loss(params)
}

pub fn estimate_iron_condor_margin(params: &IronCondorParams) -> f64 {
    iron_condor_max_loss(params)
}

pub fn estimate_order_margin(_order: &Value, kind: StrategyKind, params: &Value) -> Result<f64> {
    match kind {
        StrategyKind::Vertical => {
            let p: VerticalParams = serde_json::from_value(params.clone())?;
            estimate_vertical_margin(&p)
        }
        StrategyKind::IronCondor => {
            let p: IronCondorParams = serde_json::from_value(params.clone())?;
            Ok(estimate_iron_condor_margin(&p))
        }
    }
}

pub fn build_order_for_strategy(kind: StrategyKind, params: &Value) -> Result<Value> {
    match kind {
        StrategyKind::Vertical => {
            let p: VerticalParams = serde_json::from_value(params.clone())?;
            crate::options::strategies::build_vertical_order(&p)
        }
        StrategyKind::IronCondor => {
            let p: IronCondorParams = serde_json::from_value(params.clone())?;
            crate::options::strategies::build_iron_condor_order(&p)
        }
    }
}

pub fn params_from_value(kind: StrategyKind, params: &Value) -> Result<Value> {
    match kind {
        StrategyKind::Vertical => {
            let p: VerticalParams = serde_json::from_value(params.clone())?;
            if p.limit_credit.is_none() && p.limit_debit.is_none() {
                bail!("vertical params require limit_credit or limit_debit");
            }
            Ok(serde_json::to_value(p)?)
        }
        StrategyKind::IronCondor => {
            let p: IronCondorParams = serde_json::from_value(params.clone())?;
            Ok(serde_json::to_value(p)?)
        }
    }
}
