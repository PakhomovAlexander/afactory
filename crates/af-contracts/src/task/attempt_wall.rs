//! What the kernel records when an Attempt wall ends a Worker (ADR-0257).
//!
//! `af/TaskAttemptWallEnd@1` (`af.task-attempt-wall-end/1`) is the typed end of an Attempt whose
//! wall ran out before its Worker answered: the node, the Attempt, the wall and the time the
//! Attempt took. When the Worker writes source, the tree it left is sealed exactly as an
//! ordinary implementer's candidate is and kept as `af/TaskPartialCandidate@1`
//! (`af.task-partial-candidate/1`), which the wall end names. Both are raw evidence of the
//! failed Attempt's settlement: neither is an output, so no seal, check, review, acceptance or
//! delivery ever reads one. A continuation (`af task run NEW --from TASK`, ADR-0212) binds the
//! partial candidate as a new Task's source, which then runs its whole pipeline.

use serde::{Deserialize, Serialize};

use super::{is_name, require, safe_number};
use crate::is_digest;

pub const TASK_ATTEMPT_WALL_END_V1: &str = "af/TaskAttemptWallEnd@1";
pub const TASK_ATTEMPT_WALL_END_SCHEMA: &str = "af.task-attempt-wall-end/1";
pub const TASK_PARTIAL_CANDIDATE_V1: &str = "af/TaskPartialCandidate@1";
pub const TASK_PARTIAL_CANDIDATE_SCHEMA: &str = "af.task-partial-candidate/1";

/// The tree a source-writing Worker left when its Attempt wall ended it, sealed as a candidate
/// is: the same scratch and ignore exclusions, a derived Snapshot of the Worker's source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPartialCandidateV1 {
    /// Always [`TASK_PARTIAL_CANDIDATE_SCHEMA`].
    pub schema: String,
    /// The Snapshot the Worker started from: its invocation's `source`.
    pub parent_snapshot_id: String,
    /// The derived Snapshot of the tree it left.
    pub snapshot_id: String,
    /// The `af/SourceTree@1` that names `snapshot_id`, the artifact a continuation binds.
    pub source_tree_id: String,
    /// How many paths the tree adds, changes or removes against its parent.
    pub changed_paths: u64,
}

impl TaskPartialCandidateV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_PARTIAL_CANDIDATE_SCHEMA
                && is_digest(&self.parent_snapshot_id)
                && is_digest(&self.snapshot_id)
                && is_digest(&self.source_tree_id)
                && self.parent_snapshot_id != self.snapshot_id
                && safe_number(self.changed_paths),
            "A partial candidate names its parent Snapshot, its own derived Snapshot and \
             SourceTree, and a bounded count of changed paths",
        )
    }
}

/// The typed end of an Attempt its wall stopped before its Worker answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAttemptWallEndV1 {
    /// Always [`TASK_ATTEMPT_WALL_END_SCHEMA`].
    pub schema: String,
    pub task_id: String,
    /// The qualified node whose Attempt it was.
    pub node: String,
    pub attempt_id: String,
    /// The node's Attempt wall, as its allowance captured it.
    pub wall_ms: u64,
    /// The time from the Attempt's start to its end.
    pub elapsed_ms: u64,
    /// The `af/TaskPartialCandidate@1` the Worker's tree was kept as; absent for a Worker that
    /// writes no source, or whose tree could not be sealed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub partial_candidate_id: Option<String>,
}

impl TaskAttemptWallEndV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_ATTEMPT_WALL_END_SCHEMA,
            "An Attempt wall end needs its schema",
        )?;
        require(
            is_name(&self.task_id)
                && self.node.len() <= 4096
                && self.node.split('.').all(is_name)
                && self.attempt_id.len() == 26
                && self.attempt_id.bytes().all(|b| b.is_ascii_alphanumeric()),
            "An Attempt wall end names its Task, its qualified node and its exact Attempt",
        )?;
        require(
            self.wall_ms > 0 && safe_number(self.wall_ms) && safe_number(self.elapsed_ms),
            "An Attempt wall end needs a positive wall and a bounded elapsed time",
        )?;
        require(
            self.partial_candidate_id.as_deref().is_none_or(is_digest),
            "An Attempt wall end names its partial candidate by digest",
        )
    }

    /// The sentence every rendering leads with: `implementer reached its Attempt wall of 30 min
    /// after 30 min`, naming the Worker by its slot's own name (`root.slots.implementer` reads
    /// `implementer`), else by its node.
    pub fn describe(&self, slot: Option<&str>) -> String {
        let slot = slot.map(|slot| slot.rsplit('.').next().unwrap_or(slot));
        format!(
            "{} reached its Attempt wall of {} after {}",
            slot.unwrap_or(&self.node),
            duration_words(self.wall_ms),
            duration_words(self.elapsed_ms)
        )
    }
}

/// A duration as a person reads it: `45 s`, `30 min`, `2 h 5 min`, rounded to the second.
pub fn duration_words(ms: u64) -> String {
    let seconds = ms.saturating_add(500) / 1_000;
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3_600 => format!("{} min", seconds / 60),
        _ => match seconds % 3_600 / 60 {
            0 => format!("{} h", seconds / 3_600),
            minutes => format!("{} h {minutes} min", seconds / 3_600),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn wall_end() -> TaskAttemptWallEndV1 {
        TaskAttemptWallEndV1 {
            schema: TASK_ATTEMPT_WALL_END_SCHEMA.into(),
            task_id: "fix-1".into(),
            node: "root.nodes.implement".into(),
            attempt_id: "01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
            wall_ms: 1_800_000,
            elapsed_ms: 1_800_412,
            partial_candidate_id: Some(digest('f')),
        }
    }

    #[test]
    fn a_wall_end_names_its_attempt_wall_and_elapsed_time_in_words() {
        let end = wall_end();
        end.validate().unwrap();
        assert_eq!(
            end.describe(Some("root.slots.implementer")),
            "implementer reached its Attempt wall of 30 min after 30 min"
        );
        let short = TaskAttemptWallEndV1 {
            wall_ms: 2_000,
            elapsed_ms: 2_431,
            ..end.clone()
        };
        assert_eq!(
            short.describe(None),
            "root.nodes.implement reached its Attempt wall of 2 s after 2 s"
        );
        for broken in [
            TaskAttemptWallEndV1 {
                wall_ms: 0,
                ..end.clone()
            },
            TaskAttemptWallEndV1 {
                attempt_id: "short".into(),
                ..end.clone()
            },
            TaskAttemptWallEndV1 {
                partial_candidate_id: Some("not a digest".into()),
                ..end.clone()
            },
            TaskAttemptWallEndV1 {
                schema: "af.task-attempt-wall-end/2".into(),
                ..end
            },
        ] {
            assert!(broken.validate().is_err(), "{broken:?}");
        }
    }

    #[test]
    fn a_partial_candidate_is_a_derived_snapshot_of_its_parent() {
        let partial = TaskPartialCandidateV1 {
            schema: TASK_PARTIAL_CANDIDATE_SCHEMA.into(),
            parent_snapshot_id: digest('a'),
            snapshot_id: digest('b'),
            source_tree_id: digest('c'),
            changed_paths: 3,
        };
        partial.validate().unwrap();
        let same = TaskPartialCandidateV1 {
            snapshot_id: digest('a'),
            ..partial
        };
        assert!(
            same.validate().is_err(),
            "a partial tree is derived from its parent"
        );
    }

    #[test]
    fn durations_read_in_seconds_minutes_and_hours() {
        assert_eq!(duration_words(1_499), "1 s");
        assert_eq!(duration_words(59_400), "59 s");
        assert_eq!(duration_words(90_000), "1 min");
        assert_eq!(duration_words(3_600_000), "1 h");
        assert_eq!(duration_words(7_500_000), "2 h 5 min");
    }
}
