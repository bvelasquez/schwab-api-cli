use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct UniversePool {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub label: String,
    pub symbols: Vec<String>,
}

fn default_version() -> u32 {
    1
}

pub fn load_pool_file(path: &Path) -> Result<UniversePool> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read universe pool {}", path.display()))?;
    let trimmed = raw.trim_start();
    if trimmed.starts_with('-') || trimmed.starts_with('[') {
        let symbols: Vec<String> = serde_yaml::from_str(&raw)
            .with_context(|| format!("parse symbol list {}", path.display()))?;
        return Ok(UniversePool {
            version: 1,
            label: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("pool")
                .to_string(),
            symbols,
        });
    }
    serde_yaml::from_str(&raw).with_context(|| format!("parse universe pool {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_struct_pool() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.yaml");
        fs::write(
            &path,
            "version: 1\nlabel: test\nsymbols:\n  - AAPL\n  - MSFT\n",
        )
        .unwrap();
        let pool = load_pool_file(&path).unwrap();
        assert_eq!(pool.symbols.len(), 2);
        assert_eq!(pool.label, "test");
    }

    #[test]
    fn loads_plain_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("list.yaml");
        fs::write(&path, "- AAPL\n- GOOGL\n").unwrap();
        let pool = load_pool_file(&path).unwrap();
        assert_eq!(pool.symbols, vec!["AAPL", "GOOGL"]);
    }
}
