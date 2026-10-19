//! `af telemetry status` and `af telemetry reindex` end to end (ADR-0153), each over a private
//! HOME and private XDG roots, against directories the telemetry crate's own writer filled.

use std::path::{Path, PathBuf};
use std::process::Output;

use review_telemetry::blob::Media;
use review_telemetry::layout::Layout;
use review_telemetry::level::Level;
use review_telemetry::record::{Attrs, ProblemWhat, SealExit, SpanKind, SpanStatus};
use review_telemetry::segment::writer::{Writer, WriterOptions};
use serde_json::Value;

use crate::schemas;

/// A private machine: every root af reads lives below one temporary directory.
struct Machine {
    _root: tempfile::TempDir,
    root: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        for name in ["home", "config", "state", "data", "cache"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        Self {
            _root: directory,
            root,
        }
    }

    /// The configured telemetry directory.
    fn telemetry(&self) -> PathBuf {
        self.root.join("state/af/telemetry")
    }

    fn af(&self, env: &[(&str, &str)], args: &[&str]) -> Output {
        self.af_in(&self.root, env, args)
    }

    fn af_in(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
        let mut command = crate::common::af();
        command
            .current_dir(cwd)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("AF_SELF_OFFLINE", "1")
            .env("NO_COLOR", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("AF_VERSION")
            .env_remove("AF_TELEMETRY__LEVEL")
            .args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    }
}

fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn json(output: &Output) -> Value {
    let (stdout, stderr) = text(output);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|_| panic!("{stdout}\n{stderr}"))
}

fn writer(root: &Path, level: Level) -> Writer {
    let mut options = WriterOptions::new(root, level);
    options.argv = vec!["af".into(), "task".into(), "run".into()];
    options.cwd = PathBuf::from("/work");
    Writer::new(options).unwrap()
}

/// One sealed run with a blob and a problem, as the crate's own writer leaves it.
fn sealed_run(root: &Path, run_id: &str) {
    let writer = writer(root, Level::Content);
    let execution = review_telemetry::trace::Execution {
        store_id: "c".repeat(64),
        run_id: run_id.into(),
        execution_id: "exec".into(),
        ordinal: 1,
        store_sequence_at_start: 1,
    };
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let run = writer
        .span_start(
            Some(&execution.trace_id()),
            Some(&command),
            "af.run",
            SpanKind::Internal,
            execution.attributes().enumerated("af.run.kind", "task"),
        )
        .unwrap();
    writer.event_with_content(
        &run,
        "gen_ai.content.input",
        Attrs::new(),
        b"API_KEY=sk-FIXTURE\nthe composed input\n",
        Media::Text,
    );
    writer.problem(ProblemWhat::Redaction, "a value was withheld");
    writer.span_end(&run, SpanStatus::Ok, Attrs::new(), 1);
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    writer.finish(SealExit::Ok).unwrap();
}

#[test]
fn telemetry_status_validates_on_every_directory_shape() {
    let machine = Machine::new();
    let schema = schemas::validator("telemetry-status-v1.json");

    // Nothing written yet: the configured directory does not exist, and status creates nothing.
    let absent = json(&machine.af(&[], &["telemetry", "status", "--json"]));
    schemas::valid(&schema, &absent);
    assert_eq!(absent["exists"], false);
    assert_eq!(absent["level"], "spans");
    assert!(!machine.telemetry().exists());

    // An empty directory named with --dir.
    let empty = machine.root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let document = json(&machine.af(
        &[],
        &[
            "telemetry",
            "status",
            "--json",
            "--dir",
            empty.to_str().unwrap(),
        ],
    ));
    schemas::valid(&schema, &document);
    assert_eq!(document["exists"], true);
    assert_eq!(document["bytes"]["total"], 0);

    // A directory the crate's own writer filled.
    sealed_run(&machine.telemetry(), "run-1");
    let document = json(&machine.af(&[], &["telemetry", "status", "--json"]));
    schemas::valid(&schema, &document);
    assert_eq!(document["segments"]["sealed"], 1);
    assert_eq!(document["runs"], 1);
    assert_eq!(document["index"]["state"], "fresh");
    assert_eq!(document["problems"][0]["what"], "redaction");
    assert!(document["bytes"]["total"].as_u64().unwrap() > 0);

    // A live, a sealed and a corrupt segment, and a directory of another version.
    let layout = Layout::new(machine.telemetry());
    let broken = writer(&machine.telemetry(), Level::Spans);
    broken.span_start(None, None, "af.command", SpanKind::Internal, Attrs::new());
    broken.span_start(None, None, "af.command", SpanKind::Internal, Attrs::new());
    broken.flush();
    let broken_path = broken.path().unwrap();
    let broken_id = broken.segment_id().unwrap();
    broken.abandon();
    let mut bytes = std::fs::read(&broken_path).unwrap();
    let second = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    bytes[second] = b'#';
    std::fs::write(&broken_path, bytes).unwrap();
    review_telemetry::segment::reader::recover(&layout, &broken_id).unwrap();
    let live = writer(&machine.telemetry(), Level::Spans);
    live.span_start(None, None, "af.command", SpanKind::Internal, Attrs::new());
    live.flush();
    std::fs::create_dir_all(machine.telemetry().join("v0/segments")).unwrap();
    std::fs::write(
        machine.telemetry().join("v0/segments/1-1-00.jsonl"),
        b"{\"t\":\"from another version\"}\n",
    )
    .unwrap();
    let document = json(&machine.af(&[], &["telemetry", "status", "--json"]));
    schemas::valid(&schema, &document);
    assert_eq!(document["segments"]["live"], 1);
    assert_eq!(document["segments"]["sealed"], 1);
    assert_eq!(document["segments"]["corrupt"], 1);
    let unreadable = document["unreadable"].as_array().unwrap();
    assert_eq!(unreadable.len(), 1, "{unreadable:?}");
    assert!(unreadable[0]["path"].as_str().unwrap().ends_with("/v0"));
    let (stdout, _) = text(&machine.af(&[], &["telemetry", "status"]));
    assert!(
        stdout.contains("1 live · 0 unsealed · 1 sealed · 1 corrupt"),
        "{stdout}"
    );
    assert!(stdout.contains("another telemetry version"), "{stdout}");
    live.finish(SealExit::Ok).unwrap();
}

#[test]
fn telemetry_reindex_answers_identically() {
    let machine = Machine::new();
    sealed_run(&machine.telemetry(), "run-1");
    sealed_run(&machine.telemetry(), "run-2");
    // A crashed writer's segment, recovered by the reindex.
    let crashed = writer(&machine.telemetry(), Level::Spans);
    let span = crashed
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    crashed.event(&span, "af.note", Attrs::new().int("n", 1));
    crashed.flush();
    crashed.abandon();

    let before = json(&machine.af(&[], &["telemetry", "status", "--json"]));
    assert_eq!(before["segments"]["unsealed"], 1);
    let layout = Layout::new(machine.telemetry());
    let dump = |layout: &Layout| {
        let mut index = review_telemetry::index::Index::open(layout).unwrap();
        index.ingest().unwrap();
        index.dump().unwrap()
    };
    let reindexed = json(&machine.af(&[], &["telemetry", "reindex", "--json"]));
    schemas::valid(&schemas::validator("telemetry-reindex-v1.json"), &reindexed);
    assert_eq!(reindexed["recovered"].as_array().unwrap().len(), 1);
    assert_eq!(reindexed["segments"], 3);
    assert_eq!(reindexed["missing_blobs"], 0);
    let first = dump(&layout);
    let again = json(&machine.af(&[], &["telemetry", "reindex", "--json"]));
    assert!(again["recovered"].as_array().unwrap().is_empty());
    assert_eq!(
        dump(&layout),
        first,
        "a rebuild answers every query identically"
    );
    let after = json(&machine.af(&[], &["telemetry", "status", "--json"]));
    for key in ["runs", "missing_blobs", "problems", "withheld_attempts"] {
        assert_eq!(after[key], before[key], "{key}");
    }
    assert_eq!(after["segments"]["sealed"], 3);
    assert_eq!(after["segments"]["unsealed"], 0);
    let (stdout, _) = text(&machine.af(&[], &["telemetry", "reindex"]));
    assert!(stdout.contains("segments     3"), "{stdout}");
}

#[test]
fn telemetry_commands_refuse_and_report_with_their_exit_codes() {
    let machine = Machine::new();
    // An unreadable store: 1, naming it.
    let missing = machine.root.join("nowhere");
    for command in ["status", "reindex"] {
        let output = machine.af(
            &[],
            &["telemetry", command, "--dir", missing.to_str().unwrap()],
        );
        assert_eq!(output.status.code(), Some(1), "{command}");
        assert!(text(&output).1.contains("nowhere"), "{}", text(&output).1);
    }
    // A refused key: 2, naming it, under --json as one document.
    let output = machine.af(
        &[("AF_TELEMETRY__LEVEL", "verbose")],
        &["telemetry", "status", "--json"],
    );
    assert_eq!(output.status.code(), Some(2));
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema"], "af/error@1");
    assert_eq!(document["exit_code"], 2);
    assert!(
        document["error"]
            .as_str()
            .unwrap()
            .contains("[telemetry] level")
    );
    // A wrong command line: 2.
    assert_eq!(
        machine
            .af(&[], &["telemetry", "status", "--nope"])
            .status
            .code(),
        Some(2)
    );
    // The one-run override is honoured.
    let content = json(&machine.af(
        &[("AF_TELEMETRY__LEVEL", "content")],
        &["telemetry", "status", "--json"],
    ));
    assert_eq!(content["level"], "content");
    assert_eq!(content["perturbing"], true);

    // A project layer's [telemetry] is reported and ignored, exactly as [storage] is.
    let repo = machine.root.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::create_dir_all(repo.join(".af")).unwrap();
    std::fs::write(
        repo.join(".af/af.toml"),
        "version = 1\n[storage]\nauto_gc = true\n[telemetry]\nlevel = \"off\"\n",
    )
    .unwrap();
    let (stdout, stderr) = text(&machine.af_in(&repo, &[], &["config", "show"]));
    assert!(
        stdout.contains("# ignored: [telemetry] in "),
        "{stdout}\n{stderr}"
    );
    assert!(stdout.contains("# ignored: [storage] in "), "{stdout}");
    assert!(stdout.contains("telemetry.level = \"spans\""), "{stdout}");
    let status = json(&machine.af_in(&repo, &[], &["telemetry", "status", "--json"]));
    assert_eq!(status["level"], "spans");
}
