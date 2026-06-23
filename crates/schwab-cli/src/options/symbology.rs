use anyhow::{bail, Context, Result};
use chrono::NaiveDate;

/// Parsed OCC-style option symbol (Schwab format).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParsedOptionSymbol {
    pub underlying: String,
    #[serde(with = "naive_date_format")]
    pub expiry: NaiveDate,
    pub put_call: char,
    pub strike: f64,
    pub raw: String,
}

mod naive_date_format {
    use chrono::NaiveDate;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(date: &NaiveDate, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&date.format("%Y-%m-%d").to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<NaiveDate, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        NaiveDate::parse_from_str(&s, "%Y-%m-%d").map_err(serde::de::Error::custom)
    }
}

/// Build Schwab OCC option symbol: root(6) + YYMMDD + C/P + strike*1000 (8 digits).
pub fn build_option_symbol(
    underlying: &str,
    expiry: &str,
    put_call: char,
    strike: f64,
) -> Result<String> {
    let root = underlying.trim().to_uppercase();
    if root.is_empty() {
        bail!("underlying symbol required");
    }
    let padded_root = format!("{root:<6}");

    let date = parse_expiry(expiry)?;
    let yymmdd = date.format("%y%m%d").to_string();

    let pc = match put_call.to_ascii_uppercase() {
        'C' => 'C',
        'P' => 'P',
        other => bail!("put_call must be C or P, got `{other}`"),
    };

    let strike_int = (strike * 1000.0).round() as i64;
    if strike_int <= 0 {
        bail!("strike must be positive");
    }
    let strike_str = format!("{strike_int:08}");

    Ok(format!("{padded_root}{yymmdd}{pc}{strike_str}"))
}

pub fn parse_option_symbol(symbol: &str) -> Result<ParsedOptionSymbol> {
    let s = symbol.trim();
    if s.len() < 15 {
        bail!("invalid option symbol length: `{s}`");
    }

    let root = s[..6].trim().to_string();
    let yymmdd = &s[6..12];
    let put_call = s
        .chars()
        .nth(12)
        .context("missing put/call indicator")?
        .to_ascii_uppercase();
    if put_call != 'C' && put_call != 'P' {
        bail!("invalid put/call in `{s}`");
    }
    let strike_raw = &s[13..21.min(s.len())];
    let strike_int: i64 = strike_raw
        .parse()
        .with_context(|| format!("invalid strike in `{s}`"))?;
    let strike = strike_int as f64 / 1000.0;

    let expiry = NaiveDate::parse_from_str(yymmdd, "%y%m%d")
        .with_context(|| format!("invalid expiry in `{s}`"))?;

    Ok(ParsedOptionSymbol {
        underlying: root,
        expiry,
        put_call,
        strike,
        raw: s.to_string(),
    })
}

pub fn parse_expiry(expiry: &str) -> Result<NaiveDate> {
    let trimmed = expiry.trim();
    if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        return Ok(d);
    }
    if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%Y%m%d") {
        return Ok(d);
    }
    bail!("expiry must be YYYY-MM-DD or YYYYMMDD, got `{trimmed}`")
}

pub fn days_to_expiry(expiry: NaiveDate, today: NaiveDate) -> i64 {
    (expiry - today).num_days()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_put_symbol() {
        let sym = build_option_symbol("SPY", "2026-07-18", 'P', 540.0).unwrap();
        assert_eq!(sym, "SPY   260718P00540000");
    }

    #[test]
    fn round_trip_symbol() {
        let sym = build_option_symbol("SPY", "2026-07-18", 'P', 540.0).unwrap();
        let parsed = parse_option_symbol(&sym).unwrap();
        assert_eq!(parsed.underlying, "SPY");
        assert_eq!(parsed.strike, 540.0);
        assert_eq!(parsed.put_call, 'P');
    }
}
