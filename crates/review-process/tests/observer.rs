//! The output observer (design §7.3 of `docs/design/telemetry.md`): one non-blocking offer per
//! chunk the drain reads, a queue bounded by bytes that drops and counts, and a capture that is
//! the same with an observer as without one — its bytes, status, held output and deadline.

use review_process::{
    ExitPolicy, ObservedOutput, ObservedStream, SupervisedCapture, SupervisedError,
    run_supervised_captured_observed, run_supervised_captured_with_policy,
};
use std::process::Command;
use std::time::{Duration, Instant};

/// The wall a fixture whose subject is not the deadline gets (ADR-0114).
const WALL: Duration = Duration::from_secs(120);

fn sh(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

fn observed(script: &str, capacity: u64) -> (SupervisedCapture, ObservedOutput, u64, u64) {
    let (observer, output) = review_process::observer(capacity, None);
    let capture = run_supervised_captured_observed(
        &mut sh(script),
        None,
        WALL,
        ExitPolicy::PreserveProcessGroup,
        None,
        &observer,
    );
    observer.close();
    (capture, output, observer.dropped_chunks(), observer.gaps())
}

/// Every chunk taken, by stream, as `(offset, bytes)`.
fn taken(output: &ObservedOutput) -> Vec<(ObservedStream, u64, Vec<u8>)> {
    std::iter::from_fn(|| output.try_take())
        .map(|chunk| (chunk.stream, chunk.byte_offset, chunk.bytes))
        .collect()
}

fn joined(chunks: &[(ObservedStream, u64, Vec<u8>)], stream: ObservedStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (of, offset, chunk) in chunks {
        if *of == stream {
            assert_eq!(*offset, bytes.len() as u64, "offsets are contiguous");
            bytes.extend_from_slice(chunk);
        }
    }
    bytes
}

#[test]
fn the_observer_sees_every_chunk_at_its_offset_and_changes_nothing() {
    let script = "printf 'one\\n'; printf 'err\\n' >&2; printf 'two\\n'; exit 3";
    let (capture, output, dropped, gaps) = observed(script, 1 << 20);
    let plain = run_supervised_captured_with_policy(
        &mut sh(script),
        None,
        WALL,
        ExitPolicy::PreserveProcessGroup,
    );
    assert_eq!(capture.stdout, plain.stdout);
    assert_eq!(capture.stderr, plain.stderr);
    assert_eq!(
        capture.status.as_ref().unwrap().code(),
        plain.status.as_ref().unwrap().code()
    );
    let chunks = taken(&output);
    assert_eq!(joined(&chunks, ObservedStream::Stdout), b"one\ntwo\n");
    assert_eq!(joined(&chunks, ObservedStream::Stderr), b"err\n");
    assert_eq!((dropped, gaps), (0, 0));
    assert!(output.closed());
}

#[test]
fn telemetry_observer_full_queue_drops_and_counts() {
    // Nobody takes from a 64 KiB queue while the child writes 4 MiB: the queue fills, every
    // further chunk is dropped and counted, and the capture is still the whole stream.
    let script = "head -c 4194304 /dev/zero | tr '\\0' 'x'";
    let started = Instant::now();
    let (capture, output, dropped, gaps) = observed(script, 64 * 1024);
    assert!(capture.status.as_ref().unwrap().success());
    assert_eq!(capture.stdout.len(), 4 << 20);
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(dropped > 0, "a full queue drops");
    assert!(gaps >= 1 && gaps <= dropped);
    let chunks = taken(&output);
    let kept: u64 = chunks.iter().map(|(_, _, bytes)| bytes.len() as u64).sum();
    assert!(
        kept <= 64 * 1024,
        "the queue never held more than its bound: {kept}"
    );
    // What was kept is a prefix of the stream at its true offsets; no chunk was invented.
    for (_, offset, bytes) in &chunks {
        let at = usize::try_from(*offset).unwrap();
        assert_eq!(&capture.stdout[at..at + bytes.len()], bytes.as_slice());
    }
}

#[test]
fn telemetry_observer_keeps_held_outcomes() {
    // A descendant holds stdout: the supervisor reports `OutputHeld` and keeps the bytes; the
    // observer saw them and changed neither.
    let held = "printf 'kept\\n'; printf 'diagnostic' >&2; sleep 30 &";
    let (capture, output, _, _) = observed(held, 1 << 20);
    assert!(matches!(
        capture.status,
        Err(SupervisedError::OutputHeld("stdout"))
    ));
    assert_eq!(capture.stdout, b"kept\n");
    assert_eq!(capture.stderr, b"diagnostic");
    let chunks = taken(&output);
    assert_eq!(joined(&chunks, ObservedStream::Stdout), b"kept\n");
    // A descendant holds only stderr: the status is kept and so is `stderr_held`.
    let held = "printf 'held' >&2; sleep 30 >&2 & printf complete";
    let (capture, output, _, _) = observed(held, 1 << 20);
    assert!(capture.status.as_ref().unwrap().success());
    assert!(capture.stderr_held);
    assert_eq!(capture.stdout, b"complete");
    assert_eq!(capture.stderr, b"held");
    let chunks = taken(&output);
    assert_eq!(joined(&chunks, ObservedStream::Stdout), b"complete");
    assert_eq!(joined(&chunks, ObservedStream::Stderr), b"held");
}

#[test]
fn telemetry_observer_failure_never_blocks_drain() {
    // The consumer goes away before the child writes: every offer fails, is dropped and
    // counted, and the drain, the bytes and the status are what they are without an observer.
    let (observer, output) = review_process::observer(1 << 20, None);
    drop(output);
    let script = "head -c 1048576 /dev/zero | tr '\\0' 'y'; printf 'tail' >&2";
    let capture = run_supervised_captured_observed(
        &mut sh(script),
        None,
        WALL,
        ExitPolicy::PreserveProcessGroup,
        None,
        &observer,
    );
    assert!(capture.status.as_ref().unwrap().success());
    assert_eq!(capture.stdout.len(), 1 << 20);
    assert_eq!(capture.stderr, b"tail");
    assert!(observer.dropped_chunks() >= 2);
    assert_eq!(observer.gaps(), 2, "one run per stream");
}

#[test]
fn a_timed_out_or_cancelled_process_keeps_its_bytes_with_an_observer() {
    let (observer, output) = review_process::observer(1 << 20, None);
    let capture = run_supervised_captured_observed(
        &mut sh("printf partial; sleep 30"),
        None,
        Duration::from_millis(300),
        ExitPolicy::PreserveProcessGroup,
        None,
        &observer,
    );
    assert!(matches!(
        capture.status,
        Err(SupervisedError::TimedOut { .. })
    ));
    assert_eq!(capture.stdout, b"partial");
    assert_eq!(output.try_take().unwrap().bytes, b"partial");
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel = std::sync::Arc::clone(&flag);
    let (observer, output) = review_process::observer(1 << 20, None);
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        cancel.store(true, std::sync::atomic::Ordering::Release);
    });
    let capture = run_supervised_captured_observed(
        &mut sh("printf before; sleep 30"),
        None,
        WALL,
        ExitPolicy::PreserveProcessGroup,
        Some(&flag),
        &observer,
    );
    canceller.join().unwrap();
    assert!(matches!(capture.status, Err(SupervisedError::Cancelled)));
    assert_eq!(capture.stdout, b"before");
    assert_eq!(output.try_take().unwrap().bytes, b"before");
}
