//! The total wall a test gives a fixture process whose subject is not the deadline (ADR-0114).
//! Shared by the runner crates' test binaries, like `native_cancellation.rs`.

use std::time::Duration;

/// A fake provider answers in milliseconds when it runs alone, but under a loaded gate (seven
/// nextest threads, one-minute load 7-28) starting its shell or Python took about 5 s, so the
/// 5 s walls these tests used to pass raced scheduling and correct tests failed as timeouts.
/// Two minutes is far above the slowest observed run: it only bounds a hung fixture, and a
/// passing test never waits for it, because every process here exits on its own. Do not shrink
/// it toward a run's elapsed time. A test whose subject is the deadline keeps its own short wall.
pub const LOAD_SAFE_WALL: Duration = Duration::from_secs(120);
