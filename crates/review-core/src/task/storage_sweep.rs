//! The record `af/TaskStorageSweep@2` (ADR-0144, ADR-0158 clause 13): what the Storage Budget's
//! sweep at the end of `af task run` removed and failed to remove, as an observation of the Task
//! that run executed. It travels inline in one `storage_sweep` transition of the Task's log,
//! references no artifact and never changes the Task's result. It is bounded like a gate
//! cleanup: at most [`MAX_SWEEP_REMOVALS`] removals and [`MAX_SWEEP_FAILURES`] failures are
//! listed, each bounded, and the rest only counted. A sweep that removed nothing and failed
//! nothing records nothing; every other one is recorded, a sweep whose total after it is unknown
//! included (a removal stopped part way and what it left could not be measured): its
//! `total_after` is then `unknown` and its `stop` `unknown`, never a number or `fits` af cannot
//! vouch for, and `freed_bytes` still says what left the disk.
//!
//! Version 2 is the first that can say `unknown` and that carries `freed_bytes`; the superseded
//! `af/TaskStorageSweep@1`, whose totals were numbers only, is refused by type (ADR-0145) with
//! [`SUPERSEDED_TASK_STORAGE_SWEEP`], so a Task log holding one does not replay.

use serde::{Deserialize, Serialize};

use super::{is_name, present_option, require, safe_number};

pub const TASK_STORAGE_SWEEP_V2: &str = "af/TaskStorageSweep@2";

/// The superseded record type, whose totals were numbers only. Refused by type (ADR-0145):
/// never read, never migrated.
pub const TASK_STORAGE_SWEEP_V1_SUPERSEDED: &str = "af/TaskStorageSweep@1";

/// How a record of the superseded type is refused.
pub const SUPERSEDED_TASK_STORAGE_SWEEP: &str = "af/TaskStorageSweep@1 is a superseded version \
     of the Task storage sweep record; another af release wrote it";

/// How a total af cannot vouch for is spelled.
pub const UNKNOWN_TOTAL: &str = "unknown";

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
pub enum StorageSweepStopV2 {
    /// The total fits the budget.
    Fits,
    /// Over the budget, and every remaining entry is in use or was used within the hour.
    NothingEvictable,
    /// The total after the sweep is unknown: a removal stopped part way and what it left could
    /// not be measured, so neither of the others is claimed.
    Unknown,
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
pub struct TaskStorageSweepV2 {
    /// Always [`TASK_STORAGE_SWEEP_V2`]; [`TASK_STORAGE_SWEEP_V1_SUPERSEDED`] is refused.
    #[serde(deserialize_with = "sweep_schema")]
    pub schema: String,
    pub removals: Vec<StorageSweepRemovalV1>,
    /// Removals counted but not listed: beyond [`MAX_SWEEP_REMOVALS`], with a path no record
    /// can name (not UTF-8, a control character, longer than [`MAX_SWEEP_PATH_BYTES`]), or of a
    /// kind the record has none for, a Survey's (ADR-0158 clause 13).
    pub omitted_removals: u64,
    /// Removals that failed, each bounded; the next sweep tries again.
    pub failures: Vec<String>,
    /// Failures beyond [`MAX_SWEEP_FAILURES`], counted but not listed.
    pub omitted_failures: u64,
    /// The allocated bytes that left the disk in this sweep: every removal's, listed or not, and
    /// what a removal that failed part way freed before it stopped, as far as it was measured.
    pub freed_bytes: u64,
    pub stop: StorageSweepStopV2,
    pub max_bytes: u64,
    pub total_before: u64,
    /// af's total after the sweep; `None`, spelled [`UNKNOWN_TOTAL`], exactly when `stop` is
    /// [`StorageSweepStopV2::Unknown`].
    #[serde(with = "known_total")]
    pub total_after: Option<u64>,
}

/// The `schema` of a sweep record, deserialized only as [`TASK_STORAGE_SWEEP_V2`].
fn sweep_schema<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    use serde::de::Error as _;
    let schema = String::deserialize(deserializer)?;
    match schema.as_str() {
        TASK_STORAGE_SWEEP_V2 => Ok(schema),
        TASK_STORAGE_SWEEP_V1_SUPERSEDED => Err(D::Error::custom(SUPERSEDED_TASK_STORAGE_SWEEP)),
        _ => Err(D::Error::custom(format!(
            "a Task storage sweep record is {TASK_STORAGE_SWEEP_V2}, not `{schema}`"
        ))),
    }
}

/// A total as the record spells it: its bytes, or [`UNKNOWN_TOTAL`]; nothing else.
mod known_total {
    use serde::{Deserialize, Deserializer, Serializer};

    use super::UNKNOWN_TOTAL;

    pub(super) fn serialize<S: Serializer>(
        total: &Option<u64>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match total {
            Some(bytes) => serializer.serialize_u64(*bytes),
            None => serializer.serialize_str(UNKNOWN_TOTAL),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u64>, D::Error> {
        use serde::de::Error as _;
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Spelled {
            Bytes(u64),
            Word(String),
        }
        match Spelled::deserialize(deserializer)? {
            Spelled::Bytes(bytes) => Ok(Some(bytes)),
            Spelled::Word(word) if word == UNKNOWN_TOTAL => Ok(None),
            Spelled::Word(word) => Err(D::Error::custom(format!(
                "a total is its bytes or `{UNKNOWN_TOTAL}`, not `{word}`"
            ))),
        }
    }
}

impl TaskStorageSweepV2 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_STORAGE_SWEEP_V2,
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
            [self.max_bytes, self.total_before, self.freed_bytes]
                .into_iter()
                .chain(self.total_after)
                .all(safe_number),
            "A storage sweep's totals are bounded",
        )?;
        require(
            self.total_after.is_none() == (self.stop == StorageSweepStopV2::Unknown),
            "A storage sweep's total after it is unknown exactly when its stop is unknown",
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
        require(
            self.removals
                .iter()
                .try_fold(0_u64, |sum, removal| sum.checked_add(removal.bytes))
                .is_some_and(|listed| listed <= self.freed_bytes),
            "A storage sweep's freed bytes hold at least every listed removal's",
        )?;
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

    fn sweep() -> TaskStorageSweepV2 {
        TaskStorageSweepV2 {
            schema: TASK_STORAGE_SWEEP_V2.into(),
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
            freed_bytes: 4096,
            stop: StorageSweepStopV2::Fits,
            max_bytes: 1 << 30,
            total_before: 8192,
            total_after: Some(4096),
        }
    }

    #[test]
    fn a_sweep_record_is_bounded_and_says_something() {
        sweep().validate().unwrap();
        let empty = TaskStorageSweepV2 {
            removals: vec![],
            ..sweep()
        };
        assert!(empty.validate().is_err(), "nothing removed, nothing failed");
        let failed = TaskStorageSweepV2 {
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
        let unlisted = TaskStorageSweepV2 {
            removals: vec![],
            omitted_removals: 1,
            ..sweep()
        };
        unlisted.validate().unwrap();
        let unfreed = TaskStorageSweepV2 {
            freed_bytes: 4095,
            ..sweep()
        };
        assert!(
            unfreed.validate().is_err(),
            "the listed removals' bytes left the disk"
        );
    }

    /// A partial removal whose remainder cannot be measured: the record says what left and
    /// what failed, its total after `unknown` and its stop `unknown`, together or not at all.
    #[test]
    fn an_unknown_total_is_spelled_unknown_with_an_unknown_stop() {
        let unknown = TaskStorageSweepV2 {
            failures: vec!["removing /x: only partly removed".into()],
            freed_bytes: 4096 + 512,
            stop: StorageSweepStopV2::Unknown,
            total_after: None,
            ..sweep()
        };
        unknown.validate().unwrap();
        let value = serde_json::to_value(&unknown).unwrap();
        assert_eq!(value["total_after"], UNKNOWN_TOTAL);
        assert_eq!(value["stop"], "unknown");
        assert_eq!(value["freed_bytes"], 4608);
        let back: TaskStorageSweepV2 = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(back, unknown);
        for (stop, total_after) in [
            (StorageSweepStopV2::Fits, None),
            (StorageSweepStopV2::NothingEvictable, None),
            (StorageSweepStopV2::Unknown, Some(0)),
        ] {
            let mismatched = TaskStorageSweepV2 {
                stop,
                total_after,
                ..unknown.clone()
            };
            assert!(mismatched.validate().is_err(), "{stop:?} {total_after:?}");
        }
        let mut word = value.clone();
        word["total_after"] = serde_json::json!("lots");
        assert!(serde_json::from_value::<TaskStorageSweepV2>(word).is_err());
    }

    #[test]
    fn the_superseded_version_is_refused_by_type() {
        let mut value = serde_json::to_value(sweep()).unwrap();
        value["schema"] = serde_json::json!(TASK_STORAGE_SWEEP_V1_SUPERSEDED);
        let error = serde_json::from_value::<TaskStorageSweepV2>(value.clone())
            .unwrap_err()
            .to_string();
        assert!(error.contains(SUPERSEDED_TASK_STORAGE_SWEEP), "{error}");
        // Its shape as well: no `freed_bytes`, numbers only.
        value.as_object_mut().unwrap().remove("freed_bytes");
        assert!(serde_json::from_value::<TaskStorageSweepV2>(value).is_err());
    }
}
