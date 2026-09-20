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

    /// The harness directory this layer reads and writes, when it has one: a path a captured
    /// transcript must never carry. `None` for a layer without a host directory.
    fn store_root(&self) -> Option<&Path> {
        None
    }

    /// Byte shapes whose presence refuses a capture outright: credential material the harness
    /// could have echoed into a message or a tool record. Nothing containing one enters the CAS.
    fn credential_markers(&self) -> &'static [&'static [u8]] {
        &[]
    }
}

/// Stands in for the Attempt's sandbox path inside a stored transcript.
pub const SANDBOX_PLACEHOLDER: &[u8] = b"{{af:sandbox}}";
/// Stands in for the harness directory inside a stored transcript.
pub const HARNESS_PLACEHOLDER: &[u8] = b"{{af:harness}}";

/// Why a transcript was refused rather than stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptRefusal {
    /// A credential marker appeared in the bytes.
    Credential,
}

/// Make a transcript storable: every occurrence of the sandbox path and of the harness
/// directory becomes a reversible placeholder, and bytes carrying a credential marker are
/// refused. Paths are replaced longest first so a nested prefix is never half-replaced.
pub fn sanitize_transcript(
    transcript: &[u8],
    working_directory: &Path,
    store_root: Option<&Path>,
    markers: &[&[u8]],
) -> Result<Vec<u8>, TranscriptRefusal> {
    if markers
        .iter()
        .any(|marker| !marker.is_empty() && find(transcript, marker).is_some())
    {
        return Err(TranscriptRefusal::Credential);
    }
    let mut pairs: Vec<(Vec<u8>, &[u8])> = vec![(
        working_directory.as_os_str().as_encoded_bytes().to_vec(),
        SANDBOX_PLACEHOLDER,
    )];
    if let Some(root) = store_root {
        pairs.push((
            root.as_os_str().as_encoded_bytes().to_vec(),
            HARNESS_PLACEHOLDER,
        ));
    }
    pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    let mut bytes = transcript.to_vec();
    for (path, placeholder) in pairs {
        if !path.is_empty() {
            bytes = replace_all(&bytes, &path, placeholder);
        }
    }
    Ok(bytes)
}

/// Undo [`sanitize_transcript`] for one Attempt: the placeholders become this Attempt's
/// sandbox path and the current harness directory.
pub fn rehydrate_transcript(
    transcript: &[u8],
    working_directory: &Path,
    store_root: Option<&Path>,
) -> Vec<u8> {
    let mut bytes = replace_all(
        transcript,
        SANDBOX_PLACEHOLDER,
        working_directory.as_os_str().as_encoded_bytes(),
    );
    if let Some(root) = store_root {
        bytes = replace_all(
            &bytes,
            HARNESS_PLACEHOLDER,
            root.as_os_str().as_encoded_bytes(),
        );
    }
    bytes
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn replace_all(haystack: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(at) = find(rest, needle) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(replacement);
        rest = &rest[at + needle.len()..];
    }
    out.extend_from_slice(rest);
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_become_placeholders_and_come_back_for_the_next_attempt() {
        let sandbox = Path::new("/tmp/af-sandbox-1");
        let harness = Path::new("/home/op/.claude");
        let raw = b"{\"cwd\":\"/tmp/af-sandbox-1/src\",\"store\":\"/home/op/.claude/projects\"}";
        let stored = sanitize_transcript(raw, sandbox, Some(harness), &[]).unwrap();
        assert!(find(&stored, b"/tmp/af-sandbox-1").is_none());
        assert!(find(&stored, b"/home/op").is_none());
        assert!(find(&stored, SANDBOX_PLACEHOLDER).is_some());
        let next = Path::new("/tmp/af-sandbox-2");
        let back = rehydrate_transcript(&stored, next, Some(harness));
        assert_eq!(
            back,
            b"{\"cwd\":\"/tmp/af-sandbox-2/src\",\"store\":\"/home/op/.claude/projects\"}"
        );
    }

    #[test]
    fn a_credential_marker_refuses_the_whole_transcript() {
        let raw = b"{\"text\":\"token sk-ant-abc\"}";
        assert_eq!(
            sanitize_transcript(raw, Path::new("/tmp/s"), None, &[b"sk-ant-"]),
            Err(TranscriptRefusal::Credential)
        );
    }
}
