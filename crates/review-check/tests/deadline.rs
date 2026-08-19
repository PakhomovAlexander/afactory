//! A check that never returns must not hang the review.
//!
//! The gate runs first and the scheduler blocks on its completion, so an unbounded check would
//! block the whole run forever. Past its deadline a check is killed and recorded `not_run` —
//! the same outcome as a check that could not start — so the gate blocks and the run proceeds
//! to an honest, terminating verdict.

use std::time::Duration;

use review_check::{Arg, CheckDefinition, CheckRunner, CheckStatus, Command, GateDecision};

#[test]
fn a_hung_check_is_killed_and_recorded_not_run() {
    let dir = tempfile::tempdir().unwrap();
    let cas = review_store::Cas::open(dir.path().join("cas")).unwrap();
    let runner = CheckRunner::new(&cas, dir.path()).with_timeout(Duration::from_millis(300));

    let check = CheckDefinition::new(
        "hangs",
        Command::new(
            "/bin/sh",
            vec![Arg::literal("-c"), Arg::literal("sleep 60")],
        ),
    );

    let start = std::time::Instant::now();
    let result = runner.run(&check);
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "the deadline must fire; took {elapsed:?}"
    );
    assert_eq!(result.status, CheckStatus::NotRun);
    assert!(
        result.exit_code.is_none(),
        "a killed check has no exit code"
    );
    assert!(
        result
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("killed"),
        "reason should say it was killed: {:?}",
        result.reason
    );
    // A required check that did not run blocks the gate, exactly as a failure does.
    assert!(!GateDecision::evaluate(&[result]).passed());
}

/// The sharp cases the first deadline fix missed: a wrapper that leaves a grandchild holding
/// the stdout pipe. Killing one PID and joining the drain unbounded would hang the review
/// forever; the process-group kill and bounded collection must return.
#[test]
#[cfg(unix)]
fn a_backgrounded_grandchild_does_not_hang_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let cas = review_store::Cas::open(dir.path().join("cas")).unwrap();
    let runner = CheckRunner::new(&cas, dir.path()).with_timeout(Duration::from_millis(300));

    // The direct child exits, but a backgrounded sleep inherits stdout and outlives it.
    let check = CheckDefinition::new(
        "wrapper",
        Command::new(
            "/bin/sh",
            vec![Arg::literal("-c"), Arg::literal("sleep 600 & sleep 600")],
        ),
    );

    let start = std::time::Instant::now();
    let result = runner.run(&check);
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(8),
        "the process-group deadline must fire; took {elapsed:?}"
    );
    assert_eq!(result.status, CheckStatus::NotRun);
}

/// A check that *passes* but backgrounds a child holding stdout must not hang either: the
/// bounded collection returns instead of joining forever.
#[test]
#[cfg(unix)]
fn a_passing_check_with_a_backgrounded_child_returns() {
    let dir = tempfile::tempdir().unwrap();
    let cas = review_store::Cas::open(dir.path().join("cas")).unwrap();
    let runner = CheckRunner::new(&cas, dir.path()).with_timeout(Duration::from_secs(30));

    let check = CheckDefinition::new(
        "passes",
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal("echo done; sleep 600 & exit 0"),
            ],
        ),
    );

    let start = std::time::Instant::now();
    let result = runner.run(&check);
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(3),
        "a passing check must return promptly once its group is reaped; took {elapsed:?}"
    );
    assert_eq!(result.status, CheckStatus::Passed);
}
