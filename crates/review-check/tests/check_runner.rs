//! The check runner against real processes: what each execution records, and what it refuses.
//!
//! A project check command is a trusted program with typed arguments. `/bin/sh -c` with a
//! literal script is allowed; what is refused is splicing anything derived from the change under
//! review into a position where it would be read as an option.

use review_check::{Arg, CheckDefinition, CheckRunner, CheckStatus, Command, GateDecision};
use review_store::Cas;

fn shell(name: &str, script: &str) -> CheckDefinition {
    CheckDefinition::new(
        name,
        Command::new("/bin/sh", vec![Arg::literal("-c"), Arg::literal(script)]),
    )
}

#[test]
fn a_failure_keeps_its_exit_code_and_output_and_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let runner = CheckRunner::new(&cas, dir.path());

    let results = [
        runner.run(&shell("ok-check", "true")),
        runner.run(&shell("bad-check", "echo boom >&2; exit 7")),
    ];
    let decision = GateDecision::evaluate(&results);

    assert_eq!(results[0].status, CheckStatus::Passed);
    let bad = &results[1];
    assert_eq!(bad.status, CheckStatus::Failed);
    assert_eq!(bad.exit_code, Some(7), "the real exit code is kept");
    let stderr = bad
        .stderr
        .as_deref()
        .expect("the failing check's output is an artifact");
    assert_eq!(
        String::from_utf8(cas.get(stderr).unwrap()).unwrap(),
        "boom\n"
    );
    assert!(!decision.passed());
    assert_eq!(decision.blocking, vec!["bad-check"]);
}

/// A check whose definition names no program did not run; it did not fail. It is a visible
/// result that still blocks the gate, never a line silently dropped from the total.
#[test]
fn a_check_with_no_program_is_not_run_and_blocks_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let runner = CheckRunner::new(&cas, dir.path());

    let results = [
        runner.run(&shell("has-command", "true")),
        runner.run(&CheckDefinition::new(
            "no-command-here",
            Command::new("", vec![]),
        )),
    ];
    let decision = GateDecision::evaluate(&results);

    let malformed = &results[1];
    assert_eq!(malformed.status, CheckStatus::NotRun);
    assert!(malformed.reason.is_some());
    assert_eq!(malformed.exit_code, None);
    assert_eq!(malformed.program, None, "no program is claimed to exist");
    assert!(!decision.passed());
    assert_eq!(decision.executed, 2);
    assert_eq!(decision.required, 2);
    assert_eq!(decision.blocking, vec!["no-command-here"]);
}

/// The property the whole node exists for: two executions of the same check both survive.
#[test]
fn a_later_execution_does_not_erase_an_earlier_one() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let runner = CheckRunner::new(&cas, dir.path());

    // The gate runs, fails, someone fixes it, the gate runs again.
    let first = runner.run(&shell("build", "echo first attempt >&2; exit 2"));
    let second = runner.run(&shell("build", "echo second attempt >&2; exit 0"));
    assert_eq!(first.status, CheckStatus::Failed);
    assert!(second.passed());

    let first_stderr = first.stderr.as_deref().unwrap();
    let second_stderr = second.stderr.as_deref().unwrap();
    assert_ne!(
        first_stderr, second_stderr,
        "the two attempts are distinct artifacts"
    );
    assert_eq!(
        String::from_utf8(cas.get(first_stderr).unwrap()).unwrap(),
        "first attempt\n",
        "the first attempt's output is still readable after the second ran"
    );
    assert_eq!(
        String::from_utf8(cas.get(second_stderr).unwrap()).unwrap(),
        "second attempt\n"
    );
}

/// The vector this typing exists to close: a path taken from the diff that looks like an option.
#[test]
fn a_hostile_filename_from_the_diff_cannot_become_an_option() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let runner = CheckRunner::new(&cas, dir.path());

    // `{tests}` filled from the bundle — i.e. from paths in the change under review.
    let check = CheckDefinition::new(
        "stateless-related",
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal("echo \"$@\""),
                Arg::literal("sh"),
                Arg::untrusted("--config=/tmp/evil"),
            ],
        ),
    );

    let result = runner.run(&check);
    assert_eq!(
        result.status,
        CheckStatus::NotRun,
        "the check must be refused, not run with the value quoted into place"
    );
    assert!(
        result
            .reason
            .as_deref()
            .unwrap()
            .contains("would be read as an option")
    );
    assert!(result.stdout.is_none(), "nothing was executed");
    assert!(!GateDecision::evaluate(&[result]).passed());

    let mut provider_called = false;
    let contained = runner.run_with(&check, |_, _, _, _| {
        provider_called = true;
        unreachable!("an invalid typed command must never reach its execution provider")
    });
    assert_eq!(contained.status, CheckStatus::NotRun);
    assert!(!provider_called);
}

/// A check that writes past the output cap fails, whatever its exit code, with a reason naming
/// the cap; the capped prefix is still its recorded stdout (M2).
#[test]
fn a_check_over_the_output_cap_fails_naming_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let runner = CheckRunner::new(&cas, dir.path()).with_output_cap(4096);
    let flag = std::sync::atomic::AtomicBool::new(false);
    for runner in [
        CheckRunner::new(&cas, dir.path()).with_output_cap(4096),
        runner.with_cancellation(Some(&flag)),
    ] {
        let result = runner.run(&shell("noisy", "head -c 10000 /dev/zero; exit 0"));
        assert_eq!(result.status, CheckStatus::Failed);
        assert_eq!(result.exit_code, Some(0));
        let reason = result.reason.as_deref().unwrap();
        assert!(
            reason.contains("stdout was truncated at the 4 KiB output cap: it wrote 10000 bytes"),
            "{reason}"
        );
        assert_eq!(
            cas.get(result.stdout.as_deref().unwrap()).unwrap().len(),
            4096
        );
        assert!(!GateDecision::evaluate(&[result]).passed());
    }
    let quiet = CheckRunner::new(&cas, dir.path())
        .with_output_cap(4096)
        .run(&shell("quiet", "head -c 4096 /dev/zero"));
    assert_eq!(quiet.status, CheckStatus::Passed);
}

/// The default cap is 64 MiB: a check printing without end does not grow the kernel past it,
/// runs to its own exit and fails.
#[test]
fn a_check_printing_200_mib_fails_at_the_default_cap() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let result = CheckRunner::new(&cas, dir.path()).run(&shell(
        "flood",
        "head -c 209715200 /dev/zero; echo done >&2",
    ));
    assert_eq!(result.status, CheckStatus::Failed);
    assert_eq!(result.exit_code, Some(0));
    assert!(
        result
            .reason
            .as_deref()
            .unwrap()
            .contains("stdout was truncated at the 64 MiB output cap: it wrote 209715200 bytes")
    );
    assert_eq!(
        cas.get(result.stdout.as_deref().unwrap()).unwrap().len(),
        review_process::MAX_CAPTURED_STREAM_BYTES
    );
    assert_eq!(
        cas.get(result.stderr.as_deref().unwrap()).unwrap(),
        b"done\n"
    );
}

/// A host-local check's `PATH` holds only existing absolute directories (I6). The test runs
/// itself again in a child whose own `PATH` carries an empty, a `.` and a relative entry.
#[test]
fn a_local_check_path_holds_only_absolute_directories() {
    const PROBE: &str = "AF_FIXTURE_CHECK_PATH_PROBE";
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let result = CheckRunner::new(&cas, dir.path()).run(&shell("path", "printf '%s' \"$PATH\""));
    assert_eq!(result.status, CheckStatus::Passed);
    let path = String::from_utf8(cas.get(result.stdout.as_deref().unwrap()).unwrap()).unwrap();
    assert!(
        path.split(':')
            .all(|entry| entry.starts_with('/') && std::path::Path::new(entry).is_dir()),
        "{path}"
    );
    if std::env::var_os(PROBE).is_some() {
        assert!(path.contains("/bin"), "{path}");
        return;
    }
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_local_check_path_holds_only_absolute_directories",
            "--test-threads=1",
        ])
        .env(PROBE, "1")
        .env(
            "PATH",
            "::.:relative/bin:/nonexistent/af-bin:/usr/bin:/bin:",
        )
        .output()
        .unwrap();
    assert!(
        child.status.success() && String::from_utf8_lossy(&child.stdout).contains("1 passed"),
        "{}",
        String::from_utf8_lossy(&child.stdout)
    );
}

/// A child that leaves a local check's process group with `setsid` is ended before the check
/// returns, so it cannot write into the tree sealed after it (L7). Linux only: elsewhere a
/// `setsid` child escapes the group kill on `trusted_local`.
#[cfg(target_os = "linux")]
#[test]
fn a_setsid_child_of_a_local_check_is_ended_before_sealing() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let result = CheckRunner::new(&cas, dir.path()).run(&shell(
        "escape",
        "setsid sh -c 'sleep 2; touch x' >/dev/null 2>&1 </dev/null &",
    ));
    assert_eq!(result.status, CheckStatus::Passed);
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(
        !dir.path().join("x").exists(),
        "a setsid child wrote into the tree after its check returned"
    );
}
