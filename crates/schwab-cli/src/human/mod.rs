use anyhow::{Context, Result};
use inquire::Select;

use crate::config::RuntimeConfig;

pub async fn pick_account_hash(
    _runtime: &RuntimeConfig,
    api: &schwab_api::TraderApi,
) -> Result<String> {
    let numbers = api
        .accounts()
        .account_numbers()
        .await
        .context("Failed to list account numbers")?;

    if numbers.is_empty() {
        anyhow::bail!("No linked accounts found");
    }

    let labels: Vec<String> = numbers
        .iter()
        .map(|n| {
            format!(
                "{}  (hash: {}…)",
                n.account_number,
                &n.hash_value[..n.hash_value.len().min(8)]
            )
        })
        .collect();

    let choice = Select::new("Select account", labels.clone()).prompt()?;
    let idx = labels
        .iter()
        .position(|l| l == &choice)
        .context("Invalid selection")?;

    Ok(numbers[idx].hash_value.clone())
}

pub fn read_order_json(prompt: &str) -> Result<serde_json::Value> {
    let raw = inquire::Text::new(prompt)
        .with_help_message("Path to .json file or inline JSON object")
        .prompt()?;
    parse_order_input(&raw)
}

pub fn parse_order_input(raw: &str) -> Result<serde_json::Value> {
    let path = std::path::Path::new(raw.trim());
    if path.is_file() {
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    } else {
        Ok(serde_json::from_str(raw)?)
    }
}
