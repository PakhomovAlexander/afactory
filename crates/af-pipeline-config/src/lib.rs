//! Current typed Task packages and captured authority. Campaign definitions are retired.
//! Shared literal/provenance command DTOs remain used by Task Workers and acceptance checks.
pub mod excerpt;
pub mod layout;
pub mod lock;
pub mod path_glob;
pub mod task;

use af_contracts::{Arg, Command, Provenance};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgSpec {
    pub value: String,
    /// Defaults to `literal`, because a project writing its own check command is trusted. A
    /// value derived from the change under review must say so explicitly — the safe default is
    /// the one that cannot be reached by forgetting.
    #[serde(default = "default_provenance")]
    pub provenance: ProvenanceSpec,
}

fn default_provenance() -> ProvenanceSpec {
    ProvenanceSpec::Literal
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceSpec {
    Literal,
    Untrusted,
}

impl From<ProvenanceSpec> for Provenance {
    fn from(spec: ProvenanceSpec) -> Provenance {
        match spec {
            ProvenanceSpec::Literal => Provenance::Literal,
            ProvenanceSpec::Untrusted => Provenance::Untrusted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<ArgSpec>,
}

impl CommandSpec {
    pub fn build(&self) -> Command {
        Command::new(
            &self.program,
            self.args
                .iter()
                .map(|a| Arg {
                    value: a.value.clone(),
                    provenance: a.provenance.into(),
                })
                .collect(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSpec {
    pub name: String,
    #[serde(flatten)]
    pub command: CommandSpec,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

/// Refusals from current Task TOML parsing and contract validation.
#[derive(Debug)]
pub enum ConfigError {
    Parse(String),
    Binding(String),
}
impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) | Self::Binding(e) => write!(f, "pipeline definition: {e}"),
        }
    }
}
impl std::error::Error for ConfigError {}

/// Immutable dependency caches shared by current Task checks and machine cache policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheKindSpec {
    Cargo,
}
