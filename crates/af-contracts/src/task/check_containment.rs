//! Observed containment of native Task checks, beside their unchanged check receipt.
use super::{is_name, require};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const TASK_CHECK_CONTAINMENT_V1: &str = "af/TaskCheckContainment@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCheckContainmentV1 {
    pub receipt_id: String,
    pub checks: BTreeMap<String, ContainedCheckV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainedCheckV1 {
    pub isolation: String,
    pub image: String,
    pub admitted: bool,
    pub execution: ContainerExecutionV1,
}

/// Completion means the shared provider returned captured process output, not that the check
/// passed. Refusal includes failed execution with confirmed cleanup. Unconfirmed cleanup
/// fails the Attempt and preserves its sandbox instead of publishing an evidence document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerExecutionV1 {
    NotStarted,
    Refused,
    Completed,
}

impl TaskCheckContainmentV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            crate::is_digest(&self.receipt_id)
                && !self.checks.is_empty()
                && self.checks.len() <= 32
                && self.checks.iter().all(|(name, check)| {
                    let pinned = check.image.split_once("@sha256:");
                    is_name(name)
                        && check.isolation == "container"
                        && check.admitted
                        && pinned.is_some_and(|(image, digest)| {
                            !image.is_empty()
                                && image.len() <= 256
                                && image.bytes().enumerate().all(|(i, b)| {
                                    b.is_ascii_lowercase()
                                        || b.is_ascii_digit()
                                        || (i > 0 && b"./:_-".contains(&b))
                                })
                                && digest.len() == 64
                                && digest
                                    .bytes()
                                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        })
                }),
            "Contained check evidence needs an exact receipt, admitted Container isolation and pinned images",
        )
    }
}
