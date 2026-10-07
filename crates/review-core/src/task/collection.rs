//! The tombstone of a collected Task (ADR-0135).
//!
//! `af task gc --apply` appends one `task_collected` transition to a finished Task's log,
//! carrying this record inline. It retains the summary `af task list` and `af task show` print
//! once the Task's artifacts may be gone, and it references no artifact: the event's
//! `artifact_refs` are empty and the identities below are names, never reachability roots. The
//! Task's earlier events stay in the log; its projection stops at the tombstone.

use serde::{Deserialize, Serialize};

use super::{is_name, is_package_name, require, safe_number};
use crate::is_digest;

pub const TASK_COLLECTED_V1: &str = "af/TaskCollected@1";

/// What remains readable of a collected Task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCollectedV1 {
    /// Always [`TASK_COLLECTED_V1`].
    pub schema: String,
    pub task_id: String,
    /// The Task kind its revision named.
    pub kind: String,
    /// The Task revision current when it was collected.
    pub revision_id: String,
    /// The finished result's domain conclusion.
    pub outcome: String,
    /// Committed chargeable tokens, as decimal text (`af task list` spells them so).
    pub chargeable_tokens: String,
    /// Settled Attempts whose usage was unknown when the Task was collected: no usage was
    /// reported, so each was charged zero (ADR-0143) and `chargeable_tokens` is not their spend.
    /// Absent when none.
    #[serde(default, skip_serializing_if = "super::is_zero")]
    pub unknown_usage_attempts: u64,
    /// Policy time of the Task's last event before the tombstone.
    pub last_event_unix_ms: u64,
    /// Policy time the tombstone was written.
    pub collected_unix_ms: u64,
    /// Bytes of the CAS objects this Task reached and no retained record reaches, as the
    /// preview computed them before the tombstone was written. Objects several collected Tasks
    /// share are counted in each of them.
    pub collected_bytes: u64,
}

impl TaskCollectedV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_COLLECTED_V1
                && is_name(&self.task_id)
                && is_package_name(&self.kind)
                && is_digest(&self.revision_id),
            "A Task tombstone needs its schema, Task ID, kind and revision",
        )?;
        require(
            !self.outcome.trim().is_empty() && self.outcome.chars().count() <= 256,
            "A Task tombstone needs a bounded outcome",
        )?;
        require(
            is_decimal(&self.chargeable_tokens),
            "A Task tombstone spells its chargeable tokens as canonical decimal text",
        )?;
        require(
            self.last_event_unix_ms > 0
                && safe_number(self.last_event_unix_ms)
                && safe_number(self.collected_unix_ms)
                && self.collected_unix_ms >= self.last_event_unix_ms
                && safe_number(self.collected_bytes),
            "A Task tombstone needs bounded times, collected no earlier than its last event",
        )?;
        require(
            safe_number(self.unknown_usage_attempts),
            "A Task tombstone counts its unknown-usage Attempts within the safe integer range",
        )
    }
}

/// A collection time as `af task list`, `af task show` and every refusal spell it: RFC 3339 UTC
/// to the second, as in `2026-09-28T12:00:00Z`.
pub fn collected_time(unix_ms: u64) -> String {
    let seconds = unix_ms / 1_000;
    let days = seconds / 86_400;
    let (year, month, day) = civil_from_days(days);
    let of_day = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3_600,
        of_day / 60 % 60,
        of_day % 60
    )
}

/// Days since 1970-01-01 as a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

/// Canonical unsigned decimal text within `u128`: no sign, no leading zero, no exponent.
fn is_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
        && value.parse::<u128>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tombstone() -> TaskCollectedV1 {
        TaskCollectedV1 {
            schema: TASK_COLLECTED_V1.into(),
            task_id: "older".into(),
            kind: "implement".into(),
            revision_id: format!("sha256:{}", "a".repeat(64)),
            outcome: "verified".into(),
            chargeable_tokens: "1200".into(),
            unknown_usage_attempts: 1,
            last_event_unix_ms: 10,
            collected_unix_ms: 20,
            collected_bytes: 4096,
        }
    }

    #[test]
    fn a_tombstone_keeps_a_bounded_summary() {
        tombstone().validate().unwrap();
        let refused = [
            TaskCollectedV1 {
                schema: "af/TaskCollected@2".into(),
                ..tombstone()
            },
            TaskCollectedV1 {
                task_id: "-older".into(),
                ..tombstone()
            },
            TaskCollectedV1 {
                revision_id: "not-a-digest".into(),
                ..tombstone()
            },
            TaskCollectedV1 {
                outcome: " ".into(),
                ..tombstone()
            },
            TaskCollectedV1 {
                chargeable_tokens: "012".into(),
                ..tombstone()
            },
            TaskCollectedV1 {
                chargeable_tokens: "-1".into(),
                ..tombstone()
            },
            TaskCollectedV1 {
                collected_unix_ms: 9,
                ..tombstone()
            },
            TaskCollectedV1 {
                last_event_unix_ms: 0,
                collected_unix_ms: 0,
                ..tombstone()
            },
            TaskCollectedV1 {
                unknown_usage_attempts: 1 << 53,
                ..tombstone()
            },
        ];
        for value in refused {
            assert!(value.validate().is_err(), "{value:?}");
        }
    }

    #[test]
    fn collection_times_are_rfc3339_utc_seconds() {
        assert_eq!(collected_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(collected_time(951_782_400_000), "2000-02-29T00:00:00Z");
        assert_eq!(collected_time(1_790_599_874_258), "2026-09-28T12:51:14Z");
        assert_eq!(collected_time(4_102_444_799_999), "2099-12-31T23:59:59Z");
    }

    /// The unknown-usage count is spelled only when there is one, so a tombstone without
    /// unknown usage keeps the bytes it had before the field existed.
    #[test]
    fn a_tombstone_spells_unknown_usage_only_when_some_attempt_had_it() {
        let value = serde_json::to_value(tombstone()).unwrap();
        assert_eq!(value["unknown_usage_attempts"], 1);
        let none = TaskCollectedV1 {
            unknown_usage_attempts: 0,
            ..tombstone()
        };
        let value = serde_json::to_value(&none).unwrap();
        assert!(value.get("unknown_usage_attempts").is_none(), "{value}");
        assert_eq!(
            serde_json::from_value::<TaskCollectedV1>(value).unwrap(),
            none
        );
    }

    #[test]
    fn a_tombstone_refuses_unknown_fields() {
        let mut value = serde_json::to_value(tombstone()).unwrap();
        value["result_id"] = serde_json::json!(format!("sha256:{}", "b".repeat(64)));
        assert!(serde_json::from_value::<TaskCollectedV1>(value).is_err());
    }
}
