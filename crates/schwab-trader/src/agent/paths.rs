use std::path::{Path, PathBuf};

pub fn rules_stem(rules_path: &Path) -> String {
    rules_path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("trader")
        .to_string()
}

fn rules_dir(rules_path: &Path) -> PathBuf {
    rules_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn state_path(rules_path: &Path) -> PathBuf {
    rules_dir(rules_path).join(format!("trader-state-{}.json", rules_stem(rules_path)))
}

pub fn journal_path(rules_path: &Path) -> PathBuf {
    rules_dir(rules_path).join(format!("trader-journal-{}.jsonl", rules_stem(rules_path)))
}

pub fn pid_path(rules_path: &Path) -> PathBuf {
    rules_dir(rules_path).join(format!("trader-{}.pid", rules_stem(rules_path)))
}

pub fn log_path(rules_path: &Path) -> PathBuf {
    rules_dir(rules_path).join(format!("trader-{}.log", rules_stem(rules_path)))
}

pub fn append_trader_log(rules_path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let path = log_path(rules_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")
}
