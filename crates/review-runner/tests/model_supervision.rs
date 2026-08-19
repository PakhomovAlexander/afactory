//! The model-runner hazards, each driven by a scripted fake model: a hang, a leaked secret, a
//! missing provider. No model, no network, no spend — which is the point: every failure path
//! is proved before a real provider ever gets to exercise one.

use std::time::{Duration, Instant};

use review_core::{Arg, Command};
use review_runner::{ModelRunner, RunnerError};
use review_store::Cas;

fn workdir() -> (tempfile::TempDir, Cas) {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    (dir, cas)
}

fn sh(script: &str) -> Command {
    Command::new(
        "/bin/sh",
        vec![Arg::literal("-c"), Arg::literal(script.to_string())],
    )
}

/// A reviewer that hangs is killed at the deadline and reported as such — not waited on, and
/// not mistaken for a reviewer that found nothing.
#[test]
fn a_hung_reviewer_is_killed_at_the_deadline() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_millis(200));

    let started = Instant::now();
    let error = runner.capture(&cas, &sh("sleep 30")).unwrap_err();
    let waited = started.elapsed();

    assert!(matches!(error, RunnerError::TimedOut { after_ms: 200 }));
    assert!(
        waited < Duration::from_secs(5),
        "the deadline must be enforced by killing, not by waiting out the sleep ({waited:?})"
    );
}

/// What the model wrote before hanging is still collected: a kill must not also destroy the
/// evidence of what happened up to it.
#[test]
fn a_killed_reviewer_keeps_what_it_wrote_so_far() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_millis(200));

    // The compound command matters: `sh` forks `sleep` as a grandchild, so killing only the
    // direct child would leave an orphan holding the stdout pipe — and this call would then
    // take the orphan's 30 seconds to return. The elapsed assertion is what catches that.
    let started = Instant::now();
    let error = runner
        .capture(&cas, &sh("echo partial answer; sleep 30"))
        .unwrap_err();
    assert!(matches!(error, RunnerError::TimedOut { .. }));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "an orphaned grandchild must not hold the supervisor hostage"
    );

    // The partial stdout was stored to the CAS before the error was returned.
    assert!(
        cas.contains(&review_store::canonical::blob_content_id(
            b"partial answer\n"
        )),
        "the bytes written before the kill must be inspectable"
    );
}

/// A granted credential reaches the child — and nothing this layer stores or reports. The
/// fake model does the worst thing a CLI does in practice: echoes its environment into both
/// streams on failure.
#[test]
fn a_granted_secret_is_redacted_from_everything_kept() {
    let (dir, cas) = workdir();
    let secret = "rt_live_key_5f3a9c1b2d";
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(10))
        .with_grant("REVIEW_MODEL_KEY", secret);

    // Success path: the child proves it *received* the grant by writing it out.
    let capture = runner
        .capture(&cas, &sh("echo \"key=$REVIEW_MODEL_KEY\""))
        .unwrap();
    assert_eq!(capture.stdout, b"key=[redacted]\n");
    let stored = cas.get(&capture.raw_artifact).unwrap();
    assert_eq!(
        stored, b"key=[redacted]\n",
        "the CAS copy is the redacted one"
    );

    // Failure path: the secret lands in stderr and must not reach the error excerpt.
    let error = runner
        .capture(
            &cas,
            &sh("echo \"auth failed for $REVIEW_MODEL_KEY\" >&2; exit 7"),
        )
        .unwrap()
        .require_success()
        .unwrap_err();
    let RunnerError::Failed {
        exit_code,
        stderr_excerpt,
    } = &error
    else {
        panic!("expected Failed, got {error:?}");
    };
    assert_eq!(*exit_code, 7);
    assert!(
        !stderr_excerpt.contains(secret),
        "the excerpt would put the credential in the event log: {stderr_excerpt}"
    );
    assert!(stderr_excerpt.contains("[redacted]"));
}

/// An ungranted credential simply is not there: the environment is rebuilt, not filtered.
#[test]
fn an_ungranted_variable_never_reaches_the_child() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(10));
    let capture = runner
        .capture(&cas, &sh("echo \"token=${GITHUB_TOKEN:-absent}\""))
        .unwrap();
    assert_eq!(capture.stdout, b"token=absent\n");
}

#[test]
fn a_missing_provider_is_unavailable_not_silent() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(1));
    let command = Command::new("/nonexistent/model-cli", vec![]);
    assert!(matches!(
        runner.capture(&cas, &command).unwrap_err(),
        RunnerError::Unavailable(_)
    ));
}

/// The same typed-slot boundary as checks: an untrusted value cannot become an option, and the
/// refusal happens before any process exists.
#[test]
fn an_untrusted_option_is_refused_before_the_model_starts() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(1));
    let command = Command::new("/bin/sh", vec![Arg::untrusted("--dangerously-bypass")]);
    assert!(matches!(
        runner.capture(&cas, &command).unwrap_err(),
        RunnerError::Refused(_)
    ));
}
