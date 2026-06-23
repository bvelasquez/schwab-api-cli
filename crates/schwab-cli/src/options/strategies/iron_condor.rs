use anyhow::{bail, Result};
use schwab_api::models::order::{
    ComplexOrderStrategyType, OrderInstruction, OrderTypeRequest,
};
use serde_json::Value;

use crate::order_builder::{build_complex_option_order, parse_duration, parse_session, OrderLegSpec};
use crate::options::symbology::build_option_symbol;
use crate::options::types::IronCondorParams;

pub fn build_iron_condor_order(params: &IronCondorParams) -> Result<Value> {
    if params.contracts <= 0.0 {
        bail!("contracts must be positive");
    }
    if params.put_short <= params.put_long {
        bail!("put_short must be > put_long");
    }
    if params.call_short >= params.call_long {
        bail!("call_short must be < call_long");
    }
    if params.limit_credit <= 0.0 {
        bail!("limit_credit must be positive");
    }

    let duration = parse_duration(params.duration.as_deref())?;
    let session = parse_session(params.session.as_deref())?;

    let legs = vec![
        OrderLegSpec {
            instruction: OrderInstruction::SellToOpen,
            symbol: build_option_symbol(&params.underlying, &params.expiry, 'P', params.put_short)?,
            asset_type: "OPTION",
            quantity: params.contracts,
        },
        OrderLegSpec {
            instruction: OrderInstruction::BuyToOpen,
            symbol: build_option_symbol(&params.underlying, &params.expiry, 'P', params.put_long)?,
            asset_type: "OPTION",
            quantity: params.contracts,
        },
        OrderLegSpec {
            instruction: OrderInstruction::SellToOpen,
            symbol: build_option_symbol(&params.underlying, &params.expiry, 'C', params.call_short)?,
            asset_type: "OPTION",
            quantity: params.contracts,
        },
        OrderLegSpec {
            instruction: OrderInstruction::BuyToOpen,
            symbol: build_option_symbol(&params.underlying, &params.expiry, 'C', params.call_long)?,
            asset_type: "OPTION",
            quantity: params.contracts,
        },
    ];

    build_complex_option_order(
        ComplexOrderStrategyType::IronCondor,
        OrderTypeRequest::NetCredit,
        legs,
        Some(params.limit_credit),
        duration,
        session,
        None,
    )
}

pub fn iron_condor_max_loss(params: &IronCondorParams) -> f64 {
    let put_width = params.put_short - params.put_long;
    let call_width = params.call_long - params.call_short;
    let max_width = put_width.max(call_width);
    (max_width - params.limit_credit).max(0.0) * 100.0 * params.contracts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_iron_condor() {
        let params = IronCondorParams {
            underlying: "SPY".into(),
            expiry: "2026-08-15".into(),
            put_short: 520.0,
            put_long: 515.0,
            call_short: 560.0,
            call_long: 565.0,
            contracts: 1.0,
            limit_credit: 1.20,
            duration: None,
            session: None,
        };
        let order = build_iron_condor_order(&params).unwrap();
        assert_eq!(order["complexOrderStrategyType"], "IRON_CONDOR");
        assert_eq!(order["orderLegCollection"].as_array().unwrap().len(), 4);
    }
}
