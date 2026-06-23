use anyhow::{bail, Context, Result};
use schwab_api::models::order::{
    ComplexOrderStrategyType, OrderInstruction, OrderTypeRequest,
};
use serde_json::Value;

use crate::order_builder::{build_complex_option_order, parse_duration, parse_session, OrderLegSpec};
use crate::options::symbology::build_option_symbol;
use crate::options::types::VerticalParams;

pub fn build_vertical_order(params: &VerticalParams) -> Result<Value> {
    if params.contracts <= 0.0 {
        bail!("contracts must be positive");
    }

    let spread = params.spread_type.to_ascii_lowercase();
    let (put_call, order_type, short_inst, long_inst, price) = match spread.as_str() {
        "put_credit" => {
            if params.short_strike <= params.long_strike {
                bail!("put_credit: short_strike must be > long_strike");
            }
            let credit = params
                .limit_credit
                .context("limit_credit required for put_credit")?;
            (
                'P',
                OrderTypeRequest::NetCredit,
                OrderInstruction::SellToOpen,
                OrderInstruction::BuyToOpen,
                credit,
            )
        }
        "put_debit" => {
            if params.short_strike >= params.long_strike {
                bail!("put_debit: short_strike must be < long_strike");
            }
            let debit = params
                .limit_debit
                .context("limit_debit required for put_debit")?;
            (
                'P',
                OrderTypeRequest::NetDebit,
                OrderInstruction::BuyToOpen,
                OrderInstruction::SellToOpen,
                debit,
            )
        }
        "call_credit" => {
            if params.short_strike >= params.long_strike {
                bail!("call_credit: short_strike must be < long_strike");
            }
            let credit = params
                .limit_credit
                .context("limit_credit required for call_credit")?;
            (
                'C',
                OrderTypeRequest::NetCredit,
                OrderInstruction::SellToOpen,
                OrderInstruction::BuyToOpen,
                credit,
            )
        }
        "call_debit" => {
            if params.short_strike <= params.long_strike {
                bail!("call_debit: short_strike must be > long_strike");
            }
            let debit = params
                .limit_debit
                .context("limit_debit required for call_debit")?;
            (
                'C',
                OrderTypeRequest::NetDebit,
                OrderInstruction::BuyToOpen,
                OrderInstruction::SellToOpen,
                debit,
            )
        }
        other => bail!(
            "unknown vertical type `{other}` (use put_credit, put_debit, call_credit, call_debit)"
        ),
    };

    let short_sym = build_option_symbol(
        &params.underlying,
        &params.expiry,
        put_call,
        params.short_strike,
    )?;
    let long_sym = build_option_symbol(
        &params.underlying,
        &params.expiry,
        put_call,
        params.long_strike,
    )?;

    let duration = parse_duration(params.duration.as_deref())?;
    let session = parse_session(params.session.as_deref())?;

    build_complex_option_order(
        ComplexOrderStrategyType::Vertical,
        order_type,
        vec![
            OrderLegSpec {
                instruction: short_inst,
                symbol: short_sym,
                asset_type: "OPTION",
                quantity: params.contracts,
            },
            OrderLegSpec {
                instruction: long_inst,
                symbol: long_sym,
                asset_type: "OPTION",
                quantity: params.contracts,
            },
        ],
        Some(price),
        duration,
        session,
        None,
    )
}

pub fn vertical_max_loss(params: &VerticalParams) -> Result<f64> {
    let width = (params.short_strike - params.long_strike).abs();
    let spread = params.spread_type.to_ascii_lowercase();
    if spread.ends_with("_credit") {
        let credit = params.limit_credit.unwrap_or(0.0);
        Ok((width - credit).max(0.0) * 100.0 * params.contracts)
    } else {
        let debit = params.limit_debit.unwrap_or(0.0);
        Ok(debit * 100.0 * params.contracts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_put_credit_vertical() {
        let params = VerticalParams {
            underlying: "SPY".into(),
            expiry: "2026-07-18".into(),
            spread_type: "put_credit".into(),
            short_strike: 540.0,
            long_strike: 535.0,
            contracts: 2.0,
            limit_credit: Some(0.85),
            limit_debit: None,
            duration: None,
            session: None,
        };
        let order = build_vertical_order(&params).unwrap();
        assert_eq!(order["complexOrderStrategyType"], "VERTICAL");
        assert_eq!(order["orderType"], "NET_CREDIT");
        assert_eq!(order["orderLegCollection"].as_array().unwrap().len(), 2);
    }
}
