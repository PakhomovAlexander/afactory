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

#[test]
fn controlled_capture_preserves_redacted_evidence_after_cancellation() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (dir, cas) = workdir();
    let flag = AtomicBool::new(false);
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(10))
        .with_grant("AF_FIXTURE_SECRET", "secret-cancellation-material");
    let capture = std::thread::scope(|scope| {
        let cancel = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !dir.path().join("ready").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            flag.store(true, Ordering::Release);
            assert!(dir.path().join("ready").exists());
        });
        let capture = runner.capture_settled_with_stdin_controlled(&cas,
            &sh("cat >/dev/null; printf 'prefix:%s' \"$AF_FIXTURE_SECRET\"; printf 'diagnostic:%s' \"$AF_FIXTURE_SECRET\" >&2; touch ready; sleep 30"),
            b"captured input".to_vec(), Some(&flag));
        cancel.join().unwrap();
        capture
    });
    assert!(
        capture
            .status
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert_eq!(capture.stdout, b"prefix:[redacted]");
    assert_eq!(capture.stderr, b"diagnostic:[redacted]");
    assert_eq!(capture.raw_artifact_ids.len(), 2);
    assert_eq!(
        cas.get(&capture.raw_artifact_ids[0]).unwrap(),
        capture.stdout
    );
    assert_eq!(
        cas.get(&capture.raw_artifact_ids[1]).unwrap(),
        capture.stderr
    );
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

    assert!(matches!(error, RunnerError::TimedOut { after_ms: 200, .. }));
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
    let RunnerError::TimedOut {
        raw_artifact: Some(raw_artifact),
        ..
    } = error
    else {
        panic!("timeout did not retain its partial artifact: {error:?}");
    };
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "an orphaned grandchild must not hold the supervisor hostage"
    );

    // The partial stdout was stored to the CAS before the error was returned.
    assert_eq!(cas.get(&raw_artifact).unwrap(), b"partial answer\n");
}

#[test]
fn a_model_parent_exit_cannot_leave_the_stdin_writer_unbounded() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_millis(100));
    // Preserve the original pipe before POSIX asynchronous-command stdin redirection replaces
    // fd 0 with /dev/null; the descendant must genuinely hold the writer open.
    let command = sh("exec 3<&0; sleep 30 <&3 & printf answer");
    let started = Instant::now();

    assert!(matches!(
        runner.capture_with_stdin(&cas, &command, vec![b'x'; 16 * 1024 * 1024]),
        Err(RunnerError::TimedOut { .. })
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_model_descendant_holding_output_is_charged_not_empty_evidence() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(1));
    let error = runner
        .capture(&cas, &sh("sleep 30 & printf partial"))
        .unwrap_err();

    assert!(
        matches!(error, RunnerError::Failed { ref stderr_excerpt, .. } if stderr_excerpt.contains("stdout pipe was still held")),
        "{error:?}"
    );
}

#[test]
fn a_model_descendant_holding_only_stderr_preserves_the_complete_answer() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(1));
    let capture = runner
        .capture(&cas, &sh("sleep 30 >&2 & printf complete"))
        .unwrap();

    assert_eq!(capture.stdout, b"complete");
    assert!(
        String::from_utf8_lossy(&capture.stderr).contains("stderr was still held"),
        "{:?}",
        capture.stderr
    );
}

/// A command Worker may ignore its stdin. The input is larger than an OS pipe, so the child
/// closes it while the parent is still writing; its complete answer must win over that
/// expected broken pipe.
#[test]
fn a_worker_may_ignore_its_stdin() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(5));
    let capture = runner
        .capture_with_stdin(&cas, &sh("printf answer"), vec![b'x'; 1024 * 1024])
        .unwrap();

    assert!(capture.status.success());
    assert_eq!(capture.stdout, b"answer");
}

#[test]
fn large_stdin_and_stderr_are_drained_concurrently() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(5));
    let capture = runner
        .capture_with_stdin(
            &cas,
            &sh("head -c 1048576 /dev/zero >&2; cat >/dev/null; printf answer"),
            vec![b'x'; 1024 * 1024],
        )
        .unwrap();

    assert!(capture.status.success());
    assert_eq!(capture.stdout, b"answer");
    assert_eq!(capture.stderr.len(), 1024 * 1024);
}

#[test]
fn a_briefly_lingering_descendant_cannot_truncate_a_large_answer() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(5));
    let capture = runner
        .capture(
            &cas,
            &sh("sleep 1 & head -c 1048576 /dev/zero | tr '\\000' x"),
        )
        .unwrap();

    assert!(capture.status.success());
    assert_eq!(capture.stdout, vec![b'x'; 1024 * 1024]);
    assert_eq!(cas.get(&capture.raw_artifact).unwrap(), capture.stdout);
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

    // Failure path: the secret lands in stderr, which an adapter may quote in a diagnostic.
    let failed = runner
        .capture(
            &cas,
            &sh("echo \"auth failed for $REVIEW_MODEL_KEY\" >&2; exit 7"),
        )
        .unwrap();
    assert_eq!(failed.status.code(), Some(7));
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        !stderr.contains(secret),
        "a quoted diagnostic would put the credential in the event log: {stderr}"
    );
    assert!(stderr.contains("[redacted]"));
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

#[test]
fn settled_held_output_retains_redacted_bytes_without_admitting_a_message() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(1))
        .with_grant("FIXTURE_SECRET", "sensitive-fixture-value");
    let capture = runner.capture_settled_with_stdin(
        &cas,
        &sh("printf '%s' \"$FIXTURE_SECRET\"; printf '%s' \"$FIXTURE_SECRET\" >&2; sleep 30 &"),
        vec![],
    );
    assert!(matches!(
        capture.status,
        Err(RunnerError::Failed { exit_code: -1, .. })
    ));
    assert_eq!(capture.stdout, b"[redacted]");
    assert_eq!(capture.stderr, b"[redacted]");
    assert_eq!(capture.raw_artifact_ids.len(), 2);
    for id in capture.raw_artifact_ids {
        assert_eq!(cas.get(&id).unwrap(), b"[redacted]");
    }
}
