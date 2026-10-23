//! The warm check layer's own records (ADR-0198): what a toolchain key of the Warm Check Cache
//! (ADR-0131) remembers about the sources it built, who holds it, why it was evicted, and what
//! each warm check found in it.
//!
//! - `af.warm-sources/1` sits beside a key and names, per source group of the Snapshot (a Cargo
//!   package directory, or the files no package holds), the content digest the group had and the
//!   modification time the kernel gave its files when a check last built that content into the
//!   key. A group whose digest still matches gets that time back, so Cargo keeps its build; any
//!   other group is stamped now and rebuilt. Content decides, never a modification time a
//!   materialization happened to carry (#266).
//! - `af.warm-holder/1` names the process holding a key's lock, so a waiter can say whom it
//!   waits for.
//! - `af.warm-eviction/1` is what the Storage Budget leaves when it evicts an idle key, so a
//!   check that finds its key gone names the cause.
//! - `af/TaskCheckLayer@1` is retained beside a warm check node's receipt: per check, the key,
//!   whether its lock was held at once, waited for or cloned around, whether the layer was warm
//!   at the start, how many source groups were stale, the build and test phases' durations
//!   against their deadlines, and an eviction with its cause.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{is_name, present_option, require, safe_number};
use crate::is_digest;

pub const WARM_SOURCES_V1: &str = "af.warm-sources/1";
pub const WARM_HOLDER_V1: &str = "af.warm-holder/1";
pub const WARM_EVICTION_V1: &str = "af.warm-eviction/1";
pub const TASK_CHECK_LAYER_V1: &str = "af/TaskCheckLayer@1";

/// The most source groups one record names.
pub const MAX_WARM_SOURCE_GROUPS: usize = 100_000;
/// The longest group path, in bytes.
const MAX_GROUP_PATH_BYTES: usize = 4096;
const MAX_HOST_CHARS: usize = 255;
const MAX_COMMAND_CHARS: usize = 256;

/// What one toolchain key last built, by source group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarmSourcesV1 {
    pub schema: String,
    /// By group: a Cargo package directory as a Manifest path (`crates/review-core`), or the
    /// empty string for the files no package directory holds.
    pub groups: BTreeMap<String, WarmSourceGroupV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarmSourceGroupV1 {
    /// The domain-separated digest of the group's Manifest paths, kinds and content IDs.
    pub digest: String,
    /// The modification time the kernel gave every file of the group, in nanoseconds since the
    /// Unix epoch, as a canonical decimal string: more digits than a JSON number keeps exact.
    pub stamp_unix_ns: String,
}

impl WarmSourcesV1 {
    pub fn new(groups: BTreeMap<String, WarmSourceGroupV1>) -> Self {
        Self {
            schema: WARM_SOURCES_V1.into(),
            groups,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == WARM_SOURCES_V1,
            "warm sources record has another schema",
        )?;
        require(
            self.groups.len() <= MAX_WARM_SOURCE_GROUPS,
            "warm sources record names more than 100000 groups",
        )?;
        for (group, entry) in &self.groups {
            require(
                is_group_path(group),
                "warm sources record names an invalid group path",
            )?;
            require(
                is_digest(&entry.digest) && entry.stamp().is_some(),
                "warm sources group needs a digest and a canonical nanosecond stamp",
            )?;
        }
        Ok(())
    }
}

impl WarmSourceGroupV1 {
    /// The stamp in nanoseconds, when it is a canonical, positive decimal.
    pub fn stamp(&self) -> Option<u128> {
        canonical_decimal(&self.stamp_unix_ns).filter(|stamp| *stamp > 0)
    }
}

/// `""` or a relative Manifest path: no leading or trailing `/`, no empty, `.` or `..`
/// component.
pub fn is_group_path(path: &str) -> bool {
    path.is_empty()
        || (path.len() <= MAX_GROUP_PATH_BYTES
            && !path.chars().any(char::is_control)
            && path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."))
}

fn canonical_decimal(value: &str) -> Option<u128> {
    if value.is_empty()
        || value.len() > 30
        || !value.bytes().all(|b| b.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

/// The process that holds a toolchain key's lock, written beside the key once the lock is
/// taken and removed before it is released.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarmHolderV1 {
    pub schema: String,
    pub pid: u32,
    /// The host name the holder runs on, as the host reports it.
    pub host: String,
    pub since_unix_ms: u64,
    /// What holds it, as a person would type it: `af task run`.
    pub command: String,
}

impl WarmHolderV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == WARM_HOLDER_V1
                && self.pid > 0
                && bounded_text(&self.host, MAX_HOST_CHARS)
                && bounded_text(&self.command, MAX_COMMAND_CHARS)
                && safe_number(self.since_unix_ms),
            "warm holder record needs a pid, a host, a start time and a command",
        )
    }
}

/// Who removed a key while nothing held it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarmEvictorV1 {
    /// The Storage Budget's sweep (`af storage prune --apply`, or the sweep a run starts with).
    StorageSweep,
}

/// What the Storage Budget leaves beside a key it evicted: the cause a check that finds its
/// key gone reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarmEvictionV1 {
    pub schema: String,
    pub by: WarmEvictorV1,
    pub at_unix_ms: u64,
    pub bytes: u64,
}

impl WarmEvictionV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == WARM_EVICTION_V1
                && safe_number(self.at_unix_ms)
                && safe_number(self.bytes),
            "warm eviction record needs its evictor, its time and its bytes",
        )
    }
}

/// What a warm check node observed of its layer, per check, beside its receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCheckLayerV1 {
    /// The `af/TaskCheckReceipt@1` the checks' results are recorded in.
    pub receipt_id: String,
    pub checks: BTreeMap<String, TaskCheckLayerRunV1>,
}

impl TaskCheckLayerV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.receipt_id) && !self.checks.is_empty() && self.checks.len() <= 32,
            "Check layer record requires its receipt and at most 32 named checks",
        )?;
        for (name, run) in &self.checks {
            require(is_name(name), "Check layer record names an invalid check")?;
            run.validate()
                .map_err(|error| format!("Check layer record of `{name}`: {error}"))?;
        }
        Ok(())
    }
}

/// How a check came by its layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLayerLockV1 {
    /// Its lock was free: the check held the layer at once.
    Held,
    /// Another check held it; this one waited and then held it.
    Waited,
    /// Another check held it and `[warm] clone_when_busy` is on: the check built in a private
    /// copy of the layer, with no sources record, and left the layer untouched.
    Cloned,
    /// Another check held it for the whole wait: the check built cold.
    Busy,
    /// The layer could not be opened or locked: the check built cold.
    Unavailable,
    /// The toolchain did not resolve, so there is no key: the check built cold.
    Unresolved,
}

impl TaskLayerLockV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Waited => "waited",
            Self::Cloned => "cloned",
            Self::Busy => "busy",
            Self::Unavailable => "unavailable",
            Self::Unresolved => "unresolved",
        }
    }
}

/// Whether the build the check started from held anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLayerStartV1 {
    Warm,
    Cold,
}

/// A phase of a warm check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLayerPhaseV1 {
    /// From the check's start until it reports its `build` step.
    Build,
    /// From there to its end.
    Tests,
    /// Once the check ended, before its result was accepted.
    After,
}

impl TaskLayerPhaseV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Tests => "tests",
            Self::After => "after",
        }
    }
}

/// What removed a layer a check had started from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLayerEvictorV1 {
    /// The Storage Budget's sweep, as its eviction record says.
    StorageSweep,
    /// Something else removed it while the check held it: no eviction record names a cause.
    AnotherProcess,
    /// The kernel, after the check, because the key ended above a byte bound.
    Bound,
    /// The kernel, after the check, because a directory it used was suspect.
    Suspect,
}

impl TaskLayerEvictorV1 {
    pub const fn words(self) -> &'static str {
        match self {
            Self::StorageSweep => "storage sweep",
            Self::AnotherProcess => "another process",
            Self::Bound => "the byte bound",
            Self::Suspect => "suspicion",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLayerEvictionV1 {
    pub phase: TaskLayerPhaseV1,
    pub by: TaskLayerEvictorV1,
    pub at_unix_ms: u64,
}

impl TaskLayerEvictionV1 {
    /// `layer evicted during build by storage sweep at <time>`, `<time>` already rendered.
    pub fn sentence(&self, at: &str) -> String {
        let during = match self.phase {
            TaskLayerPhaseV1::After => "after the check".to_string(),
            phase => format!("during {}", phase.as_str()),
        };
        format!("layer evicted {during} by {} at {at}", self.by.words())
    }
}

/// The holder a check waited for, as its record named it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLayerHolderV1 {
    pub pid: u32,
    pub host: String,
    pub since_unix_ms: u64,
}

/// What the content rule did with the Snapshot's source groups (ADR-0198): `stale` groups were
/// stamped now and rebuilt, `fresh` ones got back the stamp of the build that holds their
/// content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLayerSourcesV1 {
    pub stale: u32,
    pub fresh: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLayerPhasesV1 {
    /// The deadline of the build phase, `[warm] build_wall_ms`; absent when the policy declares
    /// none, and the whole check then had `check_wall_ms`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub build_wall_ms: Option<u64>,
    /// The deadline of the tests, `check_wall_ms`, counted from the build's end.
    pub check_wall_ms: u64,
    /// From the check's start until it reported its `build` step; absent when it reported none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub build_ms: Option<u64>,
    /// From the build step's report to the check's end; absent when it reported no build step.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub tests_ms: Option<u64>,
    /// The whole check.
    pub elapsed_ms: u64,
    /// The phase whose deadline ended the check, when one did.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub deadline: Option<TaskLayerPhaseV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCheckLayerRunV1 {
    /// `<project>/<toolchain>`, the key's two path components; absent when the toolchain did
    /// not resolve.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub key: Option<String>,
    pub lock: TaskLayerLockV1,
    /// How long the check waited for the lock.
    pub waited_ms: u64,
    /// Who held it while the check waited, when that holder named itself.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub holder: Option<TaskLayerHolderV1>,
    pub start: TaskLayerStartV1,
    pub sources: TaskLayerSourcesV1,
    pub phases: TaskLayerPhasesV1,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub evicted: Option<TaskLayerEvictionV1>,
}

impl TaskCheckLayerRunV1 {
    pub fn validate(&self) -> Result<(), String> {
        let keyed = matches!(
            self.lock,
            TaskLayerLockV1::Held
                | TaskLayerLockV1::Waited
                | TaskLayerLockV1::Cloned
                | TaskLayerLockV1::Busy
        );
        require(
            self.key.as_deref().map_or(!keyed, is_layer_key)
                && (self.lock != TaskLayerLockV1::Unresolved || self.key.is_none()),
            "a layer that was locked, waited for, cloned or busy names its key, and only then",
        )?;
        require(
            self.holder.is_none()
                || matches!(
                    self.lock,
                    TaskLayerLockV1::Waited | TaskLayerLockV1::Cloned | TaskLayerLockV1::Busy
                ),
            "only a check that met another holder names it",
        )?;
        if let Some(holder) = &self.holder {
            require(
                holder.pid > 0
                    && bounded_text(&holder.host, MAX_HOST_CHARS)
                    && safe_number(holder.since_unix_ms),
                "a holder needs its pid, its host and its start time",
            )?;
        }
        let phases = &self.phases;
        require(
            safe_number(self.waited_ms)
                && [phases.check_wall_ms, phases.elapsed_ms]
                    .into_iter()
                    .chain(phases.build_wall_ms)
                    .chain(phases.build_ms)
                    .chain(phases.tests_ms)
                    .all(safe_number)
                && phases.build_wall_ms != Some(0)
                && (phases.deadline.is_none() || phases.build_wall_ms.is_some())
                && phases.check_wall_ms > 0
                && (phases.tests_ms.is_none() || phases.build_ms.is_some())
                && phases.deadline != Some(TaskLayerPhaseV1::After)
                && (phases.deadline != Some(TaskLayerPhaseV1::Tests) || phases.build_ms.is_some()),
            "phases need positive deadlines, tests only after a build, and a deadline of the \
             build or the tests, which only a declared build deadline sets",
        )?;
        if let Some(evicted) = &self.evicted {
            require(
                safe_number(evicted.at_unix_ms)
                    && (evicted.phase == TaskLayerPhaseV1::After)
                        == matches!(
                            evicted.by,
                            TaskLayerEvictorV1::Bound | TaskLayerEvictorV1::Suspect
                        ),
                "the kernel evicts only after a check; anything else evicted it during one",
            )?;
        }
        Ok(())
    }
}

/// `<64 hex>/<64 hex>`: a project key and a toolchain key.
pub fn is_layer_key(value: &str) -> bool {
    value.split_once('/').is_some_and(|(project, toolchain)| {
        [project, toolchain].iter().all(|part| {
            part.len() == 64 && part.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
    })
}

fn bounded_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(c: char, stamp: &str) -> WarmSourceGroupV1 {
        WarmSourceGroupV1 {
            digest: format!("sha256:{}", c.to_string().repeat(64)),
            stamp_unix_ns: stamp.into(),
        }
    }

    #[test]
    fn a_sources_record_names_groups_by_path_with_a_digest_and_an_exact_stamp() {
        let record = WarmSourcesV1::new(BTreeMap::from([
            (String::new(), group('a', "1761210000123456789")),
            ("crates/x".into(), group('b', "1")),
        ]));
        record.validate().unwrap();
        assert_eq!(record.groups[""].stamp(), Some(1_761_210_000_123_456_789));
        for (path, stamp) in [
            ("/abs", "1"),
            ("a//b", "1"),
            ("a/../b", "1"),
            ("ok", "0"),
            ("ok", "01"),
            ("ok", "1.5"),
            ("ok", ""),
        ] {
            let bad = WarmSourcesV1::new(BTreeMap::from([(path.to_string(), group('c', stamp))]));
            assert!(bad.validate().is_err(), "{path:?} {stamp:?}");
        }
    }

    #[test]
    fn a_layer_run_names_its_key_exactly_when_it_had_one() {
        let key = format!("{}/{}", "a".repeat(64), "b".repeat(64));
        let mut run = TaskCheckLayerRunV1 {
            key: Some(key.clone()),
            lock: TaskLayerLockV1::Waited,
            waited_ms: 61_000,
            holder: Some(TaskLayerHolderV1 {
                pid: 4242,
                host: "claw-3".into(),
                since_unix_ms: 1,
            }),
            start: TaskLayerStartV1::Warm,
            sources: TaskLayerSourcesV1 { stale: 1, fresh: 3 },
            phases: TaskLayerPhasesV1 {
                build_wall_ms: Some(2_400_000),
                check_wall_ms: 3_600_000,
                build_ms: Some(10),
                tests_ms: Some(20),
                elapsed_ms: 30,
                deadline: None,
            },
            evicted: None,
        };
        run.validate().unwrap();
        run.lock = TaskLayerLockV1::Unresolved;
        assert!(
            run.validate().is_err(),
            "an unresolved toolchain has no key"
        );
        run.key = None;
        assert!(
            run.validate().is_err(),
            "only a check that met a holder names it"
        );
        run.holder = None;
        run.validate().unwrap();
        run.lock = TaskLayerLockV1::Held;
        assert!(run.validate().is_err(), "a held layer names its key");
        run.key = Some(key);
        run.phases.build_ms = None;
        assert!(run.validate().is_err(), "tests need a build before them");
        run.phases.tests_ms = None;
        run.evicted = Some(TaskLayerEvictionV1 {
            phase: TaskLayerPhaseV1::Build,
            by: TaskLayerEvictorV1::StorageSweep,
            at_unix_ms: 5,
        });
        run.validate().unwrap();
        assert_eq!(
            run.evicted.as_ref().unwrap().sentence("12:00:00 UTC"),
            "layer evicted during build by storage sweep at 12:00:00 UTC"
        );
        run.evicted.as_mut().unwrap().by = TaskLayerEvictorV1::Bound;
        assert!(run.validate().is_err(), "the kernel never evicts mid-check");
    }

    #[test]
    fn holder_and_eviction_records_are_closed() {
        let holder = WarmHolderV1 {
            schema: WARM_HOLDER_V1.into(),
            pid: 7,
            host: "host".into(),
            since_unix_ms: 1,
            command: "af task run".into(),
        };
        holder.validate().unwrap();
        assert!(
            WarmHolderV1 {
                host: "a\nb".into(),
                ..holder
            }
            .validate()
            .is_err()
        );
        WarmEvictionV1 {
            schema: WARM_EVICTION_V1.into(),
            by: WarmEvictorV1::StorageSweep,
            at_unix_ms: 1,
            bytes: 2,
        }
        .validate()
        .unwrap();
    }
}
