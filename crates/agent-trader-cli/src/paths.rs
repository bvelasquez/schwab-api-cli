use std::path::PathBuf;

use anyhow::Result;

pub fn config_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("AGENT_TRADER_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(directories::ProjectDirs::from("", "", "agent-trader")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".agent-trader")))
}

pub fn safety_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("AGENT_TRADER_SAFETY_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    Ok(config_dir()?.join("safety.json"))
}

pub fn state_dir() -> Result<PathBuf> {
    let dir = config_dir()?.join("state");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn load_dotenv() {
    let _ = dotenvy::dotenv();
    if let Ok(cwd) = std::env::current_dir() {
        let _ = dotenvy::from_path(cwd.join(".env"));
    }
}
