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
