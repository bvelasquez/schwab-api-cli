//! The mutable artifact: a bounded `playbook` tree plus a versioned overlay store.
//!
//! Plan §5 says the agent mutates a research rules file and every tick that
//! proposes a patch writes a new version with a diff and a rollback pointer.
//! Rewriting YAML would destroy comments and operator edits, so this arm keeps
//! the arm YAML immutable and stores accepted patches as versioned overlays:
//!
//! ```text
//! rules/research/<arm>.versions/20261029T171500Z.json   # full playbook tree + diff
//! rules/research/<arm>.versions/active.json             # pointer to the live overlay
//! ```
//!
//! Only the `playbook` subtree is ever read back, so an overlay can never change
//! the account, mode, gate, universe or credentials. Rollback is deleting the
//! pointer (`research rollback`).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::ArmConfig;
use crate::patch::{apply_patches, AppliedPatch, Patch};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Playbook {
    pub exit: ExitPlaybook,
    pub entry: EntryPlaybook,
}

impl Default for Playbook {
    fn default() -> Self {
        Self {
            exit: ExitPlaybook::default(),
            entry: EntryPlaybook::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ExitPlaybook {
    pub profit_target_pct: f64,
    pub stop_loss_pct: f64,
    pub trailing: TrailingPlaybook,
}

impl Default for ExitPlaybook {
    fn default() -> Self {
        Self {
            profit_target_pct: 8.0,
            stop_loss_pct: 4.0,
            trailing: TrailingPlaybook::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TrailingPlaybook {
    pub trail_atr_multiple: f64,
    pub activate_after_profit_pct: f64,
}

impl Default for TrailingPlaybook {
    fn default() -> Self {
        Self {
            trail_atr_multiple: 2.0,
            activate_after_profit_pct: 4.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EntryPlaybook {
    /// Acceptable RSI(14) band at entry. Above the high is treated as
    /// overbought, below the low as a falling knife. Both skip the entry.
    pub rsi_14_range: [f64; 2],
}

impl Default for EntryPlaybook {
    fn default() -> Self {
        Self {
            rsi_14_range: [35.0, 70.0],
        }
    }
}

impl Playbook {
    /// Fail closed on a structurally impossible playbook.
    pub fn validate(&self) -> Result<()> {
        if self.exit.stop_loss_pct <= 0.0 || self.exit.stop_loss_pct >= self.exit.profit_target_pct
        {
            bail!(
                "playbook.exit.stop_loss_pct ({}) must be > 0 and < profit_target_pct ({})",
                self.exit.stop_loss_pct,
                self.exit.profit_target_pct
            );
        }
        if self.exit.trailing.trail_atr_multiple <= 0.0 {
            bail!("playbook.exit.trailing.trail_atr_multiple must be > 0");
        }
        if self.exit.trailing.activate_after_profit_pct <= 0.0 {
            bail!("playbook.exit.trailing.activate_after_profit_pct must be > 0");
        }
        let [low, high] = self.entry.rsi_14_range;
        if !(0.0..=100.0).contains(&low) || !(0.0..=100.0).contains(&high) || low >= high {
            bail!("playbook.entry.rsi_14_range must be [low, high] with 0 <= low < high <= 100");
        }
        Ok(())
    }

    /// The patch engine roots at `{ "playbook": ... }`.
    pub fn to_root(&self) -> Value {
        json!({ "playbook": self })
    }

    pub fn from_root(root: &Value) -> Result<Self> {
        let tree = root
            .get("playbook")
            .context("versioned playbook is missing the `playbook` key")?;
        let playbook: Playbook =
            serde_json::from_value(tree.clone()).context("versioned playbook does not parse")?;
        playbook.validate()?;
        Ok(playbook)
    }

    /// Apply bounded patches. Returns the patched playbook, the applied diff, and
    /// the resulting root tree (for storage). Fails closed: nothing is returned
    /// unless every patch clears the allowlist and the rails.
    pub fn apply(
        &self,
        patches: &[Patch],
        allowlist: &[String],
    ) -> Result<(Playbook, Vec<AppliedPatch>, Value)> {
        let mut root = self.to_root();
        let applied = apply_patches(&mut root, patches, allowlist)?;
        let patched = Playbook::from_root(&root)?;
        Ok((patched, applied, root))
    }
}

/// Everything the agent may adapt, and nothing else.
///
/// The arm YAML is the immutable base; an active overlay is this whole tree. Only
/// these fields are ever read back, so an overlay can never touch account, mode,
/// the gate, the universe, the cost model, the kill switch, or the scoring
/// contract. Every path here has a rail in [`crate::patch::rail_for`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Mutable {
    pub playbook: Playbook,
    pub paper: MutablePaper,
    pub forecast: MutableForecast,
}

impl Default for Mutable {
    fn default() -> Self {
        Self {
            playbook: Playbook::default(),
            paper: MutablePaper::default(),
            forecast: MutableForecast::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MutablePaper {
    pub max_open_positions: u32,
    pub max_hold_days: u32,
}

impl Default for MutablePaper {
    fn default() -> Self {
        Self {
            max_open_positions: 5,
            max_hold_days: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MutableForecast {
    pub min_confidence: f64,
}

impl Default for MutableForecast {
    fn default() -> Self {
        Self {
            min_confidence: 0.55,
        }
    }
}

impl Mutable {
    /// The arm YAML's values: the base an overlay starts from.
    pub fn from_config(config: &ArmConfig) -> Self {
        Self {
            playbook: config.playbook.clone(),
            paper: MutablePaper {
                max_open_positions: config.paper.max_open_positions,
                max_hold_days: config.paper.max_hold_days,
            },
            forecast: MutableForecast {
                min_confidence: config.forecast.min_confidence,
            },
        }
    }

    /// Write the adapted values back onto a config clone so the book, the prompt
    /// and the dashboard all see the effective settings.
    pub fn apply_to(&self, config: &mut ArmConfig) {
        config.playbook = self.playbook.clone();
        config.paper.max_open_positions = self.paper.max_open_positions;
        config.paper.max_hold_days = self.paper.max_hold_days;
        config.forecast.min_confidence = self.forecast.min_confidence;
    }

    /// Sane bounds for a value read from YAML or an overlay. Looser than the
    /// patch rails on purpose: a human may set a value the agent could not.
    pub fn validate(&self) -> Result<()> {
        self.playbook.validate()?;
        if !(1..=50).contains(&self.paper.max_open_positions) {
            bail!("paper.max_open_positions must be within 1..=50");
        }
        if !(1..=60).contains(&self.paper.max_hold_days) {
            bail!("paper.max_hold_days must be within 1..=60");
        }
        if !(0.0..=1.0).contains(&self.forecast.min_confidence) {
            bail!("forecast.min_confidence must be within 0..=1");
        }
        Ok(())
    }

    /// The patch engine roots at this whole tree.
    pub fn to_root(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn from_root(root: &Value) -> Result<Self> {
        let mutable: Mutable =
            serde_json::from_value(root.clone()).context("versioned overlay does not parse")?;
        mutable.validate()?;
        Ok(mutable)
    }

    /// Apply bounded patches. Returns the patched tree, the applied diff, and the
    /// resulting root value (for storage). Fails closed: nothing is returned
    /// unless every patch clears the allowlist and the rails.
    pub fn apply(
        &self,
        patches: &[Patch],
        allowlist: &[String],
    ) -> Result<(Mutable, Vec<AppliedPatch>, Value)> {
        let mut root = self.to_root();
        let applied = apply_patches(&mut root, patches, allowlist)?;
        let patched = Mutable::from_root(&root)?;
        Ok((patched, applied, root))
    }
}

/// Directory that holds overlays for `rules/research/<arm>.yaml`.
pub fn versions_dir(rules_path: &Path) -> PathBuf {
    let stem = rules_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "arm".into());
    rules_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{stem}.versions"))
}

fn active_pointer(rules_path: &Path) -> PathBuf {
    versions_dir(rules_path).join("active.json")
}

/// The settings the arm must trade this tick: the active overlay if one exists,
/// otherwise the arm YAML. A broken overlay is an error, never a silent fallback,
/// so a bad patch cannot quietly change behavior.
pub fn load_active(rules_path: &Path, fallback: &Mutable) -> Result<Mutable> {
    let pointer = active_pointer(rules_path);
    if !pointer.is_file() {
        return Ok(fallback.clone());
    }
    let raw =
        std::fs::read_to_string(&pointer).with_context(|| format!("read {}", pointer.display()))?;
    let doc: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", pointer.display()))?;
    let file = doc
        .get("file")
        .and_then(Value::as_str)
        .context("active pointer is missing `file`")?;
    let path = versions_dir(rules_path).join(file);
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let version: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    // Prefer the full mutable tree; fall back to a legacy `playbook`-only file by
    // merging its playbook over the current base.
    if version.get("mutable").is_some() {
        let tree = version.get("mutable").cloned().unwrap_or(Value::Null);
        return Mutable::from_root(&tree);
    }
    let mut merged = fallback.clone();
    merged.playbook = Playbook::from_root(&version)?;
    Ok(merged)
}

/// Write one version. `activate` promotes it to the live overlay (atomic rename).
/// A version is written even when not activated, so proposals are auditable.
pub fn write_version(
    rules_path: &Path,
    root: &Value,
    applied: &[AppliedPatch],
    meta: Value,
    activate: bool,
) -> Result<PathBuf> {
    let mutable = Mutable::from_root(root)?;
    let dir = versions_dir(rules_path);
    std::fs::create_dir_all(&dir)?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let name = format!("{stamp}.json");
    let path = dir.join(&name);
    let record = json!({
        "ts": Utc::now().to_rfc3339(),
        "mutable": mutable,
        "playbook": mutable.playbook,
        "applied": applied,
        "meta": meta,
        "active": activate,
    });
    let pretty = serde_json::to_string_pretty(&record)?;
    // Write-then-rename so a crash cannot leave a half-written version.
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, &pretty).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))?;
    if activate {
        let pointer = active_pointer(rules_path);
        let doc = json!({
            "file": name,
            "ts": record["ts"],
            "applied": applied.len(),
        });
        let tmp = dir.join(".active.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&doc)?)?;
        std::fs::rename(&tmp, &pointer).with_context(|| format!("rename {}", pointer.display()))?;
    }
    Ok(path)
}

/// Remove the active pointer. Versions are never deleted, so rollback is
/// reversible by re-writing a pointer.
pub fn clear_active(rules_path: &Path) -> Result<Option<String>> {
    let pointer = active_pointer(rules_path);
    if !pointer.is_file() {
        return Ok(None);
    }
    let previous = std::fs::read_to_string(&pointer).ok();
    std::fs::remove_file(&pointer).with_context(|| format!("remove {}", pointer.display()))?;
    Ok(previous)
}

/// Version files, newest first.
pub fn list_versions(rules_path: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(versions_dir(rules_path)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            (name.ends_with(".json") && !name.starts_with('.') && name != "active.json")
                .then_some(name)
        })
        .collect();
    names.sort();
    names.reverse();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::effective_allowlist;

    fn allow() -> Vec<String> {
        effective_allowlist(&[])
    }

    #[test]
    fn defaults_are_valid() {
        Playbook::default().validate().unwrap();
    }

    #[test]
    fn stop_must_be_below_target() {
        let mut playbook = Playbook::default();
        playbook.exit.stop_loss_pct = 12.0;
        assert!(playbook.validate().is_err());
    }

    #[test]
    fn inverted_rsi_band_is_rejected() {
        let mut playbook = Playbook::default();
        playbook.entry.rsi_14_range = [80.0, 40.0];
        assert!(playbook.validate().is_err());
    }

    #[test]
    fn patches_are_clamped_by_the_rails() {
        let (patched, applied, _root) = Playbook::default()
            .apply(
                &[Patch {
                    path: "playbook.exit.profit_target_pct".into(),
                    value: json!(99.0),
                    reason: "test".into(),
                }],
                &allow(),
            )
            .unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!(patched.exit.profit_target_pct, 20.0);
    }

    #[test]
    fn a_patch_that_would_break_the_playbook_is_refused() {
        // stop 10% with target 8% is structurally invalid after the clamp.
        let err = Playbook::default().apply(
            &[
                Patch {
                    path: "playbook.exit.stop_loss_pct".into(),
                    value: json!(10.0),
                    reason: "test".into(),
                },
                Patch {
                    path: "playbook.exit.profit_target_pct".into(),
                    value: json!(4.0),
                    reason: "test".into(),
                },
            ],
            &allow(),
        );
        assert!(err.is_err());
    }

    #[test]
    fn version_store_round_trips_and_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("arm.yaml");
        let base = Mutable::default();
        let (patched, applied, root) = base
            .apply(
                &[
                    Patch {
                        path: "playbook.exit.stop_loss_pct".into(),
                        value: json!(3.0),
                        reason: "test".into(),
                    },
                    Patch {
                        path: "paper.max_open_positions".into(),
                        value: json!(8),
                        reason: "test".into(),
                    },
                ],
                &allow(),
            )
            .unwrap();
        let path = write_version(&rules, &root, &applied, json!({"why": "test"}), true).unwrap();
        assert!(path.is_file());
        let loaded = load_active(&rules, &base).unwrap();
        assert_eq!(
            loaded.playbook.exit.stop_loss_pct,
            patched.playbook.exit.stop_loss_pct
        );
        assert_eq!(loaded.paper.max_open_positions, 8);
        assert_eq!(list_versions(&rules).len(), 1);

        let removed = clear_active(&rules).unwrap();
        assert!(removed.is_some());
        let rolled = load_active(&rules, &base).unwrap();
        assert_eq!(rolled, base);
    }

    #[test]
    fn a_legacy_playbook_only_overlay_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("arm.yaml");
        let versions = versions_dir(&rules);
        std::fs::create_dir_all(&versions).unwrap();
        let legacy = json!({
            "playbook": { "exit": { "stop_loss_pct": 2.5, "profit_target_pct": 9.0 } }
        });
        std::fs::write(versions.join("20260101T000000Z.json"), legacy.to_string()).unwrap();
        std::fs::write(
            versions.join("active.json"),
            r#"{"file":"20260101T000000Z.json"}"#,
        )
        .unwrap();
        let mut base = Mutable::default();
        base.paper.max_open_positions = 4;
        let loaded = load_active(&rules, &base).unwrap();
        assert_eq!(loaded.playbook.exit.stop_loss_pct, 2.5);
        // Non-playbook fields keep the base value.
        assert_eq!(loaded.paper.max_open_positions, 4);
    }

    #[test]
    fn a_broken_overlay_is_an_error_not_a_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("arm.yaml");
        let versions = versions_dir(&rules);
        std::fs::create_dir_all(&versions).unwrap();
        std::fs::write(versions.join("active.json"), r#"{"file":"missing.json"}"#).unwrap();
        assert!(load_active(&rules, &Mutable::default()).is_err());
    }

    #[test]
    fn overlay_cannot_override_non_mutable_fields() {
        // Extra keys in a version file are ignored: from_root only reads the tree.
        let root = json!({"playbook": Playbook::default(), "account": "schwab:HASH"});
        let parsed = Mutable::from_root(&root).unwrap();
        assert_eq!(parsed.playbook, Playbook::default());
        assert_eq!(parsed.paper, MutablePaper::default());
    }

    #[test]
    fn from_config_and_apply_to_round_trip() {
        let mut config = ArmConfig {
            arm_id: "arm".into(),
            account: Some("public:ACC".into()),
            universe: vec!["MSFT".into()],
            ..Default::default()
        };
        config.paper.max_open_positions = 3;
        config.paper.max_hold_days = 7;
        config.forecast.min_confidence = 0.6;
        config.playbook.exit.stop_loss_pct = 3.5;

        let mutable = Mutable::from_config(&config);
        assert_eq!(mutable.paper.max_open_positions, 3);
        assert_eq!(mutable.paper.max_hold_days, 7);
        assert_eq!(mutable.forecast.min_confidence, 0.6);
        assert_eq!(mutable.playbook.exit.stop_loss_pct, 3.5);

        let mut other = config.clone();
        let mut adapted = mutable.clone();
        adapted.paper.max_open_positions = 9;
        adapted.forecast.min_confidence = 0.4;
        adapted.apply_to(&mut other);
        assert_eq!(other.paper.max_open_positions, 9);
        assert_eq!(other.forecast.min_confidence, 0.4);
        assert_eq!(other.paper.max_hold_days, 7);
    }

    #[test]
    fn mutable_validate_rejects_absurd_values() {
        let mut mutable = Mutable::default();
        mutable.paper.max_open_positions = 0;
        assert!(mutable.validate().is_err());
        let mut mutable = Mutable::default();
        mutable.paper.max_hold_days = 0;
        assert!(mutable.validate().is_err());
        let mut mutable = Mutable::default();
        mutable.forecast.min_confidence = 1.5;
        assert!(mutable.validate().is_err());
    }
}
