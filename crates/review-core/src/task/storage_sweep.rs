//! The record `af/TaskStorageSweep@1` (ADR-0144): what the Storage Budget's sweep at the end of
//! `af task run` removed, as an observation of the Task that run executed. It travels inline in
//! one `storage_sweep` transition of the Task's log, references no artifact and never changes
//! the Task's result. It is bounded like a gate cleanup: at most [`MAX_SWEEP_REMOVALS`] removals
//! and [`MAX_SWEEP_FAILURES`] failures are listed, each bounded, and the rest only counted. A
//! sweep that removed nothing and failed nothing records nothing.

use serde::{Deserialize, Serialize};

use super::{is_name, present_option, require, safe_number};

pub const TASK_STORAGE_SWEEP_V1: &str = "af/TaskStorageSweep@1";

/// At most this many removals one record lists; the rest are counted in `omitted_removals`.
pub const MAX_SWEEP_REMOVALS: usize = 256;
/// At most this many failures one record lists; the rest are counted in `omitted_failures`.
pub const MAX_SWEEP_FAILURES: usize = 64;
/// The longest path a removal names, in bytes.
pub const MAX_SWEEP_PATH_BYTES: usize = 4096;
/// The longest failure a record keeps, in bytes.
pub const MAX_SWEEP_FAILURE_BYTES: usize = 2048;

/// What kind of entry a sweep removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageSweepKindV1 {
    WarmKey,
    Workspace,
    Campaign,
    Task,
    UnreadableStore,
    Version,
    /// A removal a process that died left claimed under a private name, finished.
    Claim,
}

/// Which rule took an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageSweepRuleV1 {
    /// Idle at least `keep_days` beyond the newest `keep_*`.
    Collection,
    /// Least recently used while af held more than `max_bytes`.
    Budget,
    /// The finish of a removal a process that died left claimed.
    Recovery,
}

/// Why the budget step stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageSweepStopV1 {
    /// The total fits the budget.
    Fits,
    /// Over the budget, and every remaining entry is in use or was used within the hour.
    NothingEvictable,
}

/// One entry the sweep removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageSweepRemovalV1 {
    pub kind: StorageSweepKindV1,
    /// The entry's absolute path; for a finished Task, its Store.
    pub path: String,
    /// The collected Task, for a `task` removal and only for one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub task_id: Option<String>,
    pub bytes: u64,
    pub rule: StorageSweepRuleV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskStorageSweepV1 {
    /// Always [`TASK_STORAGE_SWEEP_V1`].
    pub schema: String,
    pub removals: Vec<StorageSweepRemovalV1>,
    /// Removals counted but not listed: beyond [`MAX_SWEEP_REMOVALS`], or with a path no record
    /// can name (not UTF-8, a control character, longer than [`MAX_SWEEP_PATH_BYTES`]).
    pub omitted_removals: u64,
    /// Removals that failed, each bounded; the next sweep tries again.
    pub failures: Vec<String>,
    /// Failures beyond [`MAX_SWEEP_FAILURES`], counted but not listed.
    pub omitted_failures: u64,
    pub stop: StorageSweepStopV1,
    pub max_bytes: u64,
    pub total_before: u64,
    pub total_after: u64,
}

impl TaskStorageSweepV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_STORAGE_SWEEP_V1,
            "A storage sweep record needs its schema",
        )?;
        require(
            self.removals.len() <= MAX_SWEEP_REMOVALS
                && self.failures.len() <= MAX_SWEEP_FAILURES
                && safe_number(self.omitted_removals)
                && safe_number(self.omitted_failures)
                && (self.omitted_failures == 0 || self.failures.len() == MAX_SWEEP_FAILURES),
            "A storage sweep lists at most 256 removals and 64 failures and counts the rest",
        )?;
        require(
            !self.removals.is_empty() || !self.failures.is_empty() || self.omitted_removals > 0,
            "A storage sweep that removed nothing and failed nothing records nothing",
        )?;
        require(
            [self.max_bytes, self.total_before, self.total_after]
                .into_iter()
                .all(safe_number),
            "A storage sweep's totals are bounded",
        )?;
        for removal in &self.removals {
            require(
                removal.path.starts_with('/')
                    && removal.path.len() <= MAX_SWEEP_PATH_BYTES
                    && !removal.path.chars().any(char::is_control)
                    && safe_number(removal.bytes),
                "A storage sweep removal names a bounded absolute path and its bytes",
            )?;
            require(
                match (&removal.kind, &removal.task_id) {
                    (StorageSweepKindV1::Task, Some(task_id)) => is_name(task_id),
                    (StorageSweepKindV1::Task, None) => false,
                    (_, task_id) => task_id.is_none(),
                },
                "A storage sweep names a Task for a task removal, and only for one",
            )?;
        }
        for failure in &self.failures {
            require(
                !failure.trim().is_empty() && failure.len() <= MAX_SWEEP_FAILURE_BYTES,
                "A storage sweep failure is a bounded reason",
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sweep() -> TaskStorageSweepV1 {
        TaskStorageSweepV1 {
            schema: TASK_STORAGE_SWEEP_V1.into(),
            removals: vec![StorageSweepRemovalV1 {
                kind: StorageSweepKindV1::Task,
                path: "/state/af/task/local/0123456789abcdef".into(),
                task_id: Some("gc-older".into()),
                bytes: 4096,
                rule: StorageSweepRuleV1::Collection,
            }],
            omitted_removals: 0,
            failures: vec![],
            omitted_failures: 0,
            stop: StorageSweepStopV1::Fits,
            max_bytes: 1 << 30,
            total_before: 8192,
            total_after: 4096,
        }
    }

    #[test]
    fn a_sweep_record_is_bounded_and_says_something() {
        sweep().validate().unwrap();
        let empty = TaskStorageSweepV1 {
            removals: vec![],
            ..sweep()
        };
        assert!(empty.validate().is_err(), "nothing removed, nothing failed");
        let failed = TaskStorageSweepV1 {
            failures: vec!["removing /x: it changed".into()],
            ..empty
        };
        failed.validate().unwrap();
        let mut unnamed = sweep();
        unnamed.removals[0].task_id = None;
        assert!(unnamed.validate().is_err());
        let mut named = sweep();
        named.removals[0].kind = StorageSweepKindV1::Version;
        assert!(named.validate().is_err());
        let mut relative = sweep();
        relative.removals[0].path = "relative/path".into();
        assert!(relative.validate().is_err());
        let mut long = sweep();
        long.failures = vec!["x".repeat(MAX_SWEEP_FAILURE_BYTES + 1)];
        assert!(long.validate().is_err());
        let mut uncounted = sweep();
        uncounted.omitted_failures = 3;
        assert!(
            uncounted.validate().is_err(),
            "failures omitted only past the bound"
        );
        let unlisted = TaskStorageSweepV1 {
            removals: vec![],
            omitted_removals: 1,
            ..sweep()
        };
        unlisted.validate().unwrap();
    }
}
