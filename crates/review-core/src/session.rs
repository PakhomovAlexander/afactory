//! Worker warm layers, package P4: the Session Snapshot and Cold Closeout.
//!
//! A Session Snapshot is the content-addressed transcript of one Attempt's harness session. The
//! kernel assigns the session identity from the Attempt ID before the harness starts, captures
//! the transcript at seal, and re-materializes it for a *forked* resume, so the captured bytes
//! are never mutated. Capture is a two-phase protocol under the Attempt epoch: capture and
//! verify the bounded bytes, append [`SessionSnapshotPreparedPayloadV1`] naming the CAS object
//! and the exact source identity, delete the transcript from the harness directory without
//! following a symlink, then append [`SessionSnapshotCleanedPayloadV1`]. Recovery finishes the
//! deletion from the prepared record alone, without a provider call, and Warm Set selection
//! requires both the Attempt's admission and its completed cleanup — so a crash between the
//! phases leaves neither an orphaned CAS object nor an ambient transcript.
//!
//! Nothing here names a host path. The source identity is the provider kind plus the digest of
//! the harness path, exactly as ADR-0109 keeps workspace roots out of durable records.

use serde::{Deserialize, Serialize};

/// The six hex digits appended to an Attempt ID to make a session identity. Fixed, so the
/// derivation is injective, reproducible without a hash, and visibly carries the Attempt ID a
/// recovering kernel must look for.
const SESSION_ID_SUFFIX: &str = "5e5510";

/// Length of the Attempt ID a session identity is derived from: the 26 lowercase hex digits of
/// a Round-scoped Attempt.
const ATTEMPT_HEX_LEN: usize = 26;

/// Hard upper bound for one captured transcript. A larger session is never captured, so a
/// runaway harness file cannot become resident allocation or an unbounded CAS object.
pub const MAX_SESSION_TRANSCRIPT_BYTES: u64 = 32 * 1024 * 1024;

/// Default `warm.session.max_age` in seconds: a previous Attempt older than this is refused,
/// because no provider prompt cache can plausibly still serve it.
pub const DEFAULT_SESSION_MAX_AGE_SECS: u64 = 60 * 60;

/// Hard upper bound for `warm.session.max_age`. Policy may lower it, never raise it.
pub const MAX_SESSION_MAX_AGE_SECS: u64 = 24 * 60 * 60;

/// Whether `value` is a kernel-assigned session identity: the canonical 8-4-4-4-12 lowercase
/// hex shape the pinned Claude CLI accepts for `--session-id`.
pub fn is_session_id(value: &str) -> bool {
    let groups = [8usize, 4, 4, 4, 12];
    let mut parts = value.split('-');
    for expected in groups {
        let Some(part) = parts.next() else {
            return false;
        };
        if part.len() != expected
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return false;
        }
    }
    parts.next().is_none()
}

fn is_attempt_hex(value: &str) -> bool {
    value.len() == ATTEMPT_HEX_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The session identity one Attempt runs under: its Round-scoped Attempt ID followed by a fixed
/// suffix, rendered in the canonical identifier shape the pinned CLI accepts for `--session-id`.
/// Derived and never random, so replay names the same session and a kernel that crashed after
/// capture can still find the exact transcript it owes a deletion.
///
/// `None` for an Attempt ID outside the Round-scoped hex shape: such a node simply runs cold.
pub fn session_id_for_attempt(attempt_id: &str) -> Option<String> {
    if !is_attempt_hex(attempt_id) {
        return None;
    }
    let hex = format!("{attempt_id}{SESSION_ID_SUFFIX}");
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// The Attempt a session identity was derived from, or `None` when the identity was not derived
/// by this kernel. Recovery reads it from the prepared record rather than from a live process.
pub fn attempt_of_session_id(session_id: &str) -> Option<String> {
    if !is_session_id(session_id) {
        return None;
    }
    let hex: String = session_id.chars().filter(|c| *c != '-').collect();
    let (attempt, suffix) = hex.split_at(ATTEMPT_HEX_LEN);
    (suffix == SESSION_ID_SUFFIX).then(|| attempt.to_string())
}

fn is_provider_kind(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The exact source one transcript was captured from: the adapter kind that wrote it and the
/// content identity of its harness path, which the adapter that owns the path computes with the
/// store's blob identity. Never the path itself, so no durable record says where the operator's
/// harness directory lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSourceV1 {
    pub provider_kind: String,
    pub path_digest: String,
}

impl SessionSourceV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_provider_kind(&self.provider_kind) {
            return Err("SessionSnapshot@1 has an invalid provider kind".into());
        }
        if !crate::is_digest(&self.path_digest) {
            return Err("SessionSnapshot@1 has an invalid source path digest".into());
        }
        Ok(())
    }
}

/// The payload of a `review.kernel/SessionSnapshot@1` artifact: what one Attempt's harness
/// session was, and the CAS object holding its exact bytes. Node-private by construction: the
/// node and Attempt it names are the only ones that may resume it, and only forked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSnapshotV1 {
    pub node: String,
    pub attempt_id: String,
    pub session_id: String,
    pub head_snapshot_id: String,
    pub source: SessionSourceV1,
    /// The raw transcript bytes in the CAS. Re-materialized into the next Attempt's harness
    /// directory before a forked resume; never mutated in place.
    pub transcript_artifact_id: String,
    pub bytes: u64,
    /// What the transcript is expected to cost as input when the session is resumed. Gated
    /// against the next Attempt's reservation beside the delta prompt.
    pub estimated_tokens: u64,
}

impl SessionSnapshotV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.node.trim().is_empty() {
            return Err("SessionSnapshot@1 has an empty node".into());
        }
        if !crate::warm::is_monotonic_id(&self.attempt_id) {
            return Err("SessionSnapshot@1 has an invalid Attempt ID".into());
        }
        if !is_session_id(&self.session_id) {
            return Err("SessionSnapshot@1 has an invalid session identity".into());
        }
        if attempt_of_session_id(&self.session_id).as_deref() != Some(self.attempt_id.as_str()) {
            return Err(
                "SessionSnapshot@1 session identity is not derived from its Attempt".into(),
            );
        }
        if !crate::is_digest(&self.head_snapshot_id)
            || !crate::is_digest(&self.transcript_artifact_id)
        {
            return Err("SessionSnapshot@1 has an invalid Snapshot or transcript ID".into());
        }
        self.source.validate()?;
        if self.bytes == 0 || self.bytes > MAX_SESSION_TRANSCRIPT_BYTES {
            return Err("SessionSnapshot@1 bytes are empty or over the capture bound".into());
        }
        if self.estimated_tokens > crate::json::SAFE_INTEGER_MAX as u64 {
            return Err(
                "SessionSnapshot@1 estimated tokens exceed the JSON safe-integer bound".into(),
            );
        }
        Ok(())
    }
}

/// Payload of `SessionSnapshotPrepared@1`, phase one of the capture protocol: the transcript is
/// in the CAS and verified, and the harness copy is still on disk. A kernel that stops here
/// leaves a durable instruction to finish the deletion, never an ambient transcript nobody owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSnapshotPreparedPayloadV1 {
    pub session_id: String,
    pub session_artifact_id: String,
    pub transcript_artifact_id: String,
    pub source: SessionSourceV1,
    pub bytes: u64,
    pub estimated_tokens: u64,
    /// Host-observed capture time, the only input to the `warm.session.max_age` gate. Like
    /// `WorkspaceRebased@1`'s `preparation_ms`, it is evidence for a policy decision and never
    /// an ordering authority: the event sequence remains the only order.
    pub captured_at_unix_ms: u64,
}

impl SessionSnapshotPreparedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_session_id(&self.session_id) {
            return Err("SessionSnapshotPrepared@1 has an invalid session identity".into());
        }
        if !crate::is_digest(&self.session_artifact_id)
            || !crate::is_digest(&self.transcript_artifact_id)
        {
            return Err("SessionSnapshotPrepared@1 has an invalid artifact ID".into());
        }
        self.source.validate()?;
        if self.bytes == 0 || self.bytes > MAX_SESSION_TRANSCRIPT_BYTES {
            return Err("SessionSnapshotPrepared@1 captured an empty or over-bound session".into());
        }
        if self.estimated_tokens > crate::json::SAFE_INTEGER_MAX as u64
            || self.captured_at_unix_ms > crate::json::SAFE_INTEGER_MAX as u64
        {
            return Err(
                "SessionSnapshotPrepared@1 counts exceed the JSON safe-integer bound".into(),
            );
        }
        Ok(())
    }
}

/// What the idempotent deletion found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCleanupOutcomeV1 {
    /// The transcript was unlinked from the harness directory.
    Deleted,
    /// Nothing was there: a repeated cleanup, or a harness that removed it first. Idempotent.
    AlreadyAbsent,
    /// The path was not the regular file the capture recorded. Nothing was unlinked and the
    /// layer is refused for selection; the reason says what was found.
    Refused,
}

/// Why an idempotent deletion refused to unlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCleanupRefusalV1 {
    /// The entry is not a regular file: a directory, a device, or a symlink standing where the
    /// transcript was.
    NotRegularFile,
    /// A parent component of the harness path is a symlink, so the deletion would have reached
    /// through it. Every component is opened `O_NOFOLLOW`.
    SymlinkedParent,
    /// The harness directory could not be traversed at all.
    Unreadable,
}

/// Payload of `SessionSnapshotCleaned@1`, phase two: the harness copy is gone, or was already
/// gone, or is refused with the reason. Only a completed cleanup makes the layer selectable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSnapshotCleanedPayloadV1 {
    pub session_id: String,
    pub outcome: SessionCleanupOutcomeV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<SessionCleanupRefusalV1>,
}

impl SessionSnapshotCleanedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_session_id(&self.session_id) {
            return Err("SessionSnapshotCleaned@1 has an invalid session identity".into());
        }
        match (self.outcome, self.refusal) {
            (SessionCleanupOutcomeV1::Refused, Some(_)) => Ok(()),
            (SessionCleanupOutcomeV1::Refused, None) => {
                Err("SessionSnapshotCleaned@1 refused without a reason".into())
            }
            (_, Some(_)) => Err(
                "SessionSnapshotCleaned@1 records a refusal reason for a completed deletion".into(),
            ),
            (_, None) => Ok(()),
        }
    }

    /// Whether this cleanup completed, which is the second condition for selecting the layer.
    pub fn completed(&self) -> bool {
        matches!(
            self.outcome,
            SessionCleanupOutcomeV1::Deleted | SessionCleanupOutcomeV1::AlreadyAbsent
        )
    }
}

/// Why a node whose policy asks for the session layer starts this Round without one. Every
/// reason falls back to Notes alone; none of them fails the Attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionDropReasonV1 {
    /// The node's bound adapter cannot host a kernel-assigned session and resume it forked.
    /// Codex is refused here until it has a protocol of its own.
    ProviderUnsupported,
    /// The execution frontend hosting this Attempt does not run the session protocol, whatever
    /// its adapter could do. Distinct from `provider_unsupported` so a report never blames the
    /// provider for a frontend that simply never offered the layer.
    HostUnsupported,
    /// No admitted Attempt of this node in the previous closed Round.
    NoSource,
    /// The source Attempt captured no transcript.
    NotCaptured,
    /// The source Attempt's capture never reached its cleanup-completed phase, or the deletion
    /// was refused. An ambient transcript is never resumed.
    CleanupIncomplete,
    /// The source Attempt is older than `warm.session.max_age`.
    TooOld,
    /// The transcript's estimated tokens do not fit this Attempt's reservation beside the
    /// delta prompt.
    OverReservation,
    /// The transcript could not be re-materialized into this Attempt's harness directory.
    MaterializationFailed,
    /// This Round's Head Delta was dropped over its bound, so a fork could not be told what
    /// moved since the transcript it would continue; the Attempt runs cold with the current
    /// Change Set instead.
    HeadDeltaDropped,
}

/// Payload of `ColdCloseoutDispatched@1`: the compiled, conditional cold Attempt of a required
/// reviewer, dispatched inside the Round only when the warm result would otherwise close it
/// clean. Published once, with its outcome, so a crash never leaves a dispatch without a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdCloseoutDispatchedPayloadV1 {
    pub node: String,
    pub round: u32,
    /// The warm Attempt whose would-be-clean result triggered the closeout.
    pub warm_attempt_id: String,
    /// The exact result artifact the warm Attempt produced. The Ledger folds the cold result
    /// through this link, so the closeout reduces beside the result its own edge delivered.
    pub warm_result_artifact_id: String,
    pub cold_attempt_id: String,
    /// The reservation protected for this Attempt, taken before the warm Attempt ran so a
    /// retry storm cannot consume it. Absent only for a pipeline with no budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_tokens: Option<u64>,
    pub charged_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cold_result_artifact_id: Option<String>,
    /// Present exactly when the cold Attempt produced no admissible result. The warm result
    /// still stands; the Round is incomplete for the closeout, never silently clean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

impl ColdCloseoutDispatchedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.node.trim().is_empty() || self.round == 0 {
            return Err("ColdCloseoutDispatched@1 needs a node and a positive Round".into());
        }
        if !crate::warm::is_monotonic_id(&self.warm_attempt_id)
            || !crate::warm::is_monotonic_id(&self.cold_attempt_id)
        {
            return Err("ColdCloseoutDispatched@1 has an invalid Attempt ID".into());
        }
        if self.warm_attempt_id == self.cold_attempt_id {
            return Err("ColdCloseoutDispatched@1 names one Attempt twice".into());
        }
        if !crate::is_digest(&self.warm_result_artifact_id) {
            return Err("ColdCloseoutDispatched@1 has an invalid warm result artifact".into());
        }
        match (&self.cold_result_artifact_id, &self.failed) {
            (Some(id), None) if crate::is_digest(id) => {}
            (None, Some(reason)) if !reason.trim().is_empty() && reason.len() <= 512 => {}
            _ => {
                return Err(
                    "ColdCloseoutDispatched@1 must name exactly one of a cold result or a failure"
                        .into(),
                );
            }
        }
        if self.charged_tokens > crate::json::SAFE_INTEGER_MAX as u64 {
            return Err(
                "ColdCloseoutDispatched@1 charge exceeds the JSON safe-integer bound".into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn snapshot() -> SessionSnapshotV1 {
        let attempt_id = "a".repeat(26);
        SessionSnapshotV1 {
            session_id: session_id_for_attempt(&attempt_id).unwrap(),
            node: "correctness".into(),
            attempt_id,
            head_snapshot_id: digest('1'),
            source: SessionSourceV1 {
                provider_kind: "claude".into(),
                path_digest: digest('2'),
            },
            transcript_artifact_id: digest('3'),
            bytes: 4096,
            estimated_tokens: 1024,
        }
    }

    #[test]
    fn a_session_identity_is_derived_from_its_attempt_and_never_random() {
        let attempt = "b".repeat(26);
        let first = session_id_for_attempt(&attempt).unwrap();
        assert_eq!(Some(first.clone()), session_id_for_attempt(&attempt));
        assert_ne!(Some(first.clone()), session_id_for_attempt(&"c".repeat(26)));
        assert!(is_session_id(&first), "{first} is not a session identity");
        assert_eq!(attempt_of_session_id(&first).as_deref(), Some(&attempt[..]));
        assert_eq!(
            session_id_for_attempt(&"z".repeat(26)),
            None,
            "an Attempt outside the Round-scoped hex shape hosts no session"
        );
        assert_eq!(session_id_for_attempt("short"), None);
        assert_eq!(
            attempt_of_session_id("01234567-0123-0123-0123-0123456789ab"),
            None,
            "a session identity this kernel never derived names no Attempt"
        );
        assert!(!is_session_id(""));
        assert!(!is_session_id(&"0".repeat(32)));
        assert!(!is_session_id("0123456G-0123-0123-0123-0123456789ab"));
        assert!(!is_session_id("0123456-0123-0123-0123-0123456789ab"));
        assert!(
            !is_session_id("01234567-0123-0123-0123-0123456789ab-0123"),
            "a trailing group is not the canonical shape"
        );
        assert!(!is_session_id("01234567-0123-0123-0123-0123456789AB"));
    }

    #[test]
    fn a_snapshot_binds_its_session_to_its_attempt() {
        snapshot().validate().unwrap();
        let forged = SessionSnapshotV1 {
            session_id: session_id_for_attempt(&"b".repeat(26)).unwrap(),
            ..snapshot()
        };
        assert!(
            forged.validate().is_err(),
            "a session identity another Attempt derived is refused"
        );
        let empty = SessionSnapshotV1 {
            bytes: 0,
            ..snapshot()
        };
        assert!(empty.validate().is_err());
        let huge = SessionSnapshotV1 {
            bytes: MAX_SESSION_TRANSCRIPT_BYTES + 1,
            ..snapshot()
        };
        assert!(huge.validate().is_err());
        let path = SessionSnapshotV1 {
            source: SessionSourceV1 {
                provider_kind: "claude".into(),
                path_digest: "/Users/operator/.claude/projects".into(),
            },
            ..snapshot()
        };
        assert!(
            path.validate().is_err(),
            "a host path is never a source identity"
        );
    }

    #[test]
    fn a_cleanup_records_its_refusal_reason_exactly_when_it_refused() {
        let session_id = session_id_for_attempt(&"a".repeat(26)).unwrap();
        let deleted = SessionSnapshotCleanedPayloadV1 {
            session_id: session_id.clone(),
            outcome: SessionCleanupOutcomeV1::Deleted,
            refusal: None,
        };
        deleted.validate().unwrap();
        assert!(deleted.completed());
        let absent = SessionSnapshotCleanedPayloadV1 {
            outcome: SessionCleanupOutcomeV1::AlreadyAbsent,
            ..deleted.clone()
        };
        absent.validate().unwrap();
        assert!(
            absent.completed(),
            "an idempotent second deletion still completes the protocol"
        );
        let refused = SessionSnapshotCleanedPayloadV1 {
            outcome: SessionCleanupOutcomeV1::Refused,
            refusal: Some(SessionCleanupRefusalV1::SymlinkedParent),
            ..deleted.clone()
        };
        refused.validate().unwrap();
        assert!(!refused.completed());
        let silent = SessionSnapshotCleanedPayloadV1 {
            outcome: SessionCleanupOutcomeV1::Refused,
            ..deleted.clone()
        };
        assert!(silent.validate().is_err());
        let excused = SessionSnapshotCleanedPayloadV1 {
            refusal: Some(SessionCleanupRefusalV1::NotRegularFile),
            ..deleted
        };
        assert!(excused.validate().is_err());
    }

    #[test]
    fn a_prepared_capture_names_its_object_its_source_and_its_bound() {
        let prepared = SessionSnapshotPreparedPayloadV1 {
            session_id: session_id_for_attempt(&"a".repeat(26)).unwrap(),
            session_artifact_id: digest('4'),
            transcript_artifact_id: digest('3'),
            source: SessionSourceV1 {
                provider_kind: "claude".into(),
                path_digest: digest('2'),
            },
            bytes: 4096,
            estimated_tokens: 1024,
            captured_at_unix_ms: 1_789_000_000_000,
        };
        prepared.validate().unwrap();
        let over = SessionSnapshotPreparedPayloadV1 {
            bytes: MAX_SESSION_TRANSCRIPT_BYTES + 1,
            ..prepared.clone()
        };
        assert!(over.validate().is_err());
        let unknown = SessionSnapshotPreparedPayloadV1 {
            source: SessionSourceV1 {
                provider_kind: "Claude Code".into(),
                path_digest: digest('2'),
            },
            ..prepared
        };
        assert!(unknown.validate().is_err());
    }

    #[test]
    fn a_cold_closeout_names_exactly_one_outcome_and_two_distinct_attempts() {
        let dispatched = ColdCloseoutDispatchedPayloadV1 {
            node: "correctness".into(),
            round: 2,
            warm_attempt_id: "a".repeat(26),
            warm_result_artifact_id: digest('5'),
            cold_attempt_id: "b".repeat(26),
            reserved_tokens: Some(300_000),
            charged_tokens: 120_000,
            cold_result_artifact_id: Some(digest('6')),
            failed: None,
        };
        dispatched.validate().unwrap();
        let failed = ColdCloseoutDispatchedPayloadV1 {
            cold_result_artifact_id: None,
            failed: Some("cold closeout Attempt timed out after 1800000ms".into()),
            ..dispatched.clone()
        };
        failed.validate().unwrap();
        let both = ColdCloseoutDispatchedPayloadV1 {
            failed: Some("timed out".into()),
            ..dispatched.clone()
        };
        assert!(both.validate().is_err());
        let neither = ColdCloseoutDispatchedPayloadV1 {
            cold_result_artifact_id: None,
            ..dispatched.clone()
        };
        assert!(neither.validate().is_err());
        let same = ColdCloseoutDispatchedPayloadV1 {
            cold_attempt_id: "a".repeat(26),
            ..dispatched
        };
        assert!(
            same.validate().is_err(),
            "a closeout is a second Attempt, never the warm one relabelled"
        );
    }
}
