use std::path::Path;

use anyhow::{Context, Result};

use crate::rules::{TraderRules, WatchlistThematic};
use crate::watchlist::build::WriteTarget;

pub fn apply_build_to_rules(
    rules: &mut TraderRules,
    thematic: &[WatchlistThematic],
    core_append: &[String],
    target: WriteTarget,
) -> Result<()> {
    match target {
        WriteTarget::Thematic => {
            rules.watchlists.thematic = thematic.to_vec();
        }
        WriteTarget::Core => {
            append_core(rules, core_append);
        }
        WriteTarget::Both => {
            rules.watchlists.thematic = thematic.to_vec();
            append_core(rules, core_append);
        }
    }
    rules.validate()?;
    Ok(())
}

fn append_core(rules: &mut TraderRules, append: &[String]) {
    for sym in append {
        let u = sym.trim().to_uppercase();
        if u.is_empty() {
            continue;
        }
        if rules
            .watchlists
            .core
            .iter()
            .any(|s| s.eq_ignore_ascii_case(&u))
        {
            continue;
        }
        rules.watchlists.core.push(u);
    }
}

pub fn write_rules_watchlists(
    rules_path: &Path,
    thematic: &[WatchlistThematic],
    core_append: &[String],
    target: WriteTarget,
) -> Result<TraderRules> {
    let mut rules = TraderRules::load(rules_path)?;
    apply_build_to_rules(&mut rules, thematic, core_append, target)?;
    rules
        .save(rules_path)
        .with_context(|| format!("write watchlists to {}", rules_path.display()))?;
    Ok(rules)
}
