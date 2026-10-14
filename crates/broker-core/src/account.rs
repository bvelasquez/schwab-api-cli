use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Which brokerage backend owns an account or a data feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrokerKind {
    Schwab,
    Public,
}

impl BrokerKind {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "schwab" => Ok(Self::Schwab),
            "public" => Ok(Self::Public),
            other => bail!("unknown broker `{other}` (use schwab or public)"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Schwab => "schwab",
            Self::Public => "public",
        }
    }
}

impl std::fmt::Display for BrokerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    #[default]
    Margin,
    Cash,
    Ira,
}

/// `schwab:HASH` or `public:ACCOUNT_ID`. A bare id means Schwab, so existing plans keep working.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountRef {
    pub broker: BrokerKind,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub kind: AccountKind,
}

impl AccountRef {
    pub fn new(broker: BrokerKind, id: impl Into<String>) -> Self {
        Self {
            broker,
            id: id.into(),
            label: None,
            kind: AccountKind::Margin,
        }
    }

    pub fn address(&self) -> String {
        format!("{}:{}", self.broker.as_str(), self.id)
    }
}

/// Parse `broker:id`. A value with no known broker prefix is a Schwab hash.
pub fn parse_account_address(raw: &str) -> Result<AccountRef> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("account address is empty");
    }
    if let Some((prefix, rest)) = raw.split_once(':') {
        if matches!(prefix.to_ascii_lowercase().as_str(), "schwab" | "public") {
            if rest.is_empty() {
                bail!("account id missing after `{prefix}:`");
            }
            return Ok(AccountRef::new(BrokerKind::parse(prefix)?, rest));
        }
    }
    Ok(AccountRef::new(BrokerKind::Schwab, raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_hash_is_schwab() {
        let account = parse_account_address("ABC123HASH").unwrap();
        assert_eq!(account.broker, BrokerKind::Schwab);
        assert_eq!(account.id, "ABC123HASH");
    }

    #[test]
    fn prefixed_addresses() {
        let public = parse_account_address("public:5OA12345").unwrap();
        assert_eq!(public.broker, BrokerKind::Public);
        assert_eq!(public.id, "5OA12345");
        let schwab = parse_account_address("schwab:HASH").unwrap();
        assert_eq!(schwab.broker, BrokerKind::Schwab);
        assert_eq!(schwab.id, "HASH");
    }
}
