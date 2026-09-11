//! Shareable names for installed Task profiles. A package cannot install executable domain
//! handlers or replace the engine's acceptance rules by supplying an arbitrary schema.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKindProfile {
    Implementation,
    ReviewedImplementation,
    Review,
    Document,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskKindManifest {
    pub schema: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    pub profile: TaskKindProfile,
}

impl TaskKindManifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "af.task-kind/1"
            || !review_core::task::is_package_name(&self.name)
            || !review_core::task::is_package_name(&self.kind)
            || self.version.trim().is_empty()
        {
            return Err("Task-kind package needs an exact identity and installed profile".into());
        }
        Ok(())
    }
}
