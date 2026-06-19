use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Order = Value;
pub type OrderRequest = Value;
pub type PreviewOrder = Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderInstruction {
    Buy,
    Sell,
    BuyToOpen,
    SellToClose,
    SellToOpen,
    BuyToClose,
    SellShort,
    BuyToCover,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderType {
    Market,
    Limit,
    Stop,
    StopLimit,
    TrailingStop,
}

/// Order types accepted on POST/PUT (no UNKNOWN).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderTypeRequest {
    Market,
    Limit,
    Stop,
    StopLimit,
    TrailingStop,
    Cabinet,
    NonMarketable,
    MarketOnClose,
    Exercise,
    TrailingStopLimit,
    NetDebit,
    NetCredit,
    NetZero,
    LimitOnClose,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderDuration {
    Day,
    GoodTillCancel,
    FillOrKill,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderSession {
    Normal,
    Am,
    Pm,
    Seamless,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderStrategyType {
    Single,
    Cancel,
    Recall,
    Pair,
    Flatten,
    TwoDaySwap,
    BlastAll,
    Oco,
    Trigger,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ComplexOrderStrategyType {
    None,
    Covered,
    Vertical,
    BackRatio,
    Calendar,
    Diagonal,
    Straddle,
    Strangle,
    CollarSynthetic,
    Butterfly,
    Condor,
    IronCondor,
    VerticalRoll,
    CollarWithStock,
    DoubleDiagonal,
    UnbalancedButterfly,
    UnbalancedCondor,
    UnbalancedIronCondor,
    UnbalancedVerticalRoll,
    MutualFundSwap,
    Custom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderLegType {
    Equity,
    Option,
    Index,
    MutualFund,
    CashEquivalent,
    FixedIncome,
    Currency,
    CollectiveInvestment,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AssetType {
    Equity,
    Option,
    Index,
    MutualFund,
    CashEquivalent,
    FixedIncome,
    Currency,
    CollectiveInvestment,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaxLotMethod {
    Fifo,
    Lifo,
    HighCost,
    LowCost,
    AverageCost,
    SpecificLot,
    LossHarvester,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SpecialInstruction {
    AllOrNone,
    DoNotReduce,
    AllOrNoneDoNotReduce,
}

impl ComplexOrderStrategyType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Covered => "COVERED",
            Self::Vertical => "VERTICAL",
            Self::BackRatio => "BACK_RATIO",
            Self::Calendar => "CALENDAR",
            Self::Diagonal => "DIAGONAL",
            Self::Straddle => "STRADDLE",
            Self::Strangle => "STRANGLE",
            Self::CollarSynthetic => "COLLAR_SYNTHETIC",
            Self::Butterfly => "BUTTERFLY",
            Self::Condor => "CONDOR",
            Self::IronCondor => "IRON_CONDOR",
            Self::VerticalRoll => "VERTICAL_ROLL",
            Self::CollarWithStock => "COLLAR_WITH_STOCK",
            Self::DoubleDiagonal => "DOUBLE_DIAGONAL",
            Self::UnbalancedButterfly => "UNBALANCED_BUTTERFLY",
            Self::UnbalancedCondor => "UNBALANCED_CONDOR",
            Self::UnbalancedIronCondor => "UNBALANCED_IRON_CONDOR",
            Self::UnbalancedVerticalRoll => "UNBALANCED_VERTICAL_ROLL",
            Self::MutualFundSwap => "MUTUAL_FUND_SWAP",
            Self::Custom => "CUSTOM",
        }
    }

    pub fn all_values() -> &'static [Self] {
        &[
            Self::None,
            Self::Covered,
            Self::Vertical,
            Self::BackRatio,
            Self::Calendar,
            Self::Diagonal,
            Self::Straddle,
            Self::Strangle,
            Self::CollarSynthetic,
            Self::Butterfly,
            Self::Condor,
            Self::IronCondor,
            Self::VerticalRoll,
            Self::CollarWithStock,
            Self::DoubleDiagonal,
            Self::UnbalancedButterfly,
            Self::UnbalancedCondor,
            Self::UnbalancedIronCondor,
            Self::UnbalancedVerticalRoll,
            Self::MutualFundSwap,
            Self::Custom,
        ]
    }
}

impl OrderTypeRequest {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Market => "MARKET",
            Self::Limit => "LIMIT",
            Self::Stop => "STOP",
            Self::StopLimit => "STOP_LIMIT",
            Self::TrailingStop => "TRAILING_STOP",
            Self::Cabinet => "CABINET",
            Self::NonMarketable => "NON_MARKETABLE",
            Self::MarketOnClose => "MARKET_ON_CLOSE",
            Self::Exercise => "EXERCISE",
            Self::TrailingStopLimit => "TRAILING_STOP_LIMIT",
            Self::NetDebit => "NET_DEBIT",
            Self::NetCredit => "NET_CREDIT",
            Self::NetZero => "NET_ZERO",
            Self::LimitOnClose => "LIMIT_ON_CLOSE",
        }
    }
}
