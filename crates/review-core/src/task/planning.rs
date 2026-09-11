//! A Planner may propose bounded Pipeline TOML that refers to captured packages. A proposal
//! never installs Worker code, changes Task acceptance, or supplies developer authorization.
use super::{is_package_name, require};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PIPELINE_PROPOSAL_V1: &str = "af/PipelineProposal@1";
pub const PLANNING_REQUEST_V1: &str = "af/PlanningRequest@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineProposalV1 {
    pub schema: String,
    pub root: String,
    /// Exact proposed Pipeline TOML by package name. Compiler admission parses and validates
    /// these bytes using the same package path as configured Pipelines.
    pub definitions: BTreeMap<String, String>,
}
impl PipelineProposalV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.pipeline-proposal/1"
                && self.definitions.contains_key(&self.root)
                && !self.definitions.is_empty()
                && self.definitions.len() <= 16,
            "Pipeline proposal requires a bounded declared root and definitions",
        )?;
        let mut bytes = 0usize;
        for (name, source) in &self.definitions {
            require(
                is_package_name(name)
                    && name.starts_with("generated/")
                    && !source.trim().is_empty()
                    && source.len() <= 262144,
                "Generated Pipeline requires its reserved namespace and bounded TOML",
            )?;
            bytes = bytes
                .checked_add(source.len())
                .ok_or("Proposal size overflow")?;
        }
        require(
            bytes <= 1024 * 1024,
            "Pipeline proposal exceeds its total byte bound",
        )
    }
}
