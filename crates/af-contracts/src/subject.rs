//! The kind of immutable Subject a review judges.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubjectKind {
    Diff,
    WholeTree,
}

impl fmt::Display for SubjectKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            SubjectKind::Diff => "diff",
            SubjectKind::WholeTree => "whole-tree",
        })
    }
}

/// The payload of a `review.kernel/Subject@1` artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectV1 {
    pub kind: SubjectKind,
    pub head_snapshot_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_set_id: Option<String>,
}

impl SubjectV1 {
    pub fn whole_tree(head_snapshot_id: impl Into<String>) -> Self {
        Self {
            kind: SubjectKind::WholeTree,
            head_snapshot_id: head_snapshot_id.into(),
            base_snapshot_id: None,
            change_set_id: None,
        }
    }

    pub fn diff(
        head_snapshot_id: impl Into<String>,
        base_snapshot_id: impl Into<String>,
        change_set_id: impl Into<String>,
    ) -> Self {
        Self {
            kind: SubjectKind::Diff,
            head_snapshot_id: head_snapshot_id.into(),
            base_snapshot_id: Some(base_snapshot_id.into()),
            change_set_id: Some(change_set_id.into()),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.head_snapshot_id) {
            return Err("Subject@1 has an invalid head Snapshot ID".into());
        }
        match self.kind {
            SubjectKind::WholeTree => {
                if self.base_snapshot_id.is_some() || self.change_set_id.is_some() {
                    return Err("a whole-tree Subject forbids Base and Change Set IDs".into());
                }
            }
            SubjectKind::Diff => {
                if !self
                    .base_snapshot_id
                    .as_deref()
                    .is_some_and(crate::is_digest)
                    || !self.change_set_id.as_deref().is_some_and(crate::is_digest)
                {
                    return Err("a diff Subject requires valid Base and Change Set IDs".into());
                }
            }
        }
        Ok(())
    }
}
