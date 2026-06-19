use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, ValueEnum, Serialize, Deserialize, PartialEq, Eq)]
#[value(rename_all = "kebab-case")]
pub enum CliMode {
    #[default]
    Agent,
    Human,
}

impl CliMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Human => "human",
        }
    }

    pub fn is_human(self) -> bool {
        matches!(self, Self::Human)
    }
}
