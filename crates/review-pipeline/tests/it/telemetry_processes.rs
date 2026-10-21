//! Process spans and their resource use (telemetry design §7.2, §15 P3): the crates that receive
//! `review-process`'s results record a `process` span from each — `af.harness` from the model
//! runner, `af.check` from the check runner, `af.git` from the git adapter — with the process
//! id, its exit, its stream sizes and the CPU time and peak memory the supervisor carried
//! through every result type, a timed-out, cancelled or duplex one included. A figure the
//! platform did not deliver is absent, never zero.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use review_check::{Arg, CheckDefinition, CheckRunner, Command};
use review_runner::ModelRunner;
use review_store::Cas;
use review_telemetry::level::Level;
use review_telemetry::record::SealExit;
use review_telemetry::segment::writer::{Writer, WriterOptions};
use review_telemetry::subscriber::TelemetrySubscriber;
use serde_json::Value;

const WALL: Duration = Duration::from_secs(120);

fn sh(script: &str) -> Command {
    Command::new(
        "/bin/sh",
        vec![Arg::literal("-c"), Arg::literal(script.to_string())],
    )
}

/// Run `body` inside a recorded run (`af.command` > `af.run`) and return every span: its name,
/// kind, start attributes and end attributes and status.
fn recorded(root: &Path, body: impl FnOnce()) -> Vec<(String, String, Value, Value)> {
    let writer = Writer::new(WriterOptions::new(root.join("telemetry"), Level::Spans)).unwrap();
    let dispatch = tracing::Dispatch::new(TelemetrySubscriber::new(writer.clone()));
    tracing::dispatcher::with_default(&dispatch, || {
        let command = tracing::info_span!("af.command");
        let _command = command.enter();
        let run = tracing::info_span!(
            "af.run",
            af.store_id = "e".repeat(64).as_str(),
            af.run_id = "run-1",
            af.run.kind = "task",
            af.execution_id = "execution-1",
        );
        let _run = run.enter();
        body();
    });
    writer.flush();
    let text = std::fs::read_to_string(writer.path().unwrap()).unwrap();
    writer.finish(SealExit::Ok).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    lines
        .iter()
        .filter(|line| line["t"] == "span_start")
        .map(|start| {
            let end = lines
                .iter()
                .find(|line| line["t"] == "span_end" && line["span_id"] == start["span_id"])
                .unwrap_or_else(|| panic!("no end for {start}"));
            (
                start["name"].as_str().unwrap().to_string(),
                start["kind"].as_str().unwrap().to_string(),
                start["attributes"].clone(),
                serde_json::json!({"status": end["status"], "attributes": end["attributes"]}),
            )
        })
        .collect()
}

fn git(repo: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// Design §15 P3: a successful model process, a cancelled process, a timed-out check and a
/// duplex git operation each record their process span with the process id and the user and
/// system CPU time the supervisor delivered.
#[test]
fn telemetry_rusage_on_every_result_type() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("file.txt"), b"content\n").unwrap();
    git(&repo, &["add", "file.txt"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@invalid",
            "commit",
            "-qm",
            "one",
        ],
    );
    let spans = recorded(temp.path(), || {
        // A successful model process.
        let runner = ModelRunner::new(&work, WALL).with_harness("anthropic", Some("m".into()));
        let done = runner.capture(
            &cas,
            &sh("cat >/dev/null; echo ok"),
            b"input".to_vec(),
            None,
        );
        assert!(done.status.unwrap().success());
        // A cancelled process: cancelled once it is known to run.
        let flag = AtomicBool::new(false);
        let ready = work.join("ready");
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let deadline = Instant::now() + WALL;
                while !ready.exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                flag.store(true, Ordering::Release);
            });
            let cancelled = ModelRunner::new(&work, WALL).capture(
                &cas,
                &sh("cat >/dev/null; touch ready; sleep 30"),
                b"input".to_vec(),
                Some(&flag),
            );
            assert!(cancelled.status.is_err());
        });
        // A timed-out check.
        let check = CheckRunner::new(&cas, &work)
            .with_timeout(Duration::from_millis(300))
            .run(&CheckDefinition::new("slow", sh("sleep 30")));
        assert!(check.reason.unwrap().contains("killed"));
        // A duplex git operation: a committed capture streams its objects through one
        // `cat-file --batch`.
        let repository = review_source_git::git::Repo::open(&repo, temp.path());
        review_source_git::capture::Capture::new(&repository, &cas)
            .committed("HEAD")
            .unwrap();
    });
    let process: Vec<&(String, String, Value, Value)> = spans
        .iter()
        .filter(|(_, kind, _, _)| kind == "process")
        .collect();
    let harness: Vec<_> = process
        .iter()
        .filter(|(name, ..)| name == "af.harness")
        .collect();
    assert_eq!(harness.len(), 2, "{spans:#?}");
    assert_eq!(harness[0].3["status"], "ok");
    assert_eq!(harness[0].2["gen_ai.operation.name"], "invoke_agent");
    assert_eq!(harness[0].2["gen_ai.provider.name"], "anthropic");
    assert_eq!(harness[0].3["attributes"]["process.exit_code"], 0);
    assert_eq!(harness[1].3["status"], "error");
    assert_eq!(harness[1].3["attributes"]["af.failure.class"], "refused");
    let check = process
        .iter()
        .find(|(name, ..)| name == "af.check")
        .unwrap_or_else(|| panic!("{spans:#?}"));
    assert_eq!(check.3["attributes"]["af.check.status"], "not_run");
    assert_eq!(check.3["attributes"]["af.failure.class"], "timed_out");
    let duplex = process
        .iter()
        .find(|(name, _, start, _)| name == "af.git" && start["af.git.op"] == "cat-file")
        .unwrap_or_else(|| panic!("a duplex git operation: {spans:#?}"));
    // The caller streamed stdout itself: the supervisor counted only stderr.
    assert!(duplex.3["attributes"].get("af.stdout_bytes").is_none());
    for (name, _, start, end) in [harness[0], harness[1], check, duplex] {
        let attributes = &end["attributes"];
        assert!(attributes["process.pid"].is_u64(), "{name}: {end}");
        assert!(attributes["af.rusage.user_ms"].is_u64(), "{name}: {end}");
        assert!(attributes["af.rusage.system_ms"].is_u64(), "{name}: {end}");
        assert!(attributes["af.stderr_bytes"].is_u64(), "{name}: {end}");
        assert!(attributes["af.stderr_held"].is_boolean(), "{name}: {end}");
        // Every span of the run names the run it worked for, taken from its parent.
        assert_eq!(start["af.run_id"], "run-1", "{name}");
        assert_eq!(start["af.execution_id"], "execution-1", "{name}");
        // A peak the platform did not deliver for this process is absent, never zero.
        if let Some(peak) = attributes.get("af.rusage.max_rss_bytes") {
            assert!(peak.as_u64().unwrap() > 0, "{name}: {end}");
        }
    }
}

/// Design §7.2: a process the platform reports nothing for — one that never started — records
/// its span without any `af.rusage.*` attribute or process id, rather than zeros; and a phase
/// outside every run records nothing at all.
#[test]
fn telemetry_rusage_is_absent_not_zero() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let spans = recorded(temp.path(), || {
        let unstarted = ModelRunner::new(temp.path(), WALL).capture(
            &cas,
            &Command::new("/nonexistent/af-harness-fixture", vec![]),
            b"input".to_vec(),
            None,
        );
        assert!(unstarted.status.is_err());
        let check = CheckRunner::new(&cas, temp.path()).run(&CheckDefinition::new(
            "missing",
            Command::new("/nonexistent/af-check-fixture", vec![]),
        ));
        assert!(check.reason.unwrap().contains("could not start"));
    });
    let process: Vec<_> = spans
        .iter()
        .filter(|(_, kind, _, _)| kind == "process")
        .collect();
    assert_eq!(process.len(), 2, "{spans:#?}");
    for (name, _, _, end) in process {
        let attributes = end["attributes"].as_object().unwrap();
        assert!(
            attributes.keys().all(|key| !key.starts_with("af.rusage.")),
            "{name}: {end}"
        );
        assert!(!attributes.contains_key("process.pid"), "{name}: {end}");
        assert!(
            !attributes.contains_key("process.exit_code"),
            "{name}: {end}"
        );
        assert_eq!(end["status"], "error", "{name}");
        assert_eq!(attributes["af.failure.class"], "unavailable", "{name}");
    }
    // Outside a run the same phases open nothing: no span becomes a root of no run.
    let writer = Writer::new(WriterOptions::new(
        temp.path().join("outside"),
        Level::Spans,
    ))
    .unwrap();
    let dispatch = tracing::Dispatch::new(TelemetrySubscriber::new(writer.clone()));
    tracing::dispatcher::with_default(&dispatch, || {
        let command = tracing::info_span!("af.command");
        let _command = command.enter();
        assert!(!review_telemetry::subscriber::in_run());
        CheckRunner::new(&cas, temp.path()).run(&CheckDefinition::new("outside", sh("true")));
    });
    writer.flush();
    let text = std::fs::read_to_string(writer.path().unwrap()).unwrap();
    writer.finish(SealExit::Ok).unwrap();
    assert!(!text.contains("\"af.check\""), "{text}");
    assert!(!text.contains("\"af.git\""), "{text}");
    assert!(!text.contains("\"af.store."), "{text}");
}

/// A model process that writes past its output cap and exits 0 fails its Attempt naming the cap,
/// and its `af.harness` span ends the same way, after that judgement: `error.type = "failed"`
/// with the cap's reason, the exit, the kept stream sizes and the resource use still those of
/// the capture.
#[test]
fn telemetry_harness_span_fails_past_the_output_cap() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let reason = "stdout was truncated at the 4 KiB output cap: it wrote 10000 bytes, of which \
                  only the first 4 KiB were kept";
    let spans = recorded(temp.path(), || {
        let capped = ModelRunner::new(&work, WALL)
            .with_harness("anthropic", Some("m".into()))
            .with_output_cap(4096)
            .capture(
                &cas,
                &sh("cat >/dev/null; head -c 10000 /dev/zero; exit 0"),
                b"input".to_vec(),
                None,
            );
        match capped.status {
            Err(review_runner::RunnerError::Failed {
                exit_code,
                stderr_excerpt,
            }) => {
                assert_eq!(exit_code, 0);
                assert_eq!(stderr_excerpt, reason);
            }
            other => panic!("the Attempt must fail naming the cap: {other:?}"),
        }
    });
    let harness: Vec<_> = spans
        .iter()
        .filter(|(name, kind, ..)| name == "af.harness" && kind == "process")
        .collect();
    assert_eq!(harness.len(), 1, "{spans:#?}");
    let end = &harness[0].3;
    let attributes = &end["attributes"];
    assert_eq!(end["status"], "error", "{end}");
    assert_eq!(attributes["error.type"], "failed", "{end}");
    assert_eq!(attributes["af.failure.class"], "failed", "{end}");
    assert_eq!(attributes["af.failure.reason"], reason, "{end}");
    assert_eq!(attributes["af.outcome"], "failed", "{end}");
    assert_eq!(attributes["process.exit_code"], 0, "{end}");
    assert_eq!(attributes["af.stdout_bytes"], 4096, "{end}");
    assert_eq!(attributes["af.stderr_bytes"], 0, "{end}");
    assert_eq!(attributes["af.stderr_held"], false, "{end}");
    assert!(attributes["process.pid"].is_u64(), "{end}");
    assert!(attributes["af.rusage.user_ms"].is_u64(), "{end}");
}
