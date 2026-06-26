use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use inquire::Select;

const RULES_ENV: &str = "SCHWAB_RULES";

/// Candidate rules.yaml paths under `rules/` relative to cwd and the workspace.
pub fn discover_rules_files() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut dirs = vec![PathBuf::from("rules")];
    if let Ok(cwd) = env::current_dir() {
        dirs.push(cwd.join("rules"));
    }
    let manifest_rules = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules");
    if manifest_rules.is_dir() {
        dirs.push(manifest_rules);
    }

    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x == "yaml" || x == "yml")
            })
            .collect();
        files.sort();
        for path in files {
            let canonical = path.canonicalize().unwrap_or(path);
            if seen.insert(canonical.clone()) {
                found.push(canonical);
            }
        }
    }

    found
}

/// Resolve rules file from explicit path, `SCHWAB_RULES`, discovery, or interactive pick.
pub fn resolve_rules_file(explicit: Option<PathBuf>, interactive: bool) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }

    if let Ok(env_path) = env::var(RULES_ENV) {
        let path = PathBuf::from(env_path);
        if path.is_file() {
            return Ok(path);
        }
        anyhow::bail!("{RULES_ENV} points to missing file: {}", path.display());
    }

    let candidates = discover_rules_files();
    match candidates.len() {
        0 => anyhow::bail!(
            "no rules file specified — pass a path, set {RULES_ENV}, or add rules/*.yaml"
        ),
        1 => Ok(candidates.into_iter().next().unwrap()),
        _ if interactive => {
            let labels: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
            let choice = Select::new("Select rules file", labels.clone())
                .with_help_message("Multiple rules/*.yaml found")
                .prompt()
                .context("rules file selection cancelled")?;
            let idx = labels
                .iter()
                .position(|l| l == &choice)
                .context("invalid selection")?;
            Ok(candidates[idx].clone())
        }
        _ => {
            let listing = candidates
                .iter()
                .map(|p| format!("  - {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!(
                "multiple rules files found — pass one explicitly or set {RULES_ENV}:\n{listing}"
            )
        }
    }
}

pub fn list_rules_files_display() -> String {
    let files = discover_rules_files();
    if files.is_empty() {
        return "  (no rules/*.yaml found in ./rules)".into();
    }
    files
        .iter()
        .map(|p| format!("  • {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_finds_project_rules() {
        let files = discover_rules_files();
        assert!(
            files
                .iter()
                .any(|p| p.to_string_lossy().contains("options-pilot")),
            "expected project rules files, got {:?}",
            files
        );
    }
}
