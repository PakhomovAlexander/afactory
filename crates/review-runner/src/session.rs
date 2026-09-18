//! Worker warm layers, package P4: the adapter surface of the Session Snapshot.
//!
//! The kernel owns the protocol — assign the identity, capture, record, delete, record — and the
//! adapter owns exactly the three provider-shaped operations the protocol needs: read the
//! transcript of a kernel-assigned session, delete it without following a symlink, and put a
//! re-materialized transcript where the harness will find it for a forked resume.
//!
//! An adapter that does not implement [`SessionLayer`] has no session layer at all. That is how
//! Codex stays excluded: its pinned CLI has no `--session-id`, its `exec resume` and `exec fork`
//! reject the adapter's `-C` and `-s` flags, and its authentication directory is the directory a
//! kernel-owned session home would replace, so it keeps `--ephemeral` and returns `None` here by
//! inheriting the default.

use std::path::Path;

use review_core::SessionCleanupRefusalV1;

/// The bytes one Attempt's harness session left behind, with the source identity the durable
/// record names. The path itself never leaves the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedSession {
    pub transcript: Vec<u8>,
    /// Content identity of the harness path the transcript was read from.
    pub path_digest: String,
}

/// What locating and reading a kernel-assigned session found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCapture {
    /// The transcript was read whole, within the bound.
    Captured(CapturedSession),
    /// The harness wrote no transcript for this session. Nothing to capture and nothing to
    /// delete: the next Round's Warm Set records `not_captured`.
    Absent,
    /// The harness wrote more than the capture bound. Nothing enters the CAS; the kernel still
    /// deletes the harness copy, so an over-bound session leaves no ambient transcript either.
    OverBound { bytes: u64 },
    /// What stood at the transcript's place was not the regular file a session is, or a parent
    /// component was a symlink. Nothing is read and nothing is unlinked.
    Refused(SessionCleanupRefusalV1),
}

/// The outcome of one idempotent, no-follow deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDeletion {
    Deleted,
    AlreadyAbsent,
    Refused(SessionCleanupRefusalV1),
}

/// The provider-shaped half of the session protocol. Implemented only by adapters whose pinned
/// CLI accepts a kernel-assigned session identity and can resume it with a fork.
pub trait SessionLayer: Send + Sync {
    /// The adapter kind recorded as the session's source, such as `claude`. Lowercase, so it is
    /// a stable vocabulary term rather than a display name.
    fn provider_kind(&self) -> &'static str;

    /// Read the transcript of `session_id` from the harness directory, bounded by `max_bytes`.
    /// The session is located by its kernel-assigned identity alone, never by reconstructing the
    /// Attempt's working directory, so a kernel recovering after a crash finds the same file.
    fn capture(&self, session_id: &str, max_bytes: u64) -> Result<SessionCapture, String>;

    /// Delete the transcript of `session_id`, without following a symlink at any component.
    /// Idempotent: an absent transcript completes the protocol. `expected_path_digest` is the
    /// identity the capture recorded; a file found under another path is refused rather than
    /// unlinked, so recovery cannot delete something the capture never saw.
    fn delete(&self, session_id: &str, expected_path_digest: Option<&str>) -> SessionDeletion;

    /// Place `transcript` where the harness will find it when the next invocation in
    /// `working_directory` runs `--resume <source_session_id> --fork-session`. The bytes are
    /// written under the *source* Attempt's session identity, because that is what `--resume`
    /// names; the fork then writes the resuming Attempt's own session, so the captured object
    /// is read and never mutated. Returns the content identity of the path written, which the
    /// kernel uses to delete the working copy afterwards.
    fn materialize(
        &self,
        working_directory: &Path,
        source_session_id: &str,
        transcript: &[u8],
    ) -> Result<String, String>;
}

/// What one Attempt resumes: the source session the kernel re-materialized and the identity of
/// the artifact it came from. Never serialized into a Worker Input — the bytes reach the model
/// through the harness's own session store, and the manifest names the artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionResume {
    /// The source Attempt's session identity: what `--resume` names and where the transcript
    /// was re-materialized. The resuming Attempt's own identity is separate and is what
    /// `--session-id` assigns, so the fork lands in a session of this Attempt's own.
    pub session_id: String,
    /// The `review.kernel/SessionSnapshot@1` the transcript came from.
    pub artifact_id: String,
    pub transcript_bytes: u64,
    pub estimated_tokens: u64,
}
