//! `af/diff-subject@1`: the change a verifying Worker judges, as the paths it touched with their
//! content before and after (ADR-0212).
//!
//! A Worker that judges a candidate without writing it — a reviewer, an evaluator, a verifier —
//! gets the tree before the change beside its own, read-only, at `../base`, and this document
//! beside both at `../diff-subject.json`. Both are written by the kernel from Snapshots it
//! recorded; neither is a port, so no Worker contract changes, and neither is sealed back.

use serde::{Deserialize, Serialize};

use super::require;
use crate::is_digest;

pub const DIFF_SUBJECT_SCHEMA: &str = "af/diff-subject@1";
/// Where the tree before the change is, relative to the Worker's own tree.
pub const DIFF_SUBJECT_BASE: &str = "../base";
/// The file name of the document beside the Worker's tree.
pub const DIFF_SUBJECT_FILE: &str = "diff-subject.json";
/// The most changed paths one document lists; a larger change lists none and says so.
pub const MAX_DIFF_PATHS: usize = 100_000;

/// One changed path: the content digest on each side, absent on the side that does not hold it.
/// A mode change keeps one digest on both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffSubjectChangeV1 {
    pub path: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub before: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffSubjectV1 {
    pub schema: String,
    /// The Snapshot materialized read-only at [`DIFF_SUBJECT_BASE`].
    pub base_snapshot_id: String,
    /// The Snapshot the Worker's own tree holds.
    pub snapshot_id: String,
    /// Always [`DIFF_SUBJECT_BASE`]: said in the document so a Worker reading it knows where.
    pub base: String,
    /// Every changed path, sorted, each once.
    pub changes: Vec<DiffSubjectChangeV1>,
}

impl DiffSubjectV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == DIFF_SUBJECT_SCHEMA
                && self.base == DIFF_SUBJECT_BASE
                && is_digest(&self.base_snapshot_id)
                && is_digest(&self.snapshot_id)
                && self.changes.len() <= MAX_DIFF_PATHS,
            "A diff subject names af/diff-subject@1, both Snapshots, ../base and at most \
             100000 changed paths",
        )?;
        for pair in self.changes.windows(2) {
            require(
                pair[0].path < pair[1].path,
                "A diff subject lists each changed path once, sorted",
            )?;
        }
        for change in &self.changes {
            require(
                !change.path.is_empty()
                    && (change.before.is_some() || change.after.is_some())
                    && change.before.as_deref().is_none_or(is_digest)
                    && change.after.as_deref().is_none_or(is_digest),
                "A changed path names the content digest on at least one side",
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    #[test]
    fn a_diff_subject_lists_each_changed_path_once_with_a_side() {
        let change = |path: &str, before: Option<char>, after: Option<char>| DiffSubjectChangeV1 {
            path: path.into(),
            before: before.map(id),
            after: after.map(id),
        };
        let mut subject = DiffSubjectV1 {
            schema: DIFF_SUBJECT_SCHEMA.into(),
            base_snapshot_id: id('1'),
            snapshot_id: id('2'),
            base: DIFF_SUBJECT_BASE.into(),
            changes: vec![
                change("a.rs", None, Some('a')),
                change("b.rs", Some('b'), Some('c')),
                change("c.rs", Some('d'), None),
            ],
        };
        subject.validate().unwrap();
        subject.changes.swap(0, 1);
        assert!(subject.validate().is_err(), "sorted");
        subject.changes.swap(0, 1);
        subject.changes[1].before = None;
        subject.changes[1].after = None;
        assert!(subject.validate().is_err(), "one side at least");
    }
}
