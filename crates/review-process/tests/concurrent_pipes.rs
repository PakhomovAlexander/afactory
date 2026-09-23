//! Separate supervised commands must never keep one another's standard streams alive.

#[test]
fn concurrent_short_commands_and_killed_groups_keep_independent_pipes() {
    use review_process::{SupervisedError, run_supervised_captured};
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    let ready = Arc::new(Barrier::new(12));
    let started = Instant::now();
    let results = std::thread::scope(|scope| {
        (0..12)
            .map(|worker| {
                let ready = Arc::clone(&ready);
                scope.spawn(move || {
                    let mut failures = Vec::new();
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
                        let output =
                            run_supervised_captured(&mut command, None, Duration::from_millis(100));
                        let elapsed = started.elapsed();
                        let expected = if hanging {
                            matches!(output.status, Err(SupervisedError::TimedOut { .. }))
                                && output.stdout == b"prefix"
                        } else {
                            output.status.as_ref().is_ok_and(|status| status.success())
                                && output.stdout == b"complete"
                        };
                        if !expected || elapsed >= Duration::from_secs(2) {
                            failures.push(format!(
                                "worker {worker}, round {round}: {:?}, {elapsed:?}, {:?}",
                                output.status, output.stdout
                            ));
                        }
                    }
                    failures
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(results.is_empty(), "{}", results.join("\n"));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "process creation must not serialize Worker execution"
    );
}
