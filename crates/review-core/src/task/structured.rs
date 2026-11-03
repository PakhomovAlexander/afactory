//! Installed data-only structural contracts. Package schemas never confer kernel authority.
use super::{ArtifactInputV1, is_name};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const STRUCTURED_VALIDATION_V1: &str = "af/StructuredValidation@1";
pub const MAX_STRUCTURED_BYTES: usize = 65536;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredPortV1 {
    pub artifact_type: String,
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredContractV1 {
    pub schema: String,
    pub inputs: BTreeMap<String, StructuredPortV1>,
    pub outputs: BTreeMap<String, StructuredPortV1>,
    pub require_container: bool,
}
impl StructuredContractV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "af.structured-contract/1"
            || !(1..=4).contains(&self.inputs.len())
            || !(1..=4).contains(&self.outputs.len())
            || self.inputs.iter().chain(&self.outputs).any(|(name, port)| {
                !is_name(name)
                    || matches!(
                        name.as_str(),
                        "requirements"
                            | "source"
                            | "base"
                            | "history"
                            | "sources"
                            | "findings"
                            | "acceptance"
                            | "validation"
                    )
                    || name.starts_with("result_")
                    || !port.artifact_type.starts_with("custom/")
                    || !super::super::is_artifact_type(&port.artifact_type)
            })
            || serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 262144
        {
            return Err(
                "Structured contracts need bounded custom/ typed ports, without reserved names"
                    .into(),
            );
        }
        Ok(())
    }
}

/// Kernel-authored evidence: structural success is deliberately distinct from semantic review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredValidationV1 {
    pub schema: String,
    pub invocation: super::execution::TaskInvocationV1,
    pub plan_id: String,
    pub task_revision_id: String,
    pub contract_id: String,
    pub inputs: BTreeMap<String, ArtifactInputV1>,
    pub outputs: BTreeMap<String, ArtifactInputV1>,
    pub conclusion: String,
}
impl StructuredValidationV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "af.structured-validation/1"
            || self.conclusion != "structurally_validated"
            || [&self.plan_id, &self.task_revision_id, &self.contract_id]
                .iter()
                .any(|id| !crate::is_digest(id))
            || self.inputs.is_empty()
            || self.outputs.is_empty()
        {
            return Err(
                "Structural validation requires exact provenance and its structural conclusion"
                    .into(),
            );
        }
        self.invocation.validate()?;
        if self.invocation.plan_id != self.plan_id {
            return Err("Validation changed its plan".into());
        }
        for (name, port) in self.inputs.iter().chain(&self.outputs) {
            if !is_name(name) {
                return Err("Invalid structural validation port".into());
            }
            port.validate()?;
        }
        Ok(())
    }
}
