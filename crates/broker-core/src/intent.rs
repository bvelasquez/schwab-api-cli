use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::capabilities::Capabilities;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeInForce {
    Day,
    Gtc,
    Fok,
}

impl Default for TimeInForce {
    fn default() -> Self {
        Self::Day
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Session {
    Core,
    Extended,
    TwentyFourHours,
}

impl Default for Session {
    fn default() -> Self {
        Self::Core
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Equity,
    Option,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegInstruction {
    Buy,
    Sell,
    BuyToOpen,
    SellToOpen,
    BuyToClose,
    SellToClose,
}

impl LegInstruction {
    pub fn is_buy(self) -> bool {
        matches!(
            self,
            Self::Buy | Self::BuyToOpen | Self::BuyToClose
        )
    }

    pub fn is_short_sale(self) -> bool {
        matches!(self, Self::SellToOpen)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EquitySize {
    Shares(f64),
    Notional(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpreadKind {
    Vertical,
    IronCondor,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpreadLeg {
    pub instruction: LegInstruction,
    pub symbol: String,
    pub asset: AssetKind,
    /// Contracts or shares for this leg on one spread unit. Multiplied by `contracts`.
    pub ratio: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetPrice {
    Credit(f64),
    Debit(f64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExitBracket {
    pub symbol: String,
    pub quantity: f64,
    pub take_profit: f64,
    pub stop: f64,
    pub stop_limit: f64,
    #[serde(default)]
    pub tif: TimeInForce,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OrderIntent {
    Equity {
        side: Side,
        symbol: String,
        size: EquitySize,
        limit: Option<f64>,
        #[serde(default)]
        tif: TimeInForce,
        #[serde(default)]
        session: Session,
        /// Attach exits on the entry when the backend supports native brackets.
        #[serde(default)]
        bracket: Option<ExitBracket>,
    },
    Spread {
        strategy: SpreadKind,
        legs: Vec<SpreadLeg>,
        contracts: f64,
        net: NetPrice,
        #[serde(default)]
        tif: TimeInForce,
        #[serde(default)]
        session: Session,
    },
    /// Standalone OCO / bracket exit for a position already held.
    ExitBracket(ExitBracket),
}

impl OrderIntent {
    pub fn symbols(&self) -> Vec<String> {
        match self {
            Self::Equity { symbol, bracket, .. } => {
                let mut out = vec![symbol.clone()];
                if let Some(bracket) = bracket {
                    out.push(bracket.symbol.clone());
                }
                out
            }
            Self::Spread { legs, .. } => legs.iter().map(|leg| leg.symbol.clone()).collect(),
            Self::ExitBracket(bracket) => vec![bracket.symbol.clone()],
        }
    }

    pub fn estimated_notional(&self) -> f64 {
        match self {
            Self::Equity {
                size: EquitySize::Shares(qty),
                limit,
                ..
            } => qty.abs() * limit.unwrap_or(0.0),
            Self::Equity {
                size: EquitySize::Notional(amount),
                ..
            } => amount.abs(),
            Self::Spread {
                contracts, net, ..
            } => {
                let price = match net {
                    NetPrice::Credit(v) | NetPrice::Debit(v) => *v,
                };
                contracts.abs() * price * 100.0
            }
            Self::ExitBracket(bracket) => bracket.quantity.abs() * bracket.take_profit,
        }
    }

    pub fn max_quantity(&self) -> f64 {
        match self {
            Self::Equity {
                size: EquitySize::Shares(qty),
                ..
            } => qty.abs(),
            Self::Equity {
                size: EquitySize::Notional(_),
                ..
            } => 0.0,
            Self::Spread {
                legs, contracts, ..
            } => legs
                .iter()
                .map(|leg| (leg.ratio * contracts).abs())
                .fold(0.0, f64::max),
            Self::ExitBracket(bracket) => bracket.quantity.abs(),
        }
    }

    pub fn validate_for(&self, caps: &Capabilities) -> Result<()> {
        match self {
            Self::Equity {
                size: EquitySize::Notional(_),
                ..
            } if !caps.fractional => {
                bail!(
                    "{} does not accept notional equity orders",
                    caps.broker
                );
            }
            Self::Equity {
                size: EquitySize::Shares(qty),
                ..
            } if !caps.fractional && qty.fract() > 1e-9 => {
                bail!("{} does not accept fractional shares", caps.broker);
            }
            Self::Equity {
                session: Session::TwentyFourHours,
                ..
            } if caps.broker == "schwab" => {
                bail!("Schwab adapter does not submit 24-hour equity sessions");
            }
            Self::Equity { bracket: Some(_), .. } if !caps.native_bracket => {
                bail!(
                    "{} has no native bracket; place the exit as a separate ExitBracket after the fill",
                    caps.broker
                );
            }
            Self::Spread { legs, .. } if legs.len() > caps.max_legs as usize => {
                bail!(
                    "{} accepts at most {} legs",
                    caps.broker, caps.max_legs
                );
            }
            Self::Spread { legs, .. } if legs.len() < 2 => {
                bail!("spread orders need at least two legs");
            }
            _ => {}
        }
        Ok(())
    }
}
