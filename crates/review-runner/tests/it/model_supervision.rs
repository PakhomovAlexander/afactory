//! The model-runner hazards, each driven by a scripted fake model: a hang, a leaked secret, a
//! missing provider. No model, no network, no spend — which is the point: every failure path
//! is proved before a real provider ever gets to exercise one.

use std::time::{Duration, Instant};

use crate::load_safe_wall::LOAD_SAFE_WALL;

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
    let runner = ModelRunner::new(dir.path(), LOAD_SAFE_WALL)
        .with_grant("AF_FIXTURE_SECRET", "secret-cancellation-material");
    let capture = std::thread::scope(|scope| {
        let cancel = scope.spawn(|| {
            let deadline = Instant::now() + LOAD_SAFE_WALL;
            while !dir.path().join("ready").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            flag.store(true, Ordering::Release);
            assert!(dir.path().join("ready").exists());
        });
        let capture = runner.capture(&cas,
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
    let capture = runner.capture(&cas, &sh("sleep 30"), vec![], None);
    let waited = started.elapsed();

    assert!(matches!(
        capture.status,
        Err(RunnerError::TimedOut { after_ms: 200 })
    ));
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
    let capture = runner.capture(&cas, &sh("echo partial answer; sleep 30"), vec![], None);
    assert!(
        matches!(capture.status, Err(RunnerError::TimedOut { .. })),
        "{:?}",
        capture.status
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "an orphaned grandchild must not hold the supervisor hostage"
    );

    // The partial stdout was stored to the CAS before the error was returned.
    assert_eq!(capture.raw_artifact_ids.len(), 1);
    assert_eq!(
        cas.get(&capture.raw_artifact_ids[0]).unwrap(),
        b"partial answer\n"
    );
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
        runner
            .capture(&cas, &command, vec![b'x'; 16 * 1024 * 1024], None)
            .status,
        Err(RunnerError::TimedOut { .. })
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_model_descendant_holding_output_is_charged_not_empty_evidence() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), LOAD_SAFE_WALL);
    let capture = runner.capture(&cas, &sh("sleep 30 & printf partial"), vec![], None);

    assert!(
        matches!(capture.status, Err(RunnerError::Failed { ref stderr_excerpt, .. }) if stderr_excerpt.contains("stdout pipe was still held")),
        "{:?}",
        capture.status
    );
    assert_eq!(cas.get(&capture.raw_artifact_ids[0]).unwrap(), b"partial");
}

#[test]
fn a_model_descendant_holding_only_stderr_preserves_the_complete_answer() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), LOAD_SAFE_WALL);
    let capture = runner.capture(&cas, &sh("sleep 30 >&2 & printf complete"), vec![], None);

    assert!(capture.status.unwrap().success());
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
    let runner = ModelRunner::new(dir.path(), LOAD_SAFE_WALL);
    let capture = runner.capture(&cas, &sh("printf answer"), vec![b'x'; 1024 * 1024], None);

    assert!(capture.status.unwrap().success());
    assert_eq!(capture.stdout, b"answer");
}

#[test]
fn large_stdin_and_stderr_are_drained_concurrently() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), LOAD_SAFE_WALL);
    let capture = runner.capture(
        &cas,
        &sh("head -c 1048576 /dev/zero >&2; cat >/dev/null; printf answer"),
        vec![b'x'; 1024 * 1024],
        None,
    );

    assert!(capture.status.unwrap().success());
    assert_eq!(capture.stdout, b"answer");
    assert_eq!(capture.stderr.len(), 1024 * 1024);
}

#[test]
fn a_briefly_lingering_descendant_cannot_truncate_a_large_answer() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), LOAD_SAFE_WALL);
    let capture = runner.capture(
        &cas,
        &sh("sleep 1 & head -c 1048576 /dev/zero | tr '\\000' x"),
        vec![],
        None,
    );

    assert!(capture.status.unwrap().success());
    assert_eq!(capture.stdout, vec![b'x'; 1024 * 1024]);
    assert_eq!(
        cas.get(&capture.raw_artifact_ids[0]).unwrap(),
        capture.stdout
    );
}

/// A granted credential reaches the child — and nothing this layer stores or reports. The
/// fake model does the worst thing a CLI does in practice: echoes its environment into both
/// streams on failure.
#[test]
fn a_granted_secret_is_redacted_from_everything_kept() {
    let (dir, cas) = workdir();
    let secret = "rt_live_key_5f3a9c1b2d";
    let runner =
        ModelRunner::new(dir.path(), LOAD_SAFE_WALL).with_grant("REVIEW_MODEL_KEY", secret);

    // Success path: the child proves it *received* the grant by writing it out.
    let capture = runner.capture(&cas, &sh("echo \"key=$REVIEW_MODEL_KEY\""), vec![], None);
    assert_eq!(capture.stdout, b"key=[redacted]\n");
    let stored = cas.get(&capture.raw_artifact_ids[0]).unwrap();
    assert_eq!(
        stored, b"key=[redacted]\n",
        "the CAS copy is the redacted one"
    );

    // Failure path: the secret lands in stderr, which an adapter may quote in a diagnostic.
    let failed = runner.capture(
        &cas,
        &sh("echo \"auth failed for $REVIEW_MODEL_KEY\" >&2; exit 7"),
        vec![],
        None,
    );
    assert_eq!(failed.status.unwrap().code(), Some(7));
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        !stderr.contains(secret),
        "a quoted diagnostic would put the credential in the event log: {stderr}"
    );
    assert!(stderr.contains("[redacted]"));
    let stored = cas.get(&failed.raw_artifact_ids[0]).unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains(secret));
}

/// An ungranted credential simply is not there: the environment is rebuilt, not filtered.
#[test]
fn an_ungranted_variable_never_reaches_the_child() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), LOAD_SAFE_WALL);
    let capture = runner.capture(
        &cas,
        &sh("echo \"token=${GITHUB_TOKEN:-absent}\""),
        vec![],
        None,
    );
    assert_eq!(capture.stdout, b"token=absent\n");
}

#[test]
fn a_missing_provider_is_unavailable_not_silent() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(1));
    let command = Command::new("/nonexistent/model-cli", vec![]);
    assert!(matches!(
        runner.capture(&cas, &command, vec![], None).status,
        Err(RunnerError::Unavailable(_))
    ));
}

/// The same typed-slot boundary as checks: an untrusted value cannot become an option, and the
/// refusal happens before any process exists.
#[test]
fn an_untrusted_option_is_refused_before_the_model_starts() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), Duration::from_secs(1));
    let command = Command::new("/bin/sh", vec![Arg::untrusted("--dangerously-bypass")]);
    let capture = runner.capture(&cas, &command, vec![], None);
    assert!(matches!(capture.status, Err(RunnerError::Refused(_))));
    assert!(capture.raw_artifact_ids.is_empty());
}

#[test]
fn settled_held_output_retains_redacted_bytes_without_admitting_a_message() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), LOAD_SAFE_WALL)
        .with_grant("FIXTURE_SECRET", "sensitive-fixture-value");
    let capture = runner.capture(
        &cas,
        &sh("printf '%s' \"$FIXTURE_SECRET\"; printf '%s' \"$FIXTURE_SECRET\" >&2; sleep 30 &"),
        vec![],
        None,
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

/// Output past the cap is not evidence the kernel can keep whole: the Attempt fails, naming
/// the cap and what was written, and keeps the capped prefix (M2).
#[test]
fn a_worker_writing_past_its_output_cap_fails_its_attempt() {
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), LOAD_SAFE_WALL).with_output_cap(4096);
    let capture = runner.capture(&cas, &sh("head -c 10000 /dev/zero; exit 0"), vec![], None);
    match capture.status {
        Err(RunnerError::Failed {
            exit_code,
            stderr_excerpt,
        }) => {
            assert_eq!(exit_code, 0);
            assert!(
                stderr_excerpt
                    .contains("stdout was truncated at the 4 KiB output cap: it wrote 10000 bytes"),
                "{stderr_excerpt}"
            );
        }
        other => panic!("an over-cap Attempt must fail: {other:?}"),
    }
    assert_eq!(capture.stdout.len(), 4096);
    assert_eq!(cas.get(&capture.raw_artifact_ids[0]).unwrap().len(), 4096);

    // Under the cap, the same Worker is an ordinary success.
    let capture = runner.capture(&cas, &sh("head -c 4096 /dev/zero"), vec![], None);
    assert!(capture.status.unwrap().success());
}

/// A granted path is redacted however the CLI spells it: as granted, and canonical (a symbolic
/// link resolved, `/var` as `/private/var` on macOS).
#[test]
fn a_granted_path_is_redacted_in_its_canonical_spelling_too() {
    let (dir, cas) = workdir();
    let real = dir.path().join("real-config");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("config-link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let granted = link.display().to_string();
    let runner = ModelRunner::new(dir.path(), LOAD_SAFE_WALL).with_grant("FIXTURE_DIR", &granted);
    let capture = runner.capture(
        &cas,
        &sh("printf '%s\\n' \"$FIXTURE_DIR\"; cd \"$FIXTURE_DIR\" && pwd -P"),
        vec![],
        None,
    );
    assert_eq!(capture.stdout, b"[redacted]\n[redacted]\n");
    let canonical = real.canonicalize().unwrap().display().to_string();
    let stored = String::from_utf8(cas.get(&capture.raw_artifact_ids[0]).unwrap()).unwrap();
    assert!(!stored.contains(&canonical) && !stored.contains(&granted));
}

/// An Attempt's `PATH` is the parent's with every empty, relative or missing entry removed, so
/// a Worker never resolves a program against the candidate tree it runs in (I6). The parent
/// runs this test again in a child whose own `PATH` carries such entries.
#[test]
fn an_attempt_path_holds_only_absolute_directories() {
    const PROBE: &str = "AF_FIXTURE_ATTEMPT_PATH_PROBE";
    let (dir, cas) = workdir();
    let runner = ModelRunner::new(dir.path(), LOAD_SAFE_WALL);
    let capture = runner.capture(&cas, &sh("printf '%s' \"$PATH\""), vec![], None);
    let path = String::from_utf8(capture.stdout).unwrap();
    assert_eq!(
        std::ffi::OsString::from(&path),
        review_process::sanitized_path()
    );
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
            "model_supervision::an_attempt_path_holds_only_absolute_directories",
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
