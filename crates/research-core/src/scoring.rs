//! Forecast → outcome scoring (plan §8 stage B).
//!
//! The arm journals a forecast each tick. This module answers the only question
//! that matters for the experiment: when the horizon has elapsed, was the
//! forecast's ranking right about *excess* return versus the benchmark?
//!
//! Primary metric: cross-sectional Spearman rank IC between
//! `expected_return_pct` and the realized forward excess return, one observation
//! day per scored forecast. The promotion criteria mirror
//! `rules/analysis/llm-ic-eval.json`: mean IC >= 0.03, day-clustered t >= 2,
//! >= 20 scored days.
//!
//! Flat calls use a small dead band (`FLAT_BAND_PCT`) so "flat" is not scored as
//! a directional hit by default.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use broker_core::Bar;

use crate::forecast::Forecast;
use crate::indicators;

/// |excess return| at or below this counts as "flat" being right.
pub const FLAT_BAND_PCT: f64 = 0.5;
pub const MIN_DAYS: usize = 20;
pub const MIN_MEAN_IC: f64 = 0.03;
pub const MIN_T: f64 = 2.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingSignal {
    pub symbol: String,
    pub direction: String,
    pub expected_return_pct: f64,
    pub confidence: f64,
    pub rank: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingForecast {
    pub arm_id: String,
    pub as_of: String,
    /// Decision date (UTC). The base close is the first bar on/after this date.
    pub as_of_date: String,
    pub horizon_days: u32,
    pub benchmark: String,
    pub signals: Vec<PendingSignal>,
}

impl PendingForecast {
    pub fn from_forecast(forecast: &Forecast, as_of_date: NaiveDate) -> Self {
        Self {
            arm_id: forecast.arm_id.clone(),
            as_of: forecast.as_of.clone(),
            as_of_date: as_of_date.to_string(),
            horizon_days: forecast.horizon_days.max(1),
            benchmark: if forecast.benchmark.is_empty() {
                "SPY".into()
            } else {
                forecast.benchmark.clone()
            },
            signals: forecast
                .signals
                .iter()
                .map(|signal| PendingSignal {
                    symbol: signal.symbol.to_ascii_uppercase(),
                    direction: signal.direction.to_ascii_lowercase(),
                    expected_return_pct: signal.expected_return_pct,
                    confidence: signal.confidence,
                    rank: signal.rank,
                })
                .collect(),
        }
    }

    pub fn decision_date(&self) -> Option<NaiveDate> {
        self.as_of_date.parse().ok()
    }

    /// True once every needed series has a close `horizon_days` trading days
    /// after the base bar. The benchmark alone is enough to decide readiness.
    pub fn is_ready(&self, bars: &BTreeMap<String, Vec<Bar>>) -> bool {
        let Some(benchmark) = bars.get(&self.benchmark.to_ascii_uppercase()) else {
            return false;
        };
        let Some(base) = base_index(benchmark, self.decision_date()) else {
            return false;
        };
        indicators::sorted(benchmark).len() > base + self.horizon_days as usize
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignalScore {
    pub symbol: String,
    pub direction: String,
    pub expected_return_pct: f64,
    pub confidence: f64,
    pub realized_return_pct: f64,
    pub excess_return_pct: f64,
    pub correct: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForecastScore {
    pub arm_id: String,
    pub as_of: String,
    pub as_of_date: String,
    /// Date of the bar the forward window started from.
    pub base_date: String,
    pub scored_at: String,
    pub horizon_days: u32,
    pub benchmark: String,
    pub benchmark_return_pct: f64,
    pub signals: Vec<SignalScore>,
    pub hit_rate_pct: f64,
    pub mean_excess_pct: f64,
    pub rank_ic: Option<f64>,
    #[serde(default)]
    pub missing_symbols: Vec<String>,
}

/// First bar at or after `date`.
fn base_index(bars: &[Bar], date: Option<NaiveDate>) -> Option<usize> {
    let sorted = indicators::sorted(bars);
    let date = date?;
    sorted
        .iter()
        .position(|bar| indicators::parse_bar_date(&bar.timestamp).is_some_and(|d| d >= date))
}

fn close_at(bars: &[Bar], index: usize) -> Option<f64> {
    indicators::sorted(bars)
        .get(index)
        .map(|bar| bar.close)
        .filter(|close| *close > 0.0)
}

fn date_at(bars: &[Bar], index: usize) -> Option<String> {
    indicators::sorted(bars)
        .get(index)
        .and_then(|bar| indicators::parse_bar_date(&bar.timestamp))
        .map(|date| date.to_string())
}

fn pct_return(base: f64, end: f64) -> f64 {
    if base <= 0.0 {
        0.0
    } else {
        (end / base - 1.0) * 100.0
    }
}

fn direction_correct(direction: &str, excess_pct: f64) -> bool {
    match direction {
        "up" => excess_pct > 0.0,
        "down" => excess_pct < 0.0,
        "flat" => excess_pct.abs() <= FLAT_BAND_PCT,
        _ => false,
    }
}

/// Score one pending forecast. `None` when the forward window is not complete
/// or when nothing scorable is available. Never partially fabricates data: a
/// symbol with missing bars is reported in `missing_symbols`.
pub fn score(
    pending: &PendingForecast,
    bars: &BTreeMap<String, Vec<Bar>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<ForecastScore> {
    let benchmark_symbol = pending.benchmark.to_ascii_uppercase();
    let benchmark_bars = bars.get(&benchmark_symbol)?;
    let base = base_index(benchmark_bars, pending.decision_date())?;
    let end = base + pending.horizon_days as usize;
    let benchmark_return_pct = pct_return(
        close_at(benchmark_bars, base)?,
        close_at(benchmark_bars, end)?,
    );

    let mut signals = Vec::new();
    let mut missing = Vec::new();
    for signal in &pending.signals {
        let Some(symbol_bars) = bars.get(&signal.symbol) else {
            missing.push(signal.symbol.clone());
            continue;
        };
        let Some(symbol_base) = base_index(symbol_bars, pending.decision_date()) else {
            missing.push(signal.symbol.clone());
            continue;
        };
        if symbol_base + pending.horizon_days as usize >= indicators::sorted(symbol_bars).len() {
            missing.push(signal.symbol.clone());
            continue;
        }
        let Some(start) = close_at(symbol_bars, symbol_base) else {
            missing.push(signal.symbol.clone());
            continue;
        };
        let Some(finish) = close_at(symbol_bars, symbol_base + pending.horizon_days as usize)
        else {
            missing.push(signal.symbol.clone());
            continue;
        };
        let realized = pct_return(start, finish);
        let excess = realized - benchmark_return_pct;
        signals.push(SignalScore {
            symbol: signal.symbol.clone(),
            direction: signal.direction.clone(),
            expected_return_pct: signal.expected_return_pct,
            confidence: signal.confidence,
            realized_return_pct: realized,
            excess_return_pct: excess,
            correct: direction_correct(&signal.direction, excess),
        });
    }
    if signals.is_empty() {
        return None;
    }
    let correct = signals.iter().filter(|signal| signal.correct).count();
    let hit_rate_pct = correct as f64 * 100.0 / signals.len() as f64;
    let mean_excess_pct = signals
        .iter()
        .map(|signal| signal.excess_return_pct)
        .sum::<f64>()
        / signals.len() as f64;
    let rank_ic = spearman(
        &signals
            .iter()
            .map(|signal| signal.expected_return_pct)
            .collect::<Vec<_>>(),
        &signals
            .iter()
            .map(|signal| signal.excess_return_pct)
            .collect::<Vec<_>>(),
    );
    Some(ForecastScore {
        arm_id: pending.arm_id.clone(),
        as_of: pending.as_of.clone(),
        as_of_date: pending.as_of_date.clone(),
        base_date: date_at(benchmark_bars, base).unwrap_or_else(|| pending.as_of_date.clone()),
        scored_at: now.to_rfc3339(),
        horizon_days: pending.horizon_days,
        benchmark: benchmark_symbol,
        benchmark_return_pct,
        signals,
        hit_rate_pct,
        mean_excess_pct,
        rank_ic,
        missing_symbols: missing,
    })
}

/// Average ranks, 1-based, ties share the mean rank.
fn ranks(values: &[f64]) -> Vec<f64> {
    let mut ordered: Vec<(usize, f64)> = values.iter().copied().enumerate().collect();
    ordered.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut ranks = vec![0.0; values.len()];
    let mut index = 0;
    while index < ordered.len() {
        let mut end = index;
        while end + 1 < ordered.len() && ordered[end + 1].1 == ordered[index].1 {
            end += 1;
        }
        let average = (index + end) as f64 / 2.0 + 1.0;
        for slot in index..=end {
            ranks[ordered[slot].0] = average;
        }
        index = end + 1;
    }
    ranks
}

/// Spearman rank correlation. `None` when n < 3 or either side is constant.
pub fn spearman(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() || a.len() < 3 {
        return None;
    }
    let ra = ranks(a);
    let rb = ranks(b);
    let n = ra.len() as f64;
    let mean = (n + 1.0) / 2.0;
    let mut cov = 0.0;
    let mut va = 0.0;
    let mut vb = 0.0;
    for (x, y) in ra.iter().zip(rb.iter()) {
        cov += (x - mean) * (y - mean);
        va += (x - mean).powi(2);
        vb += (y - mean).powi(2);
    }
    if va <= f64::EPSILON || vb <= f64::EPSILON {
        return None;
    }
    Some((cov / (va.sqrt() * vb.sqrt())).clamp(-1.0, 1.0))
}

/// Aggregate of every scored forecast for one arm.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Scorecard {
    pub scored_days: usize,
    /// Days whose forecast produced a defined rank IC. This, not `scored_days`,
    /// is what the stage-B gate counts: a day where the model marks everything
    /// `flat` has no cross-sectional information to measure.
    pub ic_days: usize,
    pub signals_scored: usize,
    pub hit_rate_pct: f64,
    pub mean_excess_pct: f64,
    pub mean_rank_ic: Option<f64>,
    pub ic_t_stat: Option<f64>,
    pub gate_pass: bool,
    pub gate_reason: String,
    pub last_scored_date: Option<String>,
}

impl Scorecard {
    pub fn from_scores(scores: &[ForecastScore]) -> Self {
        if scores.is_empty() {
            return Self {
                gate_reason: format!("no scored forecasts yet; {MIN_DAYS} days needed"),
                ..Default::default()
            };
        }
        let signals: Vec<&SignalScore> = scores
            .iter()
            .flat_map(|score| score.signals.iter())
            .collect();
        let correct = signals.iter().filter(|signal| signal.correct).count();
        let hit_rate_pct = if signals.is_empty() {
            0.0
        } else {
            correct as f64 * 100.0 / signals.len() as f64
        };
        let mean_excess_pct = if signals.is_empty() {
            0.0
        } else {
            signals
                .iter()
                .map(|signal| signal.excess_return_pct)
                .sum::<f64>()
                / signals.len() as f64
        };
        let ics: Vec<f64> = scores.iter().filter_map(|score| score.rank_ic).collect();
        let mean_rank_ic = if ics.is_empty() {
            None
        } else {
            Some(ics.iter().sum::<f64>() / ics.len() as f64)
        };
        let ic_t_stat = t_stat(&ics);
        let last_scored_date = scores.iter().map(|score| score.as_of_date.clone()).max();
        let mut scorecard = Self {
            scored_days: scores.len(),
            ic_days: ics.len(),
            signals_scored: signals.len(),
            hit_rate_pct,
            mean_excess_pct,
            mean_rank_ic,
            ic_t_stat,
            gate_pass: false,
            gate_reason: String::new(),
            last_scored_date,
        };
        scorecard.gate_reason = scorecard.gate_reason_text();
        scorecard.gate_pass = scorecard.passes_gate();
        scorecard
    }

    fn passes_gate(&self) -> bool {
        self.ic_days >= MIN_DAYS
            && self.mean_rank_ic.is_some_and(|ic| ic >= MIN_MEAN_IC)
            && self.ic_t_stat.is_some_and(|t| t >= MIN_T)
    }

    fn gate_reason_text(&self) -> String {
        if self.ic_days < MIN_DAYS {
            return format!(
                "{} of {MIN_DAYS} days with a defined IC ({} scored days; mean IC {}, t {})",
                self.ic_days,
                self.scored_days,
                self.mean_rank_ic
                    .map(|ic| format!("{ic:.3}"))
                    .unwrap_or_else(|| "—".into()),
                self.ic_t_stat
                    .map(|t| format!("{t:.2}"))
                    .unwrap_or_else(|| "—".into()),
            );
        }
        if !self.mean_rank_ic.is_some_and(|ic| ic >= MIN_MEAN_IC) {
            return format!(
                "mean IC {} is below {MIN_MEAN_IC}",
                self.mean_rank_ic
                    .map(|ic| format!("{ic:.3}"))
                    .unwrap_or_else(|| "—".into())
            );
        }
        if !self.ic_t_stat.is_some_and(|t| t >= MIN_T) {
            return format!(
                "IC t {} is below {MIN_T}",
                self.ic_t_stat
                    .map(|t| format!("{t:.2}"))
                    .unwrap_or_else(|| "—".into())
            );
        }
        "stage B signal criteria met on paper".into()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "scored_days": self.scored_days,
            "ic_days": self.ic_days,
            "signals_scored": self.signals_scored,
            "hit_rate_pct": self.hit_rate_pct,
            "mean_excess_pct": self.mean_excess_pct,
            "mean_rank_ic": self.mean_rank_ic,
            "ic_t_stat": self.ic_t_stat,
            "gate_pass": self.gate_pass,
            "gate_reason": self.gate_reason,
            "last_scored_date": self.last_scored_date,
            "criteria": {
                "min_days": MIN_DAYS,
                "mean_daily_ic": MIN_MEAN_IC,
                "day_clustered_t": MIN_T,
            },
        })
    }
}

/// Day-clustered t-stat of the mean: mean / (stdev / sqrt(n)).
fn t_stat(values: &[f64]) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let variance = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (n - 1.0);
    let stdev = variance.sqrt();
    if stdev <= f64::EPSILON {
        return None;
    }
    Some(mean / (stdev / n.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone};

    fn bar(date: &str, close: f64) -> Bar {
        Bar {
            timestamp: date.to_string(),
            open: close,
            high: close,
            low: close,
            close,
            volume: 1_000.0,
        }
    }

    /// Business-day closes starting 2026-10-01 (Thu).
    fn series(closes: &[f64]) -> Vec<Bar> {
        let start = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let mut bars = Vec::new();
        let mut date = start;
        for close in closes {
            while matches!(date.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                date = date.succ_opt().unwrap();
            }
            bars.push(bar(&date.to_string(), *close));
            date = date.succ_opt().unwrap();
        }
        bars
    }

    fn pending(signals: Vec<(&str, &str, f64)>) -> PendingForecast {
        PendingForecast {
            arm_id: "research-test".into(),
            as_of: "2026-10-01T13:30:00Z".into(),
            as_of_date: "2026-10-01".into(),
            horizon_days: 3,
            benchmark: "SPY".into(),
            signals: signals
                .into_iter()
                .map(|(symbol, direction, expected)| PendingSignal {
                    symbol: symbol.into(),
                    direction: direction.into(),
                    expected_return_pct: expected,
                    confidence: 0.7,
                    rank: 1,
                })
                .collect(),
        }
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc
            .with_ymd_and_hms(2026, 10, 20, 20, 0, 0)
            .unwrap()
    }

    #[test]
    fn scores_direction_and_excess_return() {
        let mut bars = BTreeMap::new();
        bars.insert(
            "SPY".to_string(),
            series(&[100.0, 100.0, 101.0, 102.0, 103.0]),
        );
        bars.insert(
            "MSFT".to_string(),
            series(&[100.0, 100.0, 103.0, 106.0, 109.0]),
        );
        let forecast = pending(vec![("MSFT", "up", 1.0)]);
        let score = score(&forecast, &bars, now()).unwrap();
        // Base bar is 2026-10-01 = 100.0; 3 trading days later = 106.0.
        assert_eq!(score.base_date, "2026-10-01");
        assert!((score.benchmark_return_pct - 2.0).abs() < 1e-9);
        assert!((score.signals[0].realized_return_pct - 6.0).abs() < 1e-9);
        assert!((score.signals[0].excess_return_pct - 4.0).abs() < 1e-9);
        assert!(score.signals[0].correct);
        assert_eq!(score.hit_rate_pct, 100.0);
    }

    #[test]
    fn not_ready_until_the_horizon_bars_exist() {
        let mut bars = BTreeMap::new();
        bars.insert("SPY".to_string(), series(&[100.0, 100.0, 101.0]));
        bars.insert("MSFT".to_string(), series(&[100.0, 100.0, 101.0]));
        let forecast = pending(vec![("MSFT", "up", 1.0)]);
        assert!(!forecast.is_ready(&bars));
        assert!(score(&forecast, &bars, now()).is_none());
    }

    #[test]
    fn down_call_scores_on_negative_excess() {
        let mut bars = BTreeMap::new();
        bars.insert(
            "SPY".to_string(),
            series(&[100.0, 100.0, 101.0, 102.0, 103.0]),
        );
        bars.insert(
            "MSFT".to_string(),
            series(&[100.0, 100.0, 99.0, 98.0, 97.0]),
        );
        let score = score(&pending(vec![("MSFT", "down", -1.0)]), &bars, now()).unwrap();
        assert!(score.signals[0].correct);
        assert_eq!(score.hit_rate_pct, 100.0);
    }

    #[test]
    fn flat_call_uses_the_dead_band() {
        let mut bars = BTreeMap::new();
        bars.insert(
            "SPY".to_string(),
            series(&[100.0, 100.0, 100.0, 100.0, 100.0]),
        );
        bars.insert(
            "MSFT".to_string(),
            series(&[100.0, 100.0, 100.1, 100.2, 100.3]),
        );
        let score = score(&pending(vec![("MSFT", "flat", 0.0)]), &bars, now()).unwrap();
        assert!(score.signals[0].correct, "{:?}", score.signals[0]);
    }

    #[test]
    fn missing_symbols_are_reported_not_faked() {
        let mut bars = BTreeMap::new();
        bars.insert(
            "SPY".to_string(),
            series(&[100.0, 100.0, 101.0, 102.0, 103.0]),
        );
        bars.insert(
            "MSFT".to_string(),
            series(&[100.0, 100.0, 103.0, 106.0, 109.0]),
        );
        let score = score(
            &pending(vec![("MSFT", "up", 1.0), ("AAPL", "up", 1.0)]),
            &bars,
            now(),
        )
        .unwrap();
        assert_eq!(score.signals.len(), 1);
        assert_eq!(score.missing_symbols, vec!["AAPL".to_string()]);
    }

    #[test]
    fn spearman_is_one_for_a_monotone_relationship() {
        let ic = spearman(&[1.0, 2.0, 3.0, 4.0], &[10.0, 20.0, 30.0, 40.0]).unwrap();
        assert!((ic - 1.0).abs() < 1e-9);
        let ic = spearman(&[1.0, 2.0, 3.0, 4.0], &[40.0, 30.0, 20.0, 10.0]).unwrap();
        assert!((ic + 1.0).abs() < 1e-9);
    }

    #[test]
    fn spearman_handles_ties_and_degeneracy() {
        assert!(spearman(&[1.0, 2.0], &[3.0, 4.0]).is_none());
        assert!(spearman(&[1.0, 1.0, 1.0], &[1.0, 2.0, 3.0]).is_none());
        let ic = spearman(&[1.0, 1.0, 2.0, 3.0], &[1.0, 2.0, 3.0, 4.0]).unwrap();
        assert!(ic > 0.8, "got {ic}");
    }

    fn score_with_ic(as_of_date: &str, ic: f64) -> ForecastScore {
        ForecastScore {
            arm_id: "research-test".into(),
            as_of: format!("{as_of_date}T13:30:00Z"),
            as_of_date: as_of_date.into(),
            base_date: as_of_date.into(),
            scored_at: format!("{as_of_date}T21:00:00Z"),
            horizon_days: 5,
            benchmark: "SPY".into(),
            benchmark_return_pct: 0.0,
            signals: vec![SignalScore {
                symbol: "MSFT".into(),
                direction: "up".into(),
                expected_return_pct: 1.0,
                confidence: 0.7,
                realized_return_pct: ic * 10.0,
                excess_return_pct: ic * 10.0,
                correct: ic > 0.0,
            }],
            hit_rate_pct: if ic > 0.0 { 100.0 } else { 0.0 },
            mean_excess_pct: ic * 10.0,
            rank_ic: Some(ic),
            missing_symbols: vec![],
        }
    }

    #[test]
    fn a_flat_day_does_not_count_toward_the_ic_gate() {
        // 20 scored days, none with a defined IC (everything marked flat).
        let flat: Vec<ForecastScore> = (0..MIN_DAYS)
            .map(|i| {
                let mut s = score_with_ic(&format!("2026-09-{:02}", i + 1), 0.0);
                s.rank_ic = None;
                s
            })
            .collect();
        let card = Scorecard::from_scores(&flat);
        assert_eq!(card.scored_days, MIN_DAYS);
        assert_eq!(card.ic_days, 0);
        assert!(!card.gate_pass);
        assert!(
            card.gate_reason.contains("defined IC"),
            "{}",
            card.gate_reason
        );
    }

    #[test]
    fn an_empty_scorecard_does_not_pass() {
        let card = Scorecard::from_scores(&[]);
        assert!(!card.gate_pass);
        assert_eq!(card.scored_days, 0);
        assert!(card.gate_reason.contains("no scored forecasts"));
    }

    #[test]
    fn the_gate_needs_days_ic_and_together() {
        let scores: Vec<ForecastScore> = (0..MIN_DAYS)
            .map(|i| score_with_ic(&format!("2026-09-{:02}", i + 1), 0.10))
            .collect();
        let card = Scorecard::from_scores(&scores);
        assert_eq!(card.scored_days, MIN_DAYS);
        // A constant IC stream has no dispersion, so t is undefined, not infinite.
        assert!(card.ic_t_stat.is_none());
        assert!(!card.gate_pass);
        assert!(card.gate_reason.contains("IC t"));

        let mut noisy = scores.clone();
        for (index, score) in noisy.iter_mut().enumerate() {
            // Alternate so the mean stays positive but dispersion is real.
            score.rank_ic = Some(if index % 2 == 0 { 0.20 } else { 0.04 });
        }
        let card = Scorecard::from_scores(&noisy);
        assert!(
            card.ic_t_stat.is_some_and(|t| t > MIN_T),
            "{:?}",
            card.ic_t_stat
        );
        assert!(card.gate_pass, "{}", card.gate_reason);

        let weak: Vec<ForecastScore> = (0..MIN_DAYS)
            .map(|i| {
                let mut s = score_with_ic(&format!("2026-09-{:02}", i + 1), 0.0);
                s.rank_ic = Some(0.01);
                s
            })
            .collect();
        let card = Scorecard::from_scores(&weak);
        assert!(!card.gate_pass);
        assert!(card.gate_reason.contains("below"));
    }

    #[test]
    fn t_stat_matches_the_textbook_formula() {
        let values = [0.1, 0.2, 0.3, 0.4, 0.5];
        let mean: f64 = 0.3;
        let variance = values
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / 4.0;
        let expected = mean / (variance.sqrt() / (values.len() as f64).sqrt());
        let actual = t_stat(&values).unwrap();
        assert!((actual - expected).abs() < 1e-12);
    }
}
