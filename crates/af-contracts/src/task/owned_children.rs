//! Complete owned-child data. Only a protected Store registration grants membership;
//! these CAS bytes cannot introduce operators, resource bounds, or execution authority.

use super::{is_name, require};
use crate::is_digest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const TASK_OWNED_CHILD_SET_V1: &str = "af/TaskOwnedChildSet@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOwnedChildV1 {
    pub node: String,
    /// The exact item envelope, not an item-local logical identifier.
    pub source_item_id: String,
    pub invocation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOwnedChildSetV1 {
    pub plan_id: String,
    pub parent_invocation_id: String,
    pub source_artifact_id: String,
    /// Complete source order, including children that never acquire an Attempt.
    pub children: Vec<TaskOwnedChildV1>,
}

impl TaskOwnedChildSetV1 {
    pub fn artifact_refs(&self) -> Vec<&str> {
        let mut refs = vec![
            self.plan_id.as_str(),
            self.parent_invocation_id.as_str(),
            self.source_artifact_id.as_str(),
        ];
        for child in &self.children {
            refs.extend([child.source_item_id.as_str(), child.invocation_id.as_str()]);
        }
        refs
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.artifact_refs().iter().all(|id| is_digest(id)),
            "Invalid owned-child artifact identity",
        )?;
        let mut nodes = BTreeSet::new();
        let mut items = BTreeSet::new();
        let mut invocations = BTreeSet::new();
        for child in &self.children {
            require(
                child.node.split('.').all(is_name),
                "Invalid owned-child node address",
            )?;
            require(
                nodes.insert(&child.node)
                    && items.insert(&child.source_item_id)
                    && invocations.insert(&child.invocation_id),
                "Duplicate owned-child node, source item, or invocation",
            )?;
        }
        Ok(())
    }
}
