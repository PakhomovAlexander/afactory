//! Trace and execution identities (design §5). Telemetry ids are not kernel identities and
//! carry no authority; the kernel's identities are the join keys.
//!
//! A kernel run id is not machine-global (`task_run_id` hashes only the Task ID), so every run
//! is namespaced by its Store instance: `af.store_id` is `SHA-256` of the Store directory's
//! canonical absolute path, and `trace_id` the first 16 bytes of
//! `SHA-256("af/trace@1:" || af.store_id || ":" || run_id)`. Every process that works on the run
//! in that directory appends to the same trace; a copied Store directory is a new instance. Each
//! process is one **execution**, identified by its segment id.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::record::Attrs;

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 15)] as char);
    }
    out
}

/// `sha256:` and the 64 hex digits of `SHA-256(bytes)`.
pub fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

/// `af.store_id`: the 64 hex digits of `SHA-256` over the Store directory's canonical absolute
/// path. Fails when the path cannot be canonicalized.
pub fn store_id(store: &Path) -> std::io::Result<String> {
    let canonical = std::fs::canonicalize(store)?;
    Ok(store_id_of_canonical(&canonical))
}

/// [`store_id`] of a path already canonical.
pub fn store_id_of_canonical(canonical: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    hex(&Sha256::digest(canonical.as_os_str().as_bytes()))
}

/// `trace_id`: 32 hex digits, the first 16 bytes of
/// `SHA-256("af/trace@1:" || store_id || ":" || run_id)`.
pub fn trace_id(store_id: &str, run_id: &str) -> String {
    let digest = Sha256::digest(format!("af/trace@1:{store_id}:{run_id}").as_bytes());
    hex(&digest[..16])
}

/// `span_id`: 16 hex digits, the first 8 bytes of `SHA-256(segment_id || ":" || seq)` of the
/// span's `span_start` record. Reproducible from the segment, a derived one included.
pub fn span_id(segment_id: &str, seq: u64) -> String {
    let digest = Sha256::digest(format!("{segment_id}:{seq}").as_bytes());
    hex(&digest[..8])
}

/// `occurrence_id` of a `blob_ref` record: `segment_id:seq`.
pub fn occurrence_id(segment_id: &str, seq: u64) -> String {
    format!("{segment_id}:{seq}")
}

/// `af.corr`: the first 16 hex digits of `SHA-256(segment_id || ":" || original)`, the
/// kernel-derived correlation id that stands in for a harness-supplied identifier until the
/// release decision (§7.3).
pub fn correlation_id(segment_id: &str, original: &str) -> String {
    let digest = Sha256::digest(format!("{segment_id}:{original}").as_bytes());
    hex(&digest[..8])
}

/// One execution of a run: the process's segment and its place among the run's executions.
/// The kernel supplies `ordinal` (derived from the Store's committed lease records when the
/// execution starts: one more than the leases committed before it) and
/// `store_sequence_at_start` (the Store's highest committed event sequence at that moment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Execution {
    pub store_id: String,
    pub run_id: String,
    /// The segment id of the process.
    pub execution_id: String,
    pub ordinal: u64,
    pub store_sequence_at_start: u64,
}

impl Execution {
    /// The ordinal of an execution started after `committed_leases` earlier lease grants of the
    /// same run.
    pub fn ordinal_after(committed_leases: u64) -> u64 {
        committed_leases.saturating_add(1)
    }

    pub fn trace_id(&self) -> String {
        trace_id(&self.store_id, &self.run_id)
    }

    /// The identity attributes of the execution's `af.run` span.
    pub fn attributes(&self) -> Attrs {
        Attrs::new()
            .id("af.store_id", self.store_id.clone())
            .id("af.run_id", self.run_id.clone())
            .id("af.execution_id", self.execution_id.clone())
            .int(
                "af.execution.ordinal",
                i64::try_from(self.ordinal).unwrap_or(i64::MAX),
            )
            .decimal(
                "af.store.sequence_at_start",
                self.store_sequence_at_start.to_string(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_have_their_documented_widths() {
        assert_eq!(trace_id(&"a".repeat(64), "run").len(), 32);
        assert_eq!(span_id("seg", 2).len(), 16);
        assert_eq!(correlation_id("seg", "msg_1").len(), 16);
        assert_ne!(span_id("seg", 2), span_id("seg", 3));
        assert_eq!(occurrence_id("seg", 7), "seg:7");
        assert_eq!(
            sha256_digest(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(Execution::ordinal_after(0), 1);
    }
}
