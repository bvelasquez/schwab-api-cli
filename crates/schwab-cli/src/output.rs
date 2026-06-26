use chrono::Utc;
use clap::ValueEnum;
use console::Style;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, ValueEnum, Serialize, Deserialize, PartialEq, Eq)]
#[value(rename_all = "kebab-case")]
pub enum OutputFormat {
    #[default]
    Pretty,
    Json,
    Md,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub success: bool,
    pub command: String,
    pub inputs: Value,
    pub data: Value,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    #[serde(rename = "next_actions")]
    pub next_actions: Vec<String>,
    pub timestamp: String,
}

impl ResponseEnvelope {
    pub fn ok(command: impl Into<String>, data: Value) -> Self {
        Self {
            success: true,
            command: command.into(),
            inputs: Value::Null,
            data,
            warnings: vec![],
            errors: vec![],
            next_actions: vec![],
            timestamp: Utc::now().to_rfc3339(),
        }
    }

    pub fn err(command: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            success: false,
            command: command.into(),
            inputs: Value::Null,
            data: Value::Null,
            warnings: vec![],
            errors: vec![message.into()],
            next_actions: vec![],
            timestamp: Utc::now().to_rfc3339(),
        }
    }

    pub fn with_inputs(mut self, inputs: Value) -> Self {
        self.inputs = inputs;
        self
    }

    /// Attach non-fatal warnings to the response envelope.
    #[allow(dead_code)] // used from lib/tests; bin crate root triggers false positive
    pub fn with_warnings(mut self, warnings: Vec<String>) -> Self {
        self.warnings = warnings;
        self
    }

    pub fn with_next_actions(mut self, actions: Vec<String>) -> Self {
        self.next_actions = actions;
        self
    }
}

#[derive(Debug, Clone)]
pub struct OutputSink;

impl OutputSink {
    pub fn stdout() -> Self {
        Self
    }

    pub fn write(&self, envelope: &ResponseEnvelope, format: OutputFormat) {
        match format {
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(envelope).unwrap_or_else(|_| "{}".into())
                );
            }
            OutputFormat::Md => println!("{}", to_markdown(envelope)),
            OutputFormat::Pretty => print_pretty(envelope),
        }
    }
}

fn print_pretty(envelope: &ResponseEnvelope) {
    let title = Style::new().cyan().bold();
    let ok = Style::new().green().bold();
    let err = Style::new().red().bold();
    let dim = Style::new().dim();

    let is_agent_tick = matches!(envelope.command.as_str(), "agent tick" | "agent run once");

    if !is_agent_tick {
        println!("{}", title.apply_to(format!("schwab {}", envelope.command)));
        println!(
            "status: {}",
            if envelope.success {
                ok.apply_to("success")
            } else {
                err.apply_to("error")
            }
        );
    }

    if envelope.data != Value::Null {
        if let Some(body) = format_agent_pretty_body(&envelope.command, &envelope.data) {
            if is_agent_tick {
                println!("{}", dim.apply_to(format!("── {} ──", envelope.timestamp)));
            } else {
                println!();
            }
            println!("{body}");
        } else {
            println!("\ndata:");
            println!(
                "{}",
                serde_json::to_string_pretty(&envelope.data).unwrap_or_else(|_| "null".into())
            );
        }
    }

    if !envelope.warnings.is_empty() {
        println!("\n{}", Style::new().yellow().apply_to("warnings:"));
        for w in &envelope.warnings {
            println!("  - {w}");
        }
    }

    if !envelope.errors.is_empty() {
        println!("\n{}", Style::new().red().apply_to("errors:"));
        for e in &envelope.errors {
            println!("  - {e}");
        }
    }

    if !envelope.next_actions.is_empty() {
        println!("\n{}", Style::new().dim().apply_to("next:"));
        for n in &envelope.next_actions {
            println!("  {n}");
        }
    }
}

fn format_agent_pretty_body(command: &str, data: &Value) -> Option<String> {
    match command {
        "agent tick" | "agent run once" => Some(crate::agent::format::format_tick_data(data)),
        "agent status" => Some(crate::agent::format::format_status_data(data)),
        "agent validate" => Some(crate::agent::format::format_validate_data(data)),
        "agent background" => Some(crate::agent::format::format_background_data(data)),
        _ => None,
    }
}

fn to_markdown(envelope: &ResponseEnvelope) -> String {
    let mut md = String::new();
    md.push_str(&format!("# schwab {}\n\n", envelope.command));
    md.push_str(&format!(
        "**success:** {}\n\n",
        if envelope.success { "true" } else { "false" }
    ));
    if envelope.data != Value::Null {
        md.push_str("## data\n\n```json\n");
        md.push_str(&serde_json::to_string_pretty(&envelope.data).unwrap_or_default());
        md.push_str("\n```\n");
    }
    if !envelope.next_actions.is_empty() {
        md.push_str("\n## next_actions\n\n");
        for action in &envelope.next_actions {
            md.push_str(&format!("- `{action}`\n"));
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_err_and_inputs() {
        let e = ResponseEnvelope::ok("x", Value::Null).with_inputs(serde_json::json!({"a": 1}));
        assert!(e.success);
        let e = ResponseEnvelope::err("x", "nope");
        assert!(!e.success);
    }
}
