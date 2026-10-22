//! `af/TaskProviderEvent@1` (ADR-0196): what one started Attempt did about its Provider without
//! failing — a wait before it calls again after a transient failure, a re-ask of a typed reply
//! that did not parse, an identity recheck that probed again or found only a CLI update, an
//! admission probe sent again in its alternate wording. It is its own event kind of the Task's
//! log, `TaskProviderEvent@1`, beside `TaskTransition@5` and `TaskExecutionStarted@1`, whose
//! shapes it leaves unchanged (ADR-0145). It names the lease that wrote it, the Attempt and its
//! node; it references no artifact, never changes a charge, a reservation, a deadline or the
//! Task's result, and the Store refuses it for an Attempt that is not started and unsettled.

use serde::{Deserialize, Serialize};

use super::{is_name, require, safe_number};

pub const TASK_PROVIDER_EVENT_V1: &str = "af/TaskProviderEvent@1";

/// The most retries one Attempt makes after transient Provider failures.
pub const MAX_PROVIDER_RETRIES: u32 = 3;

/// A Provider failure the same input may get past after a wait (ADR-0196).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransientClassV1 {
    /// The request did not reach the Provider or its answer did not come back.
    Network,
    /// The Provider said the model is at capacity or overloaded.
    Capacity,
    /// The native process or a probe ran out of its own time.
    Timeout,
    /// The native process exited non-zero with no output and no parsable result.
    ExitedWithoutOutput,
}

impl TransientClassV1 {
    pub const ALL: [Self; 4] = [
        Self::Network,
        Self::Capacity,
        Self::Timeout,
        Self::ExitedWithoutOutput,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Capacity => "capacity",
            Self::Timeout => "timeout",
            Self::ExitedWithoutOutput => "exited_without_output",
        }
    }

    /// The words `af task show` uses: `waiting 120 s for capacity`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Capacity => "capacity",
            Self::Timeout => "timeout",
            Self::ExitedWithoutOutput => "a native exit without output",
        }
    }
}

/// Why an identity probe proved nothing either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeSilenceV1 {
    /// The probe did not answer within its own wall.
    TimedOut,
    /// The probe answered with a line af cannot read.
    Malformed,
}

impl ProbeSilenceV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TimedOut => "timed out",
            Self::Malformed => "answered malformed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderEventV1 {
    /// A transient failure: the Attempt waits `wait_ms` before call `retry + 1`. The wait counts
    /// against the Attempt's own wall; it never extends a deadline or resets a reservation.
    RetryWait {
        class: TransientClassV1,
        retry: u32,
        max_retries: u32,
        wait_ms: u64,
        /// The wait is the one the Provider asked for (`Retry-After`), not af's backoff.
        retry_after: bool,
    },
    /// The typed reply did not parse: the same Worker is asked once more in the same Attempt,
    /// with the parse error and the offending excerpt. Both digests name CAS blobs the Attempt
    /// keeps as raw evidence: the reply and the re-ask request.
    ReplyReasked {
        raw_reply_id: String,
        reask_id: String,
    },
    /// The identity probe before a send proved nothing, so it was sent again.
    IdentityReprobed { cause: ProbeSilenceV1, probe: u32 },
    /// The Provider's CLI reports another version than at capture while the account, its
    /// authentication method and its subscription are unchanged: the capture follows it.
    IdentityRefreshed {
        provider: String,
        previous_version: String,
        current_version: String,
    },
    /// The admission probe was refused by the bound model's safety classifier, so the alternate
    /// probe wording was sent once. `category` is the refusal category the stream named.
    AdmissionReprobed {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "super::present_option"
        )]
        category: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProviderEventV1 {
    /// Always [`TASK_PROVIDER_EVENT_V1`].
    pub schema: String,
    /// The lease that wrote it, judged against the Task's current lease as a transition's is.
    pub writer: String,
    pub epoch: u64,
    pub now_unix_ms: u64,
    /// The started, unsettled Attempt it concerns, and that Attempt's node.
    pub attempt_id: String,
    pub node: String,
    pub event: ProviderEventV1,
}

/// A short printable word: a version, a Provider ID, a refusal category.
fn word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|c| c.is_ascii_graphic() || c == ' ')
}

impl TaskProviderEventV1 {
    pub fn new(
        writer: impl Into<String>,
        epoch: u64,
        now_unix_ms: u64,
        attempt_id: impl Into<String>,
        node: impl Into<String>,
        event: ProviderEventV1,
    ) -> Self {
        Self {
            schema: TASK_PROVIDER_EVENT_V1.into(),
            writer: writer.into(),
            epoch,
            now_unix_ms,
            attempt_id: attempt_id.into(),
            node: node.into(),
            event,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_PROVIDER_EVENT_V1,
            "A Provider event needs its schema",
        )?;
        require(
            is_name(&self.writer)
                && self.epoch > 0
                && safe_number(self.epoch)
                && self.now_unix_ms > 0
                && safe_number(self.now_unix_ms),
            "A Provider event needs a writer, epoch and bounded policy time",
        )?;
        require(
            self.attempt_id.len() == 26
                && self.attempt_id.bytes().all(|b| b.is_ascii_alphanumeric())
                && self.node.split('.').all(is_name),
            "A Provider event needs its exact Attempt and qualified node",
        )?;
        match &self.event {
            ProviderEventV1::RetryWait {
                retry,
                max_retries,
                wait_ms,
                ..
            } => require(
                (1..=MAX_PROVIDER_RETRIES).contains(max_retries)
                    && (1..=*max_retries).contains(retry)
                    && *wait_ms > 0
                    && safe_number(*wait_ms),
                "A Provider retry wait needs a bounded retry number and a positive wait",
            ),
            ProviderEventV1::ReplyReasked {
                raw_reply_id,
                reask_id,
            } => require(
                crate::is_digest(raw_reply_id)
                    && crate::is_digest(reask_id)
                    && raw_reply_id != reask_id,
                "A reply re-ask names the reply and the re-ask request it kept",
            ),
            ProviderEventV1::IdentityReprobed { probe, .. } => require(
                (2..=3).contains(probe),
                "An identity re-probe is the second or third probe",
            ),
            ProviderEventV1::IdentityRefreshed {
                provider,
                previous_version,
                current_version,
            } => require(
                is_name(provider)
                    && word(previous_version)
                    && word(current_version)
                    && previous_version != current_version,
                "An identity refresh names its Provider and two different CLI versions",
            ),
            ProviderEventV1::AdmissionReprobed { category } => require(
                category.as_deref().is_none_or(word),
                "An admission re-probe names at most a short refusal category",
            ),
        }
    }

    /// The line `af task show` prints for this event while its Attempt runs.
    pub fn describe(&self) -> String {
        match &self.event {
            ProviderEventV1::RetryWait {
                class,
                retry,
                max_retries,
                wait_ms,
                ..
            } => format!(
                "waiting {} s for {} (retry {retry} of {max_retries})",
                wait_ms.div_ceil(1000),
                class.label()
            ),
            ProviderEventV1::ReplyReasked { raw_reply_id, .. } => format!(
                "re-asked the Worker once for a typed reply that did not parse; raw reply kept: \
                 {raw_reply_id}"
            ),
            ProviderEventV1::IdentityReprobed { cause, probe } => format!(
                "identity probe {} and was sent again (probe {probe} of 3)",
                cause.as_str()
            ),
            ProviderEventV1::IdentityRefreshed {
                provider,
                previous_version,
                current_version,
            } => format!(
                "Provider {provider} CLI updated from {previous_version} to {current_version}; \
                 the same account continues"
            ),
            ProviderEventV1::AdmissionReprobed { category } => match category {
                Some(category) => {
                    format!("admission probe refused ({category}); sent the alternate probe once")
                }
                None => "admission probe refused; sent the alternate probe once".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event: ProviderEventV1) -> TaskProviderEventV1 {
        TaskProviderEventV1::new(
            "cli-1",
            1,
            10,
            "01jd8m4qz9k7v3n2p6r8t0w200",
            "root.nodes.review",
            event,
        )
    }

    #[test]
    fn a_retry_wait_reads_as_the_show_line_and_bounds_its_numbers() {
        let wait = event(ProviderEventV1::RetryWait {
            class: TransientClassV1::Capacity,
            retry: 2,
            max_retries: 3,
            wait_ms: 120_000,
            retry_after: false,
        });
        wait.validate().unwrap();
        assert_eq!(wait.describe(), "waiting 120 s for capacity (retry 2 of 3)");
        for (retry, max, ms) in [(0, 3, 1), (4, 3, 1), (1, 4, 1), (1, 3, 0)] {
            let bad = event(ProviderEventV1::RetryWait {
                class: TransientClassV1::Network,
                retry,
                max_retries: max,
                wait_ms: ms,
                retry_after: false,
            });
            assert!(bad.validate().is_err(), "{retry} {max} {ms}");
        }
    }

    #[test]
    fn identity_and_admission_events_bound_what_they_name() {
        let refreshed = event(ProviderEventV1::IdentityRefreshed {
            provider: "claude-main".into(),
            previous_version: "2.1.299 (Claude Code)".into(),
            current_version: "2.1.300 (Claude Code)".into(),
        });
        refreshed.validate().unwrap();
        let same = event(ProviderEventV1::IdentityRefreshed {
            provider: "claude-main".into(),
            previous_version: "2.1.299".into(),
            current_version: "2.1.299".into(),
        });
        assert!(same.validate().is_err());
        let control = event(ProviderEventV1::AdmissionReprobed {
            category: Some("cyber\nharm".into()),
        });
        assert!(control.validate().is_err());
        assert!(
            event(ProviderEventV1::IdentityReprobed {
                cause: ProbeSilenceV1::TimedOut,
                probe: 1,
            })
            .validate()
            .is_err()
        );
    }
}
