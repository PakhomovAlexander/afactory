//! Separate supervised commands must never keep one another's standard streams alive.
//!
//! Split in two (ADR-0124): the pipe independence below holds on any host, so its short commands
//! run under a load-safe timeout and only the hanging ones under the 100 ms timeout their kill is
//! about; the real-time bounds (each short command within 2 s, the whole run within 10 s) are
//! the second test's, which runs alone in nextest's exclusive group.

use review_process::{SupervisedError, run_supervised_captured};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

/// A short command that answers ends when it answers; only a hung one ever waits this long
/// (ADR-0114). A loaded host took seconds to start a shell.
const LOAD_SAFE: Duration = Duration::from_secs(120);

/// What every round of every worker observed: the failures, and each command's elapsed time
/// with whether it was a hanging one.
struct Rounds {
    failures: Vec<String>,
    elapsed: Vec<(bool, Duration)>,
}

/// Twelve workers, 24 rounds each, every round started together: every third worker runs a
/// command that prints a prefix and hangs, killed at its 100 ms timeout; the others run one that
/// prints and exits, under `short_timeout`.
fn run_rounds(short_timeout: Duration) -> Rounds {
    let ready = Arc::new(Barrier::new(12));
    let results = std::thread::scope(|scope| {
        (0..12)
            .map(|worker| {
                let ready = Arc::clone(&ready);
                scope.spawn(move || {
                    let mut failures = Vec::new();
                    let mut timings = Vec::new();
                    for round in 0..24 {
                        let hanging = worker % 3 == 0;
                        let mut command = std::process::Command::new("/bin/sh");
                        command.args([
                            "-c",
                            if hanging {
                                "printf prefix; sleep 30"
                            } else {
                                "printf complete"
                            },
                        ]);
                        ready.wait();
                        let started = Instant::now();
                        let timeout = if hanging {
                            Duration::from_millis(100)
                        } else {
                            short_timeout
                        };
                        let output = run_supervised_captured(&mut command, None, timeout);
                        let elapsed = started.elapsed();
                        timings.push((hanging, elapsed));
                        let expected = if hanging {
                            matches!(output.status, Err(SupervisedError::TimedOut { .. }))
                                && output.stdout == b"prefix"
                        } else {
                            output.status.as_ref().is_ok_and(|status| status.success())
                                && output.stdout == b"complete"
                        };
                        if !expected {
                            failures.push(format!(
                                "worker {worker}, round {round}: {:?}, {elapsed:?}, {:?}",
                                output.status, output.stdout
                            ));
                        }
                    }
                    (failures, timings)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let mut rounds = Rounds {
        failures: Vec::new(),
        elapsed: Vec::new(),
    };
    for (failures, timings) in results {
        rounds.failures.extend(failures);
        rounds.elapsed.extend(timings);
    }
    rounds
}

#[test]
fn concurrent_short_commands_and_killed_groups_keep_independent_pipes() {
    let rounds = run_rounds(LOAD_SAFE);
    assert!(rounds.failures.is_empty(), "{}", rounds.failures.join("\n"));
    assert_eq!(
        rounds
            .elapsed
            .iter()
            .filter(|(hanging, _)| !hanging)
            .count(),
        8 * 24,
        "every short command completed"
    );
}

/// The real-time half: with the hanging groups killed beside them, each short command completes
/// within its own 100 ms timeout, every command, killed or not, is done within 2 s, and the
/// whole run within 10 s, so process creation does not serialize Worker execution. Listed in
/// `.config/nextest.toml`'s exclusive group.
#[test]
fn concurrent_short_commands_complete_within_their_real_time_bounds() {
    let started = Instant::now();
    let rounds = run_rounds(Duration::from_millis(100));
    assert!(rounds.failures.is_empty(), "{}", rounds.failures.join("\n"));
    let slow: Vec<String> = rounds
        .elapsed
        .iter()
        .filter(|(_, elapsed)| *elapsed >= Duration::from_secs(2))
        .map(|(hanging, elapsed)| format!("{elapsed:?} (hanging: {hanging})"))
        .collect();
    assert!(
        slow.is_empty(),
        "commands took 2 s or more: {}",
        slow.join(", ")
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "process creation must not serialize Worker execution"
    );
}
