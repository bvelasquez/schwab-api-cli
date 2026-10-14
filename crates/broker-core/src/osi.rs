use anyhow::{bail, Context, Result};
use chrono::NaiveDate;

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedOsi {
    pub underlying: String,
    pub expiry: NaiveDate,
    pub put_call: char,
    pub strike: f64,
    pub raw: String,
}

/// OCC symbol: root padded to 6, YYMMDD, C/P, strike*1000 in 8 digits.
/// This is the symbol both Schwab and Public use for listed options.
pub fn build_osi_symbol(
    underlying: &str,
    expiry: &str,
    put_call: char,
    strike: f64,
) -> Result<String> {
    let root = underlying.trim().to_uppercase();
    if root.is_empty() {
        bail!("underlying symbol required");
    }
    let padded = format!("{root:<6}");
    let date = NaiveDate::parse_from_str(expiry.trim(), "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(expiry.trim(), "%y%m%d"))
        .with_context(|| format!("expiry `{expiry}` is not YYYY-MM-DD"))?;
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
    Ok(format!("{padded}{yymmdd}{pc}{strike_int:08}"))
}

pub fn parse_osi_symbol(symbol: &str) -> Result<ParsedOsi> {
    let compact: String = symbol.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() < 15 {
        bail!("invalid option symbol `{symbol}`");
    }
    let root_len = compact.len() - 15;
    let underlying = compact[..root_len].to_string();
    let rest = &compact[root_len..];
    let yymmdd = &rest[..6];
    let put_call = rest.chars().nth(6).context("missing put/call")?;
    let strike_raw: i64 = rest[7..15].parse().context("bad strike")?;
    let expiry = NaiveDate::parse_from_str(yymmdd, "%y%m%d")?;
    if put_call != 'C' && put_call != 'P' {
        bail!("invalid put/call in `{symbol}`");
    }
    Ok(ParsedOsi {
        underlying,
        expiry,
        put_call,
        strike: strike_raw as f64 / 1000.0,
        raw: symbol.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osi_round_trip_matches_schwab_padding() {
        let symbol = build_osi_symbol("SPY", "2026-08-15", 'P', 520.0).unwrap();
        assert_eq!(symbol, "SPY   260815P00520000");
        let parsed = parse_osi_symbol(&symbol).unwrap();
        assert_eq!(parsed.underlying, "SPY");
        assert_eq!(parsed.strike, 520.0);
        assert_eq!(parsed.put_call, 'P');
    }
}
