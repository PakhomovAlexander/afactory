//! Segments: one per process, append-only JSONL under an exclusive lock, sealed on exit and
//! recovered by a later reader to their last complete line (design §6).
//!
//! [`writer`] is the write side, linked by every crate that records. [`reader`] is the read
//! side — recovery, sealed and live reads — and with [`crate::index`] and [`crate::status`] it
//! is the reader module only `af telemetry`, `af storage` and the TUI link (ADR-0153 clause 1).

pub mod reader;
pub mod writer;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use sha2::{Digest, Sha256};

/// Bytes at the end of a segment kept for its terminal records (`problem`, `seal`).
pub const TERMINAL_RESERVE_BYTES: u64 = 64 * 1024;

/// Eight lowercase hex digits no other call in this machine's recent past returned: a hash of
/// the time, the process, the thread and a counter.
pub fn nonce() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = Sha256::new();
    hasher.update(crate::layout::unix_ns_now().to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(format!("{:?}", std::thread::current().id()).as_bytes());
    hasher.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    crate::trace::hex(&hasher.finalize()[..4])
}

/// A segment id: `<started_unix_ms>-<pid>-<nonce>`.
pub fn new_segment_id(started_unix_ns: u128) -> String {
    format!(
        "{}-{}-{}",
        started_unix_ns / 1_000_000,
        std::process::id(),
        nonce()
    )
}

/// The writer's clock: durations come from the monotonic clock, and the segment header records
/// once the unix time of its origin.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    origin: Instant,
    origin_unix_ns: u128,
}

impl Clock {
    pub fn start() -> Self {
        Self {
            origin: Instant::now(),
            origin_unix_ns: crate::layout::unix_ns_now(),
        }
    }

    pub fn origin_unix_ns(&self) -> u128 {
        self.origin_unix_ns
    }

    /// Unix nanoseconds now: the origin plus the monotonic time since it.
    pub fn now_unix_ns(&self) -> u128 {
        self.origin_unix_ns + self.origin.elapsed().as_nanos()
    }

    /// Unix nanoseconds of `instant` on this clock; an instant before the origin is the origin.
    pub fn unix_ns_at(&self, instant: Instant) -> u128 {
        self.origin_unix_ns + instant.saturating_duration_since(self.origin).as_nanos()
    }
}
