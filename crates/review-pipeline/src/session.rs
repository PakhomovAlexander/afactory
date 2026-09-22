//! Worker warm layers, package P4: the Session Snapshot protocol and its recovery.
//!
//! The kernel owns the whole protocol and the adapter owns only the three provider-shaped
//! operations behind [`review_runner::SessionLayer`]. One Attempt of a session-hosting node:
//!
//! 1. **Assign.** Before the harness starts, the Attempt's session identity is derived from its
//!    Attempt ID. The transcript is therefore the kernel's to capture and delete, and a kernel
//!    that crashed can still name the file it owes a deletion.
//! 2. **Resume, forked.** When the Round's Warm Set carried a Session Snapshot, its transcript
//!    is re-materialized into this Attempt's harness directory under the *source* session's
//!    identity and resumed with a fork, so the captured object is read and never mutated. The
//!    prompt becomes the delta prompt.
//! 3. **Capture and verify, then delete.** At seal the bounded transcript enters the CAS as
//!    `review.kernel/SessionSnapshot@1`, `SessionSnapshotPrepared@1` records it with the exact
//!    source identity, the harness copy is deleted without following a symlink, and
//!    `SessionSnapshotCleaned@1` closes the protocol.
//!
//! A crash between the two phases leaves a durable prepared record with no cleanup. The sweep
//! that runs before the Round's first dispatch finishes it **without a provider call**: the
//! deletion is a filesystem operation against an identity the log already holds. The same sweep
//! removes the derived working copies a resume materialized and the transcripts failed Attempts
//! left, because every session identity of this Campaign is derivable from an Attempt ID the log
//! records. Warm Set selection requires both the source Attempt's admission and its completed
//! cleanup, so an ambient transcript is never resumed.
//!
//! The Task host does not install a session capability yet, so every Warm Set it selects
//! records `host_unsupported`. The protocol below is kept as the base for that port
//! (ADR-0110).

use std::collections::BTreeSet;
use std::path::Path;

use review_core::{
    EventType, MAX_SESSION_TRANSCRIPT_BYTES, Producer, RunEvent, SessionCleanupOutcomeV1,
    SessionDropReasonV1, SessionSnapshotCleanedPayloadV1, SessionSnapshotPreparedPayloadV1,
    SessionSnapshotV1, SessionSourceV1, attempt_of_session_id, session_id_for_attempt,
};
use review_runner::{ReviewerInputs, SessionCapture, SessionDeletion, SessionLayer, SessionResume};
use review_store::{Cas, NewEvent};

use super::review_domain::ReviewDomainState;

/// The tokens a resumed Attempt must keep free beyond its transcript and its measured delta
/// prompt: the answer it still has to write, plus the Attempt authority section the delta
/// gains once the Attempt is bound. A transcript that does not leave this much of the
/// reservation beside the delta is refused at selection, so no Attempt starts a session it
/// cannot finish.
pub(crate) const SESSION_ANSWER_ALLOWANCE_TOKENS: u64 = 16_384;

/// What the execution frontend hosting a node can do with the session layer. Installed before
/// the Round's first Warm Set is selected; an absent entry is a host that does not run the
/// protocol at all and drops the layer as `host_unsupported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionCapability {
    /// Whether the node's bound adapter can host a kernel-assigned session and resume it forked.
    pub supported: bool,
    /// The reservation one Attempt of this node takes, or `None` for an uncapped pipeline.
    pub reservation_tokens: Option<u64>,
}

impl ReviewDomainState<'_> {
    /// The node's pinned session policy, `None` for a node that hosts no session.
    pub(crate) fn session_policy(&self, node_id: &str) -> Option<review_config::SessionSpec> {
        self.warm_policy(node_id)
            .map(|policy| policy.session)
            .filter(|session| session.captures())
    }

    /// The age bound a resume of this node must satisfy, in seconds.
    pub(crate) fn session_max_age_secs(&self, node_id: &str) -> u64 {
        self.warm_policy(node_id).map_or(0, |policy| {
            policy.session.max_age_secs(policy.session_max_age_secs)
        })
    }

    /// Record what the frontend hosting `node_id` can do with the session layer.
    #[allow(dead_code)] // kept for the Task-host port of ADR-0110
    pub(crate) fn install_session_capability(&self, node_id: &str, capability: SessionCapability) {
        self.session_hosts
            .lock()
            .expect("session hosts")
            .insert(node_id.to_string(), capability);
    }

    pub(crate) fn session_capability(&self, node_id: &str) -> Option<SessionCapability> {
        self.session_hosts
            .lock()
            .expect("session hosts")
            .get(node_id)
            .copied()
    }
}

/// Host-observed wall clock, in milliseconds since the epoch. The only input to the age gate.
pub(crate) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

/// What the session layer amounts to for one Round of one node: the carried
/// `review.kernel/SessionSnapshot@1`, or the exact reason it was not carried. Never both.
pub(crate) type SessionSelection = (Option<String>, Option<SessionDropReasonV1>);

/// Decide the session layer for one Round of one node. Every gate the design names is here —
/// provider support, completed cleanup, age, and fitting the reservation beside the delta
/// prompt — and every failure is a recorded drop rather than a refusal of the Round.
pub(crate) fn select_session(
    cas: &Cas,
    events: &[RunEvent],
    capability: Option<SessionCapability>,
    max_age_secs: u64,
    source_attempt_id: Option<&str>,
    now_ms: u64,
    delta_estimate_tokens: u64,
) -> Result<SessionSelection, String> {
    let dropped = |reason| -> Result<SessionSelection, String> { Ok((None, Some(reason))) };
    let Some(capability) = capability else {
        return dropped(SessionDropReasonV1::HostUnsupported);
    };
    if !capability.supported {
        return dropped(SessionDropReasonV1::ProviderUnsupported);
    }
    let Some(attempt_id) = source_attempt_id else {
        return dropped(SessionDropReasonV1::NoSource);
    };
    let Some(session_id) = session_id_for_attempt(attempt_id) else {
        return dropped(SessionDropReasonV1::NoSource);
    };
    let Some(prepared) = prepared_capture(events, &session_id)? else {
        return dropped(SessionDropReasonV1::NotCaptured);
    };
    if !cleanup_completed(events, &session_id)? {
        return dropped(SessionDropReasonV1::CleanupIncomplete);
    }
    let age_ms = now_ms.saturating_sub(prepared.captured_at_unix_ms);
    if age_ms > max_age_secs.saturating_mul(1000) {
        return dropped(SessionDropReasonV1::TooOld);
    }
    // The transcript, the delta this exact Attempt would send, and the answer allowance must
    // all fit the reservation together; a checked sum, so an overflow drops rather than admits.
    let needed = prepared
        .estimated_tokens
        .checked_add(delta_estimate_tokens)
        .and_then(|sum| sum.checked_add(SESSION_ANSWER_ALLOWANCE_TOKENS));
    if let Some(reservation) = capability.reservation_tokens
        && needed.is_none_or(|needed| needed > reservation)
    {
        return dropped(SessionDropReasonV1::OverReservation);
    }
    // The layer is only selectable if it can still be re-materialized: a transcript the CAS can
    // no longer verify is dropped here, where the drop is recorded, rather than at the sandbox.
    if cas.verify(&prepared.session_artifact_id).is_err()
        || cas.verify(&prepared.transcript_artifact_id).is_err()
    {
        return dropped(SessionDropReasonV1::MaterializationFailed);
    }
    Ok((Some(prepared.session_artifact_id), None))
}

/// The session identity this Attempt runs under, when its node hosts sessions at all. Assigned
/// before the harness starts, so nothing the provider chooses can name the transcript.
pub fn assign_session_id(
    inputs: &mut ReviewerInputs,
    capability: Option<SessionCapability>,
    attempt_id: &str,
) {
    inputs.session_id = capability
        .filter(|capability| capability.supported)
        .and_then(|_| session_id_for_attempt(attempt_id));
}

/// Re-materialize the carried transcript into this Attempt's harness directory and bind the
/// forked resume. A failure here fails the Attempt rather than silently dropping a layer the
/// Round's Warm Set already declared: warmth is declared, so an Attempt either starts from what
/// was recorded or does not start.
pub fn apply_session(
    cas: &Cas,
    layer: Option<&dyn SessionLayer>,
    working_directory: &Path,
    session_artifact_id: Option<&str>,
    inputs: &mut ReviewerInputs,
) -> Result<(), String> {
    let Some(artifact_id) = session_artifact_id else {
        return Ok(());
    };
    let layer =
        layer.ok_or("the Warm Set carries a Session Snapshot but the adapter hosts none")?;
    let snapshot = read_snapshot(cas, artifact_id)?;
    let transcript = cas
        .get_bounded(&snapshot.transcript_artifact_id, snapshot.bytes)
        .map_err(|error| format!("re-materializing the carried session: {error}"))?;
    // The stored bytes name no host path; this Attempt's sandbox and the current harness
    // directory take the placeholders' place, and only here.
    let transcript =
        review_runner::rehydrate_transcript(&transcript, working_directory, layer.store_root());
    layer.materialize(working_directory, &snapshot.session_id, &transcript)?;
    inputs.session_resume = Some(SessionResume {
        session_id: snapshot.session_id,
        artifact_id: artifact_id.to_string(),
        transcript_bytes: snapshot.bytes,
        estimated_tokens: snapshot.estimated_tokens,
    });
    Ok(())
}

/// An Attempt's assigned session, removed from the harness directory when the Attempt ends
/// without the two-phase capture claiming it: a timeout, a malformed answer, a refused
/// result, a panic, or a retry. Dropped at the end of every Attempt, so no path through the
/// reviewer loop can leave the transcript ambient until a later sweep.
pub struct AssignedSession<'a> {
    layer: Option<&'a dyn SessionLayer>,
    session_id: Option<String>,
    node_id: String,
    kept: bool,
}

impl<'a> AssignedSession<'a> {
    pub fn new(
        layer: Option<&'a dyn SessionLayer>,
        session_id: Option<String>,
        node_id: &str,
    ) -> Self {
        Self {
            layer,
            session_id,
            node_id: node_id.to_string(),
            kept: false,
        }
    }

    /// The capture protocol took ownership: it stored the transcript and deleted the copy.
    pub fn keep(&mut self) {
        self.kept = true;
    }
}

impl Drop for AssignedSession<'_> {
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        if let (Some(layer), Some(session_id)) = (self.layer, self.session_id.as_deref())
            && let SessionDeletion::Refused(reason) = layer.delete(session_id, None)
        {
            // The refusal is not silent: the operator sees it now, and the next Round's sweep
            // retries the same deletion before any Attempt is dispatched.
            eprintln!(
                "session hygiene diagnostic for `{}`: the transcript of a failed Attempt could not be removed ({reason:?}); the next Round's sweep retries",
                self.node_id
            );
        }
    }
}

/// Remove the working copy a resume materialized. It is a byte-identical copy of a CAS object
/// the log already names, so its removal is hygiene rather than a phase of the capture protocol
/// and records no event; the sweep removes it too if this never runs.
pub fn remove_working_copy(layer: Option<&dyn SessionLayer>, inputs: &ReviewerInputs) {
    if let (Some(layer), Some(resume)) = (layer, &inputs.session_resume) {
        let _ = layer.delete(&resume.session_id, None);
    }
}

/// Capture one admitted Attempt's transcript through both durable phases. The prepared record
/// is appended before anything is deleted, so a crash in between leaves an instruction to
/// finish rather than an orphaned object or an ambient transcript.
///
/// A node that hosts no session, or an Attempt whose harness wrote nothing, records nothing.
#[allow(dead_code)] // kept for the Task-host port of ADR-0110
pub(crate) fn capture_session(
    domain: &ReviewDomainState<'_>,
    layer: Option<&dyn SessionLayer>,
    node_id: &str,
    attempt_id: &str,
    session_id: Option<&str>,
    working_directory: &Path,
) -> Result<(), String> {
    let (Some(layer), Some(session_id)) = (layer, session_id) else {
        return Ok(());
    };
    let captured = layer.capture(session_id, MAX_SESSION_TRANSCRIPT_BYTES)?;
    let captured = match captured {
        SessionCapture::Captured(captured) => captured,
        SessionCapture::Absent => return Ok(()),
        // An over-bound or refused capture stores nothing. The harness copy is still removed,
        // so the operator's directory never keeps a transcript this Attempt produced.
        SessionCapture::OverBound { .. } | SessionCapture::Refused(_) => {
            let _ = layer.delete(session_id, None);
            return Ok(());
        }
    };
    // What enters the CAS names no host path: the sandbox and the harness directory become
    // placeholders the next Attempt's materialization rehydrates, and a transcript carrying
    // a credential shape is not stored at all. The harness copy is removed either way.
    let transcript = match review_runner::sanitize_transcript(
        &captured.transcript,
        working_directory,
        layer.store_root(),
        layer.credential_markers(),
    ) {
        Ok(transcript) => transcript,
        Err(refusal) => {
            eprintln!(
                "session capture diagnostic for `{node_id}`: transcript of Attempt {attempt_id} refused ({refusal:?}); nothing stored"
            );
            let _ = layer.delete(session_id, None);
            return Ok(());
        }
    };
    let bytes = transcript.len() as u64;
    let transcript_artifact_id = domain
        .cas
        .put(&transcript)
        .map_err(|error| format!("capturing the session transcript: {error}"))?;
    let snapshot = SessionSnapshotV1 {
        node: node_id.to_string(),
        attempt_id: attempt_id.to_string(),
        session_id: session_id.to_string(),
        head_snapshot_id: domain.authority.head_snapshot_id.clone(),
        source: SessionSourceV1 {
            provider_kind: layer.provider_kind().to_string(),
            path_digest: captured.path_digest,
        },
        transcript_artifact_id: transcript_artifact_id.clone(),
        bytes,
        estimated_tokens: review_runner::estimate_tokens(transcript.len()),
    };
    snapshot.validate()?;
    let producer = Producer::Attempt {
        run_id: domain.run_id.clone(),
        node_id: node_id.to_string(),
        attempt_id: attempt_id.to_string(),
    };
    let (session_artifact_id, _) = domain
        .cas
        .put_artifact(
            review_core::contract::SESSION_SNAPSHOT_V1,
            producer,
            vec![transcript_artifact_id.clone()],
            Some(domain.authority.head_snapshot_id.clone()),
            serde_json::to_value(&snapshot).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    let prepared = SessionSnapshotPreparedPayloadV1 {
        session_id: session_id.to_string(),
        session_artifact_id: session_artifact_id.clone(),
        transcript_artifact_id: transcript_artifact_id.clone(),
        source: snapshot.source.clone(),
        bytes,
        estimated_tokens: snapshot.estimated_tokens,
        captured_at_unix_ms: now_unix_ms(),
    };
    prepared.validate()?;
    domain.append(
        NewEvent::new(
            EventType::SessionSnapshotPreparedV1,
            serde_json::to_value(&prepared).map_err(|error| error.to_string())?,
        )
        .node(node_id)
        .attempt(attempt_id.to_string())
        .referencing(vec![session_artifact_id, transcript_artifact_id]),
    )?;
    let deletion = layer.delete(session_id, Some(&prepared.source.path_digest));
    append_cleanup(domain, node_id, Some(attempt_id), session_id, deletion)
}

/// Finish every cleanup this Campaign still owes, and remove every harness transcript its
/// Attempts could have left, before the Round's first dispatch. No provider process is started:
/// a session identity is derived from an Attempt ID the log already records, and the deletion
/// is a filesystem operation.
#[allow(dead_code)] // kept for the Task-host port of ADR-0110
pub(crate) fn sweep_sessions(
    domain: &ReviewDomainState<'_>,
    layer: Option<&dyn SessionLayer>,
    node_id: &str,
) -> Result<(), String> {
    let Some(layer) = layer else {
        return Ok(());
    };
    let events = domain
        .store
        .lock()
        .expect("event store")
        .replay(&domain.run_id)
        .map_err(|error| error.to_string())?;
    // Every session this node could have written, derived from its own Attempt IDs, plus every
    // session a resume of this node materialized as a working copy — which is one of the same
    // Attempt IDs, from the Round before.
    let mut sessions: BTreeSet<String> = BTreeSet::new();
    for event in events.iter().filter(|event| {
        event.event_type == EventType::AttemptDispatchedV1
            && event.node_id.as_deref() == Some(node_id)
    }) {
        if let Some(attempt_id) = &event.attempt_id
            && let Some(session_id) = session_id_for_attempt(attempt_id)
        {
            sessions.insert(session_id);
        }
    }
    let unpaired = unpaired_captures(&events, node_id)?;
    sessions.extend(unpaired.iter().map(|(session, _)| session.clone()));
    for session_id in &sessions {
        let expected = unpaired
            .iter()
            .find(|(session, _)| session == session_id)
            .map(|(_, digest)| digest.as_str());
        let deletion = layer.delete(session_id, expected);
        // Only a capture the log prepared owes a cleanup-completed record. A working copy or a
        // failed Attempt's transcript is removed silently: it was never a capture.
        if expected.is_some() {
            let attempt = attempt_of_session_id(session_id);
            append_cleanup(domain, node_id, attempt.as_deref(), session_id, deletion)?;
        }
    }
    Ok(())
}

fn append_cleanup(
    domain: &ReviewDomainState<'_>,
    node_id: &str,
    attempt_id: Option<&str>,
    session_id: &str,
    deletion: SessionDeletion,
) -> Result<(), String> {
    let (outcome, refusal) = match deletion {
        SessionDeletion::Deleted => (SessionCleanupOutcomeV1::Deleted, None),
        SessionDeletion::AlreadyAbsent => (SessionCleanupOutcomeV1::AlreadyAbsent, None),
        SessionDeletion::Refused(reason) => (SessionCleanupOutcomeV1::Refused, Some(reason)),
    };
    let payload = SessionSnapshotCleanedPayloadV1 {
        session_id: session_id.to_string(),
        outcome,
        refusal,
    };
    payload.validate()?;
    let mut event = NewEvent::new(
        EventType::SessionSnapshotCleanedV1,
        serde_json::to_value(&payload).map_err(|error| error.to_string())?,
    )
    .node(node_id);
    if let Some(attempt_id) = attempt_id {
        event = event.attempt(attempt_id.to_string());
    }
    domain.append(event)
}

/// Captures this node prepared whose cleanup never completed, with the source identity each
/// deletion must match. What recovery owes, read from the log alone.
fn unpaired_captures(events: &[RunEvent], node_id: &str) -> Result<Vec<(String, String)>, String> {
    let mut prepared: Vec<(String, String)> = Vec::new();
    let mut completed: BTreeSet<String> = BTreeSet::new();
    for event in events {
        match event.event_type {
            EventType::SessionSnapshotPreparedV1 if event.node_id.as_deref() == Some(node_id) => {
                let payload: SessionSnapshotPreparedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                payload.validate()?;
                prepared.push((payload.session_id, payload.source.path_digest));
            }
            EventType::SessionSnapshotCleanedV1 => {
                let payload: SessionSnapshotCleanedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                payload.validate()?;
                if payload.completed() {
                    completed.insert(payload.session_id);
                }
            }
            _ => {}
        }
    }
    prepared.retain(|(session, _)| !completed.contains(session));
    Ok(prepared)
}

/// The prepared capture of one session, or `None` when the log holds none.
fn prepared_capture(
    events: &[RunEvent],
    session_id: &str,
) -> Result<Option<SessionSnapshotPreparedPayloadV1>, String> {
    for event in events
        .iter()
        .filter(|event| event.event_type == EventType::SessionSnapshotPreparedV1)
    {
        let payload: SessionSnapshotPreparedPayloadV1 =
            serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
        payload.validate()?;
        if payload.session_id == session_id {
            return Ok(Some(payload));
        }
    }
    Ok(None)
}

fn cleanup_completed(events: &[RunEvent], session_id: &str) -> Result<bool, String> {
    for event in events
        .iter()
        .filter(|event| event.event_type == EventType::SessionSnapshotCleanedV1)
    {
        let payload: SessionSnapshotCleanedPayloadV1 =
            serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
        payload.validate()?;
        if payload.session_id == session_id && payload.completed() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_snapshot(cas: &Cas, artifact_id: &str) -> Result<SessionSnapshotV1, String> {
    let envelope = cas.get_artifact(artifact_id).map_err(|e| e.to_string())?;
    if envelope.artifact_type != review_core::contract::SESSION_SNAPSHOT_V1 {
        return Err(format!("artifact {artifact_id} is not SessionSnapshot@1"));
    }
    let snapshot: SessionSnapshotV1 =
        serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
    snapshot.validate()?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cas() -> (tempfile::TempDir, Cas) {
        let root = tempfile::tempdir().expect("temporary CAS root");
        let cas = Cas::open(root.path()).expect("CAS");
        (root, cas)
    }

    fn capability(reservation: Option<u64>) -> Option<SessionCapability> {
        Some(SessionCapability {
            supported: true,
            reservation_tokens: reservation,
        })
    }

    fn prepared_event(session_id: &str, tokens: u64, captured_at_unix_ms: u64) -> RunEvent {
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let payload = SessionSnapshotPreparedPayloadV1 {
            session_id: session_id.to_string(),
            session_artifact_id: digest('4'),
            transcript_artifact_id: digest('3'),
            source: SessionSourceV1 {
                provider_kind: "claude".into(),
                path_digest: digest('2'),
            },
            bytes: 1024,
            estimated_tokens: tokens,
            captured_at_unix_ms,
        };
        event(
            EventType::SessionSnapshotPreparedV1,
            serde_json::to_value(payload).unwrap(),
        )
    }

    fn cleaned_event(session_id: &str, outcome: SessionCleanupOutcomeV1) -> RunEvent {
        let payload = SessionSnapshotCleanedPayloadV1 {
            session_id: session_id.to_string(),
            outcome,
            refusal: match outcome {
                SessionCleanupOutcomeV1::Refused => {
                    Some(review_core::SessionCleanupRefusalV1::NotRegularFile)
                }
                _ => None,
            },
        };
        event(
            EventType::SessionSnapshotCleanedV1,
            serde_json::to_value(payload).unwrap(),
        )
    }

    fn event(event_type: EventType, payload: serde_json::Value) -> RunEvent {
        RunEvent {
            event_id: "e".repeat(26),
            run_id: "run".into(),
            sequence: 1,
            event_type,
            occurred_at: "2026-09-18T00:00:00Z".into(),
            node_id: Some("correctness".into()),
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload,
        }
    }

    #[test]
    fn an_unsupported_host_and_an_unsupported_provider_are_distinct_drops() {
        let (_root, cas) = cas();
        let source = "a".repeat(26);
        let (carried, dropped) =
            select_session(&cas, &[], None, 3_600, Some(&source), 0, 0).unwrap();
        assert!(carried.is_none());
        assert_eq!(dropped, Some(SessionDropReasonV1::HostUnsupported));
        let unsupported = Some(SessionCapability {
            supported: false,
            reservation_tokens: None,
        });
        let (_, dropped) =
            select_session(&cas, &[], unsupported, 3_600, Some(&source), 0, 0).unwrap();
        assert_eq!(
            dropped,
            Some(SessionDropReasonV1::ProviderUnsupported),
            "Codex and every other adapter without the protocol drop the layer here"
        );
    }

    #[test]
    fn every_gate_failure_is_a_recorded_drop_and_never_a_refusal() {
        let (_root, cas) = cas();
        let source = "a".repeat(26);
        let session_id = session_id_for_attempt(&source).unwrap();
        let (carried, dropped) =
            select_session(&cas, &[], capability(None), 3_600, None, 0, 0).unwrap();
        assert!(carried.is_none());
        assert_eq!(dropped, Some(SessionDropReasonV1::NoSource));

        let (_, dropped) =
            select_session(&cas, &[], capability(None), 3_600, Some(&source), 0, 0).unwrap();
        assert_eq!(dropped, Some(SessionDropReasonV1::NotCaptured));

        let prepared = vec![prepared_event(&session_id, 1_000, 0)];
        let (_, dropped) = select_session(
            &cas,
            &prepared,
            capability(None),
            3_600,
            Some(&source),
            0,
            0,
        )
        .unwrap();
        assert_eq!(
            dropped,
            Some(SessionDropReasonV1::CleanupIncomplete),
            "an ambient transcript is never resumed"
        );

        let refused = vec![
            prepared_event(&session_id, 1_000, 0),
            cleaned_event(&session_id, SessionCleanupOutcomeV1::Refused),
        ];
        let (_, dropped) =
            select_session(&cas, &refused, capability(None), 3_600, Some(&source), 0, 0).unwrap();
        assert_eq!(dropped, Some(SessionDropReasonV1::CleanupIncomplete));

        let clean = vec![
            prepared_event(&session_id, 1_000, 0),
            cleaned_event(&session_id, SessionCleanupOutcomeV1::Deleted),
        ];
        let (_, dropped) = select_session(
            &cas,
            &clean,
            capability(None),
            3_600,
            Some(&source),
            3_600_001,
            0,
        )
        .unwrap();
        assert_eq!(dropped, Some(SessionDropReasonV1::TooOld));

        let (_, dropped) = select_session(
            &cas,
            &clean,
            capability(Some(SESSION_ANSWER_ALLOWANCE_TOKENS + 999)),
            3_600,
            Some(&source),
            0,
            0,
        )
        .unwrap();
        assert_eq!(
            dropped,
            Some(SessionDropReasonV1::OverReservation),
            "a transcript must fit the reservation beside the delta prompt"
        );

        let (_, dropped) = select_session(
            &cas,
            &clean,
            capability(Some(SESSION_ANSWER_ALLOWANCE_TOKENS + 1_000)),
            3_600,
            Some(&source),
            0,
            0,
        )
        .unwrap();
        assert_eq!(
            dropped,
            Some(SessionDropReasonV1::MaterializationFailed),
            "a transcript the CAS cannot verify is dropped at selection, where it is recorded"
        );

        // The measured delta counts: the same transcript no longer fits once the delta this
        // Attempt would send is added, and fits again when the reservation covers all three.
        let (_, dropped) = select_session(
            &cas,
            &clean,
            capability(Some(SESSION_ANSWER_ALLOWANCE_TOKENS + 1_499)),
            3_600,
            Some(&source),
            0,
            500,
        )
        .unwrap();
        assert_eq!(dropped, Some(SessionDropReasonV1::OverReservation));
        let (_, dropped) = select_session(
            &cas,
            &clean,
            capability(Some(SESSION_ANSWER_ALLOWANCE_TOKENS + 1_500)),
            3_600,
            Some(&source),
            0,
            500,
        )
        .unwrap();
        assert_eq!(dropped, Some(SessionDropReasonV1::MaterializationFailed));
        let (_, dropped) = select_session(
            &cas,
            &clean,
            capability(Some(u64::MAX)),
            3_600,
            Some(&source),
            0,
            u64::MAX,
        )
        .unwrap();
        assert_eq!(
            dropped,
            Some(SessionDropReasonV1::OverReservation),
            "a sum that overflows drops rather than admits"
        );
    }

    #[test]
    fn recovery_owes_exactly_the_captures_whose_cleanup_never_completed() {
        let first = session_id_for_attempt(&"a".repeat(26)).unwrap();
        let second = session_id_for_attempt(&"b".repeat(26)).unwrap();
        let events = vec![
            prepared_event(&first, 10, 0),
            cleaned_event(&first, SessionCleanupOutcomeV1::Deleted),
            prepared_event(&second, 10, 0),
        ];
        let owed = unpaired_captures(&events, "correctness").unwrap();
        assert_eq!(
            owed.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            vec![second.clone()]
        );
        let finished = {
            let mut events = events;
            events.push(cleaned_event(
                &second,
                SessionCleanupOutcomeV1::AlreadyAbsent,
            ));
            events
        };
        assert!(
            unpaired_captures(&finished, "correctness")
                .unwrap()
                .is_empty(),
            "an idempotent second deletion closes the protocol"
        );
        assert!(
            unpaired_captures(&finished, "other").unwrap().is_empty(),
            "one node never recovers another node's session"
        );
    }
}
