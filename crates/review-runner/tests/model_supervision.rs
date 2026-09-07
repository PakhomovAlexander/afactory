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

/// A granted secret split across two reads of the pipe is still redacted: the chunk boundary
/// is inside the secret, and both the returned bytes and the CAS copy are scrubbed.
#[test]
fn a_secret_split_across_chunks_is_still_redacted() {
    let (dir, cas) = workdir();
    let secret = "rt_live_key_5f3a9c1b2d";
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(10))
        .with_grant("REVIEW_MODEL_KEY", secret);
    let capture = runner
        .capture(
            &cas,
            &sh("printf 'key=rt_live'; sleep 0.3; printf '_key_5f3a9c1b2d\\n'"),
        )
        .unwrap();
    assert_eq!(capture.stdout, b"key=[redacted]\n");
    assert_eq!(cas.get(&capture.raw_artifact).unwrap(), b"key=[redacted]\n");
}

/// Streaming changes nothing about what is kept: a multi-megabyte, multi-chunk capture is
/// byte-identical to the process's own stdout, returned and in the CAS.
#[test]
fn a_normal_capture_is_byte_identical_to_the_direct_bytes() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(30));
    let script = "seq 1 300000";
    let direct = std::process::Command::new("/bin/sh")
        .args(["-c", script])
        .output()
        .unwrap()
        .stdout;
    assert!(direct.len() > 1024 * 1024, "{} bytes", direct.len());

    let capture = runner.capture(&cas, &sh(script)).unwrap();
    assert!(capture.status.success());
    assert_eq!(capture.stdout, direct);
    assert_eq!(cas.get(&capture.raw_artifact).unwrap(), direct);

    let mut streamed = Vec::new();
    let capture = runner
        .capture_streamed(&cas, &sh(script), None, &mut |chunk: &[u8]| {
            streamed.extend_from_slice(chunk);
        })
        .unwrap();
    assert_eq!(streamed, direct);
    assert_eq!(capture.stdout_bytes, direct.len() as u64);
    assert_eq!(cas.get(&capture.raw_artifact).unwrap(), direct);
}

/// The ceiling is enforced while draining: an endless producer is ended at the limit — long
/// before its deadline — the Attempt is malformed output naming the limit, and exactly the
/// bytes up to the limit are its raw artifact.
#[test]
fn reviewer_stdout_past_the_ceiling_ends_the_process_and_is_malformed_output() {
    let (dir, cas) = workdir();
    let limit = 4096;
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(30)).with_output_limit(limit);
    let started = Instant::now();
    let error = runner.capture(&cas, &sh("yes")).unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the producer must be ended at the ceiling, not at the deadline"
    );
    let RunnerError::MalformedOutput { raw_artifact, why } = error else {
        panic!("expected MalformedOutput, got {error:?}");
    };
    assert!(why.contains("MAX_REVIEWER_OUTPUT_BYTES"), "{why}");
    assert!(why.contains(&limit.to_string()), "{why}");
    let kept = cas.get(&raw_artifact).unwrap();
    assert_eq!(kept.len(), limit);
    assert!(kept.starts_with(b"y\ny\n"));

    // Exactly the limit is not an overflow.
    let exact = runner.capture(&cas, &sh("head -c 4096 /dev/zero")).unwrap();
    assert_eq!(exact.stdout.len(), limit);
    assert_eq!(
        review_runner::MAX_REVIEWER_OUTPUT_BYTES,
        64 * 1024 * 1024,
        "the documented default"
    );
}

/// A capture with no input gives the child `/dev/null` on fd 0, not an open pipe. Provider CLIs
/// branch on it — several read a prompt from stdin when it is a pipe — so what fd 0 *is* is part
/// of the invocation, not an implementation detail of how stdout is drained.
#[test]
fn a_capture_without_input_leaves_the_child_no_pipe_on_stdin() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(10));
    let probe = sh("if [ -p /dev/stdin ]; then echo pipe; else echo not-a-pipe; fi");

    let capture = runner.capture(&cas, &probe).unwrap();
    assert_eq!(capture.stdout, b"not-a-pipe\n");

    // With input there is a pipe, and the input arrives.
    let echo = sh("cat; if [ -p /dev/stdin ]; then echo pipe; else echo not-a-pipe; fi");
    let capture = runner
        .capture_with_stdin(&cas, &echo, b"prompt\n".to_vec())
        .unwrap();
    assert_eq!(capture.stdout, b"prompt\npipe\n");
}

/// The ceiling is not a stdout ceiling with a hole beside it: a producer that writes its runaway
/// output to fd 2 is bounded too, and what is kept says so instead of pretending to be whole.
#[test]
fn runaway_stderr_is_bounded_and_says_it_was_cut() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(60));
    // Four times the stderr ceiling, then a normal answer on stdout.
    let capture = runner
        .capture(
            &cas,
            &sh("yes 'stderr noise' | head -c 4194304 >&2; printf answer"),
        )
        .unwrap();

    assert_eq!(capture.stdout, b"answer");
    assert!(
        capture.stderr.len() < review_process::MAX_STDERR_BYTES + 4096,
        "stderr was not bounded: {} bytes",
        capture.stderr.len()
    );
    let stderr = String::from_utf8_lossy(&capture.stderr);
    assert!(stderr.starts_with("stderr noise"), "{}", &stderr[..64]);
    assert!(
        stderr.trim_end().ends_with("the rest was discarded]"),
        "a silent cut is indistinguishable from a short stderr: {:?}",
        &stderr[stderr.len().saturating_sub(120)..]
    );
}

/// The ceiling bounds what is *kept*, not what was read. Every occurrence of a grant shorter
/// than `[redacted]` expands, so counting raw bytes would spool, publish, and report an artifact
/// larger than the limit its own refusal names.
#[test]
fn the_ceiling_bounds_the_redacted_artifact_that_grants_expanded() {
    let (dir, cas) = workdir();
    let limit = 4096;
    // Seven bytes in, ten bytes out: `sk-abc\n` (7) becomes `[redacted]\n` (11).
    let secret = "sk-abc";
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(30))
        .with_output_limit(limit)
        .with_grant("REVIEW_MODEL_KEY", secret);

    let error = runner
        .capture(&cas, &sh("yes \"$REVIEW_MODEL_KEY\""))
        .unwrap_err();
    let RunnerError::MalformedOutput { raw_artifact, why } = error else {
        panic!("expected MalformedOutput, got {error:?}");
    };
    assert!(why.contains(&limit.to_string()), "{why}");
    let kept = cas.get(&raw_artifact).unwrap();
    assert_eq!(
        kept.len(),
        limit,
        "the published artifact must never exceed the limit the refusal names"
    );
    assert!(kept.starts_with(b"[redacted]\n[redacted]\n"));
    assert!(!kept.windows(secret.len()).any(|w| w == secret.as_bytes()));

    // And below the ceiling, the reported count is exactly what was published.
    let mut streamed = Vec::new();
    let capture = runner
        .capture_streamed(
            &cas,
            &sh("printf '%s\\n' \"$REVIEW_MODEL_KEY\" \"$REVIEW_MODEL_KEY\""),
            None,
            &mut |chunk: &[u8]| streamed.extend_from_slice(chunk),
        )
        .unwrap();
    assert_eq!(streamed, b"[redacted]\n[redacted]\n");
    assert_eq!(capture.stdout_bytes, streamed.len() as u64);
    assert_eq!(
        cas.get(&capture.raw_artifact).unwrap().len() as u64,
        capture.stdout_bytes
    );
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
