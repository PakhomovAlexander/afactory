//! Design §7.3 and §15 P6: the harness stream observer and tap end to end — a supervised
//! harness's chunks reach Telemetry's consumer as they arrive, never delaying the drain, and
//! `af telemetry turns` and `tools` read what the tap recorded from the telemetry directory
//! alone.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use review_core::{Arg, Command};
use review_runner::ModelRunner;
use review_runner::stream::HarnessStream;
use review_store::Cas;
use review_telemetry::level::Level;
use review_telemetry::record::{Attrs, Record, SealExit, SpanKind, SpanStatus};
use review_telemetry::segment::writer::{Writer, WriterOptions};
use review_telemetry::stream::{Harness, Release, Stream, Tap, TapOptions};
use review_telemetry::subscriber::{RecordedSpan, SpanIdentity};
use serde_json::{Value, json};

use crate::telemetry_ledger::Recording;

/// A shell script as a command.
fn sh(script: &str) -> Command {
    Command::new("/bin/sh", vec![Arg::literal("-c"), Arg::literal(script)])
}

/// The lines of a Claude `stream-json` session: init, one tool-calling turn whose two tools
/// answer, a final turn, the result. `secret` is model-authored text that must never be live.
fn claude_session(secret: &str) -> Vec<String> {
    [
        json!({"type":"system","subtype":"init","session_id":"5d2f8b9e-1c3a-4e7f-8a9b-0c1d2e3f4a5b",
               "model":"claude-fixture-1","tools":["Read","Grep","Bash"],"mcp_servers":[],
               "claude_code_version":"2.1.303"}),
        json!({"type":"assistant","message":{"id":"msg_01GoldenTurnOne","model":"claude-fixture-1",
               "stop_reason":"tool_use","usage":{"input_tokens":12,"output_tokens":34,
               "cache_read_input_tokens":5600,"cache_creation_input_tokens":78},
               "content":[{"type":"thinking","thinking":format!("plan {secret}")},
                          {"type":"tool_use","id":"toolu_01GoldenRead","name":"Read",
                           "input":{"file_path":"src/lib.rs"}},
                          {"type":"tool_use","id":"toolu_01GoldenGrep","name":"Grep",
                           "input":{"pattern":"TODO"}}]}}),
        json!({"type":"user","message":{"content":[
               {"type":"tool_result","tool_use_id":"toolu_01GoldenRead","content":"pub fn example() {}\n"}]}}),
        json!({"type":"user","message":{"content":[
               {"type":"tool_result","tool_use_id":"toolu_01GoldenGrep","content":"no matches",
                "is_error":true}]}}),
        json!({"type":"assistant","message":{"id":"msg_01GoldenTurnTwo","model":"claude-fixture-1",
               "stop_reason":"end_turn","usage":{"input_tokens":9,"output_tokens":21,
               "cache_read_input_tokens":5700,"cache_creation_input_tokens":0},
               "content":[{"type":"text","text":format!("done {secret}")}]}}),
        json!({"type":"result","subtype":"success","is_error":false,"num_turns":2,
               "result":format!("done {secret}"),"session_id":"5d2f8b9e-1c3a-4e7f-8a9b-0c1d2e3f4a5b",
               "usage":{"input_tokens":21,"output_tokens":55,"cache_creation_input_tokens":78}}),
    ]
    .iter()
    .map(Value::to_string)
    .collect()
}

// ------------------------------------------------------------------------------------------
// the goldens

const STORE: &str = "6666666666666666666666666666666666666666666666666666666666666666";
const RUN: &str = "task:sha256:8888888888888888888888888888888888888888888888888888888888888888";
const SEGMENT: &str = "1700000300000-4245-1a2b3c4d";

fn golden_path(name: &str) -> PathBuf {
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    workspace
        .join("crates/af/tests/it/telemetry_golden")
        .join(name)
}

/// Compare `actual` with the golden `name`; `AF_UPDATE_TELEMETRY_GOLDEN=1` rewrites it.
fn golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("AF_UPDATE_TELEMETRY_GOLDEN").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert_eq!(actual, expected, "{name} differs from its golden");
}

/// One execution of the golden run whose Attempt's Claude harness streamed `lines`, each
/// arriving 5 ms after the last: the command, run, node and Attempt spans as the kernel records
/// them, the `af.harness` span with what the observer counted, and the tap's records between.
fn record_golden(root: &Path, lines: &[String]) {
    let clock = Arc::new(AtomicU64::new(1_700_000_300_000 * 1_000_000));
    let mut options = WriterOptions::new(root, Level::Spans);
    options.argv = vec!["af".into(), "task".into(), "run".into(), "golden".into()];
    options.cwd = PathBuf::from("/work");
    options.af_version = "0.0.0-golden".into();
    options.af_commit = "0123456789ab".into();
    options.redactor = review_telemetry::redact::Redactor::new();
    options.segment_id = Some(SEGMENT.into());
    options.clock = Some(Arc::new(move || {
        u128::from(clock.fetch_add(1_000_000, Ordering::SeqCst))
    }));
    let writer = Writer::new(options).unwrap();
    let execution = review_telemetry::trace::Execution {
        store_id: STORE.into(),
        run_id: RUN.into(),
        execution_id: SEGMENT.into(),
        ordinal: 1,
        store_sequence_at_start: 2,
    };
    let trace = execution.trace_id();
    let command = writer
        .span_start(
            None,
            None,
            "af.command",
            SpanKind::Internal,
            Attrs::new().text("process.command", "af"),
        )
        .unwrap();
    let span = |name: &str, parent: &str, kind: SpanKind, attrs: Attrs| {
        writer
            .span_start(Some(&trace), Some(parent), name, kind, attrs)
            .unwrap()
    };
    let identity = execution.attributes().enumerated("af.run.kind", "task");
    let run = span("af.run", &command, SpanKind::Internal, identity.clone());
    let node = span(
        "af.node",
        &run,
        SpanKind::Internal,
        Attrs::new().id("af.node_id", "root.nodes.review"),
    );
    let attempt = span(
        "af.attempt",
        &node,
        SpanKind::Internal,
        Attrs::new()
            .id("af.attempt_id", "golden-attempt-1")
            .id("af.node_id", "root.nodes.review"),
    );
    let harness = span(
        "af.harness",
        &attempt,
        SpanKind::Process,
        Attrs::new()
            .enumerated("gen_ai.operation.name", "invoke_agent")
            .enumerated("gen_ai.provider.name", "anthropic")
            .text("gen_ai.request.model", "claude-fixture-1"),
    );
    let mut tap = Tap::new(
        &writer,
        TapOptions {
            harness: Harness::Claude,
            parent: SpanIdentity {
                span: RecordedSpan {
                    span_id: harness.clone(),
                    trace_id: Some(trace.clone()),
                },
                identity: identity.0,
                depth: 4,
            },
            provider: "anthropic",
            bound_model: Some("claude-fixture-1".into()),
            tools: vec!["Read".into(), "Glob".into(), "Grep".into()],
            frame_max_bytes: 16 << 20,
            content_allowance_bytes: 512 << 20,
            grants: vec![],
        },
    );
    let start = 1_700_000_300_010 * 1_000_000;
    let mut offset = 0;
    for (index, line) in lines.iter().enumerate() {
        let bytes = format!("{line}\n");
        tap.chunk(
            &writer,
            Stream::Stdout,
            offset,
            bytes.as_bytes(),
            start + index as u128 * 5_000_000,
        );
        offset += bytes.len() as u64;
    }
    tap.finish(
        &writer,
        &Release {
            withheld: false,
            native_withheld: false,
            stdout_bytes: offset,
            stderr_bytes: 0,
        },
    );
    writer.span_end(
        &harness,
        SpanStatus::Ok,
        Attrs::new()
            .int("dropped_chunks", 0)
            .int("gaps", 0)
            .boolean("af.stream.withheld", false)
            .int("process.exit_code", 0),
        4,
    );
    writer.span_end(&attempt, SpanStatus::Ok, Attrs::new(), 3);
    writer.span_end(&node, SpanStatus::Ok, Attrs::new(), 2);
    writer.span_end(&run, SpanStatus::Ok, Attrs::new(), 1);
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    writer.finish(SealExit::Ok).unwrap();
}

fn af(root: &Path, args: &[&str]) -> std::process::Output {
    crate::telemetry_ledger::af(root, args)
}

fn json_of(output: &std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Design §10: `af telemetry turns` and `tools` over one recorded Claude stream — two turns with
/// the harness's per-turn usage and stop reasons, two tool calls with their input and output
/// bytes, timed by arrival — in text and as `af/telemetry-turns@1` and `af/telemetry-tools@1`,
/// against goldens; each document matches its schema and refuses a member its schema does not
/// name; none carries a word the model wrote or an identifier the harness supplied.
#[test]
fn telemetry_turns_and_tools_match_their_goldens() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    record_golden(&root, &claude_session("model-authored words"));
    let trace = review_telemetry::trace::trace_id(STORE, RUN);
    let normalize = |text: String| {
        text.replace(&root.display().to_string(), "<root>")
            .replace(&trace, "<trace>")
    };
    let turns = json_of(&af(&root, &["telemetry", "turns", RUN, "--json"]));
    let tools = json_of(&af(&root, &["telemetry", "tools", RUN, "--json"]));
    for (name, document, pointers) in [
        (
            "telemetry-turns-v1.json",
            &turns,
            ["", "/run", "/turns/0", "/turns/0/harness", "/turns/0/usage"].as_slice(),
        ),
        (
            "telemetry-tools-v1.json",
            &tools,
            ["", "/run", "/tools/0", "/tools/0/harness"].as_slice(),
        ),
    ] {
        let validator = crate::schemas::validator(name);
        crate::schemas::valid(&validator, document);
        for pointer in pointers {
            let mut extended = document.clone();
            extended
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("unnamed".into(), Value::from(1));
            assert!(
                !validator.is_valid(&extended),
                "{name}{pointer} accepts an unnamed member"
            );
        }
        let text = document.to_string();
        for original in [
            "model-authored",
            "msg_01Golden",
            "toolu_01Golden",
            "5d2f8b9e",
        ] {
            assert!(!text.contains(original), "{name}: {original}");
        }
    }
    let rows = turns["turns"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "one row per assistant message: {turns}");
    assert_eq!(rows[0]["usage"]["cache_read_tokens"], "5600");
    assert_eq!(rows[0]["finish_reasons"], json!(["tool_use"]));
    assert_eq!(rows[0]["duration_us"], 5_000, "init to the first message");
    assert_eq!(rows[1]["finish_reasons"], json!(["end_turn"]));
    assert_eq!(rows[0]["harness"]["attempt_id"], "golden-attempt-1");
    assert_eq!(rows[0]["harness"]["analysis"], "complete");
    let calls = tools["tools"].as_array().unwrap();
    assert_eq!(calls.len(), 2, "one row per tool call: {tools}");
    assert_eq!(calls[0]["name"], "Read");
    assert_eq!(calls[0]["output_bytes"], 20);
    assert_eq!(calls[0]["duration_us"], 5_000);
    assert_eq!(calls[1]["duration_us"], 10_000);
    assert_eq!(calls[1]["outcome"], "failed");
    assert_eq!(calls[0]["turn_span_id"], rows[0]["span_id"]);
    golden(
        "turns.json",
        &normalize(serde_json::to_string_pretty(&turns).unwrap() + "\n"),
    );
    golden(
        "tools.json",
        &normalize(serde_json::to_string_pretty(&tools).unwrap() + "\n"),
    );
    for (command, name) in [("turns", "turns.txt"), ("tools", "tools.txt")] {
        let text = af(&root, &["telemetry", command, RUN]);
        assert!(text.status.success());
        golden(name, &normalize(String::from_utf8(text.stdout).unwrap()));
    }
    // `--attempt` keeps one Attempt's rows and refuses an unknown one.
    let one = json_of(&af(
        &root,
        &[
            "telemetry",
            "tools",
            RUN,
            "--attempt",
            "golden-attempt-1",
            "--json",
        ],
    ));
    assert_eq!(one["tools"].as_array().unwrap().len(), 2);
    let unknown = af(&root, &["telemetry", "turns", RUN, "--attempt", "nope"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("no Attempt nope"));
}

// ------------------------------------------------------------------------------------------
// the observer under a real supervisor

/// A Recording of a fresh run at `level` with a 50 ms visibility bound, and its Store's CAS.
fn recording(level: Level) -> (tempfile::TempDir, PathBuf, Recording, Cas) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let store = temp.path().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let mut options = WriterOptions::new(&root, level);
    options.flush_latency_ms = 50;
    let run = "task:sha256:".to_string() + &"9".repeat(64);
    let recording = Recording::with_options(options, &store, &run, 1);
    let cas = Cas::open(store.join("cas")).unwrap();
    (temp, root, recording, cas)
}

/// Every record the writer flushed so far, from its live segment.
fn live_records(writer: &Writer) -> (Vec<Record>, String) {
    let path = writer.path().unwrap();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let records = text
        .lines()
        .filter_map(|line| Record::parse_line(line.as_bytes()).ok())
        .collect();
    (records, text)
}

fn events_named<'a>(records: &'a [Record], name: &str) -> Vec<&'a review_telemetry::record::Event> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::Event(event) if event.name == name => Some(event),
            _ => None,
        })
        .collect()
}

/// Design §7.3: a stream far smaller than `flush_bytes` whose last line comes after the
/// harness's last turn, its process waiting until `af telemetry tail` has shown that line's
/// structural record: the record is in the tail while the process still runs, and nothing the
/// line carried is anywhere in the segment.
#[test]
fn telemetry_observer_sees_lines_before_exit() {
    let (temp, root, recording, cas) = recording(Level::Spans);
    let marker = temp.path().join("seen");
    let lines = [
        json!({"type":"system","subtype":"init","session_id":"session-before-exit"}).to_string(),
        json!({"type":"assistant","message":{"id":"msg_last_turn","stop_reason":"end_turn",
               "content":[{"type":"text","text":"the last turn"}]}})
        .to_string(),
        json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning",
               "note":"words after the last turn"}})
        .to_string(),
    ];
    // The child waits at most the load-safe wall (ADR-0114) for the tail to show its last line.
    let script = format!(
        "printf '%s\\n' '{}'; i=0; while [ ! -e '{}' ] && [ $i -lt 2400 ]; do sleep 0.05; i=$((i+1)); done; [ -e '{}' ]",
        lines.join("\n"),
        marker.display(),
        marker.display()
    );
    // The test's private home and XDG directories (`common::af`).
    let mut tail = crate::common::af();
    tail.args(["telemetry", "tail", "--json", "--dir"])
        .arg(&root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let shown = Arc::new(AtomicBool::new(false));
    let capture = recording.within(|| {
        let runner = ModelRunner::new(temp.path(), Duration::from_secs(120))
            .with_harness("anthropic", Some("claude-fixture-1".into()));
        std::thread::scope(|scope| {
            let watcher = scope.spawn(|| {
                // The harness span opens when the process starts; follow the live segment.
                let deadline = Instant::now() + Duration::from_secs(60);
                while recording.writer.path().is_none() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                let mut child = tail.spawn().unwrap();
                let reader = std::io::BufReader::new(child.stdout.take().unwrap());
                use std::io::BufRead as _;
                for line in reader.lines() {
                    let line = line.unwrap();
                    if line.contains("af.harness.rate_limit") {
                        assert!(!line.contains("words after"), "{line}");
                        // Live, before the process ends: structure only, the harness's
                        // identifiers as correlation ids.
                        let (_, live) = live_records(&recording.writer);
                        for words in [
                            "words after",
                            "the last turn",
                            "session-before-exit",
                            "msg_last_turn",
                        ] {
                            assert!(!live.contains(words), "{words} is never live");
                        }
                        shown.store(true, Ordering::SeqCst);
                        std::fs::write(&marker, b"seen").unwrap();
                        break;
                    }
                }
                let _ = child.kill();
                let _ = child.wait();
            });
            let capture = runner.capture_observed(
                &cas,
                &sh(&script),
                vec![],
                None,
                &HarnessStream::claude("Read,Glob,Grep"),
                |_, _, _| {},
            );
            watcher.join().unwrap();
            capture
        })
    });
    assert!(
        shown.load(Ordering::SeqCst),
        "tail showed the line before exit"
    );
    assert!(
        capture.status.as_ref().is_ok_and(|status| status.success()),
        "the process exited only after tail showed the line: {:?}",
        capture.status
    );
    recording.writer.flush();
    let (records, text) = live_records(&recording.writer);
    assert_eq!(events_named(&records, "af.harness.rate_limit").len(), 1);
    // The words never reach a record; the identifiers only the ids event after the decision.
    for words in ["words after", "the last turn"] {
        assert!(!text.contains(words), "{words} is never recorded");
    }
    recording.finish();
}

/// Design §7.3: a cancelled Attempt: the supervisor returns `Cancelled` with what the process
/// wrote, the tap ends every open turn and tool as `incomplete` once the decision is taken, and
/// the `af.harness` span records the observer's counts beside the refusal.
#[test]
fn telemetry_observer_survives_cancellation() {
    let (temp, _root, recording, cas) = recording(Level::Spans);
    let lines = [
        json!({"type":"system","subtype":"init","session_id":"s"}).to_string(),
        json!({"type":"assistant","message":{"id":"msg_cancelled","content":[
            {"type":"tool_use","id":"toolu_cancelled","name":"Read","input":{}}]}})
        .to_string(),
    ];
    let script = format!("printf '%s\\n' '{}'; sleep 30", lines.join("\n"));
    let flag = AtomicBool::new(false);
    let capture = recording.within(|| {
        let runner = ModelRunner::new(temp.path(), Duration::from_secs(120))
            .with_harness("anthropic", Some("claude-fixture-1".into()));
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(500));
                flag.store(true, Ordering::Release);
            });
            runner.capture_observed(
                &cas,
                &sh(&script),
                vec![],
                Some(&flag),
                &HarnessStream::claude("Read"),
                |_, _, _| {},
            )
        })
    });
    assert!(matches!(
        capture.status,
        Err(review_runner::RunnerError::Failed { .. })
    ));
    assert!(String::from_utf8_lossy(&capture.stdout).contains("toolu_cancelled"));
    let writer = recording.writer.clone();
    recording.finish();
    let sealed = review_telemetry::segment::reader::read_sealed(
        &writer
            .layout()
            .sealed_segment(&writer.segment_id().unwrap()),
    )
    .unwrap();
    let ended = |name: &str| -> Vec<serde_json::Map<String, Value>> {
        let ids: Vec<String> = sealed
            .iter()
            .filter_map(|record| match record {
                Record::SpanStart(start) if start.name == name => Some(start.span_id.clone()),
                _ => None,
            })
            .collect();
        sealed
            .iter()
            .filter_map(|record| match record {
                Record::SpanEnd(end) if ids.contains(&end.span_id) => Some(end.attributes.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(ended("af.tool")[0]["af.outcome"], "incomplete");
    assert_eq!(ended("af.turn").len(), 1);
    let harness = &ended("af.harness")[0];
    assert_eq!(harness["af.failure.class"], "refused");
    assert_eq!(harness["dropped_chunks"], 0);
    assert_eq!(harness["af.stream.withheld"], false);
    assert_eq!(events_named(&sealed, "af.harness.ids").len(), 1);
}

/// Design §7.3: a consumer stalled in a blocked write leaves the drain, the collected output and
/// the supervision outcome as they are without Telemetry: the full stream is collected in its
/// own time, the observer's queue fills and drops, and the command's bounded shutdown abandons
/// the consumer.
#[test]
fn telemetry_stalled_consumer_never_blocks_drain() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let store = temp.path().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = std::sync::Mutex::new(held);
    let mut options = WriterOptions::new(&root, Level::Spans);
    options.flush_latency_ms = 50;
    options.observer_queue_bytes = 256 * 1024;
    // Every write after the first blocks until released: a write held on the volume.
    options.write_hook = Some(Arc::new(move |ordinal| {
        if ordinal > 1 {
            let _ = held.lock().unwrap().recv_timeout(Duration::from_secs(30));
        }
        Ok(())
    }));
    let run = "task:sha256:".to_string() + &"4".repeat(64);
    let recording = Recording::with_options(options, &store, &run, 1);
    let cas = Cas::open(store.join("cas")).unwrap();
    let line = json!({"type":"user","message":{"content":[{"type":"tool_result",
        "tool_use_id":"t","content":"x".repeat(4000)}]}})
    .to_string();
    // 8 MiB of stream lines, as fast as the pipe takes them.
    let script =
        format!("i=0; while [ $i -lt 2048 ]; do printf '%s\\n' '{line}'; i=$((i+1)); done");
    let started = Instant::now();
    let (capture, baseline) = recording.within(|| {
        let runner = ModelRunner::new(temp.path(), Duration::from_secs(120));
        let capture = runner.capture_observed(
            &cas,
            &sh(&script),
            vec![],
            None,
            &HarnessStream::claude("Read"),
            |_, _, _| {},
        );
        let baseline = runner.capture(&cas, &sh(&script), vec![], None);
        (capture, baseline)
    });
    let elapsed = started.elapsed();
    assert!(capture.status.as_ref().is_ok_and(|status| status.success()));
    assert_eq!(
        capture.stdout, baseline.stdout,
        "the drain collected everything"
    );
    assert_eq!(capture.stdout.len(), 2048 * (line.len() + 1));
    assert!(
        elapsed < Duration::from_secs(25),
        "the drain was never held: {elapsed:?}"
    );
    // The command's shutdown is bounded: the consumer is abandoned, the segment left unsealed.
    let shutdown = Instant::now();
    let writer = recording.writer.clone();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| recording.finish())).ok();
    assert!(shutdown.elapsed() < Duration::from_secs(10));
    assert!(writer.abandoned() || writer.failed());
    drop(release);
}

// ------------------------------------------------------------------------------------------
// the Claude fake through the Task runtime

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// Every committed event of the Store at `state`, as text.
fn store_events(state: &Path) -> Vec<Vec<String>> {
    let connection = rusqlite::Connection::open_with_flags(
        state.join("events.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = connection
        .prepare(
            "SELECT run_id, sequence, event_id, type, occurred_at, node_id, attempt_id,
                    causation_id, correlation_id, artifact_refs, payload
             FROM events ORDER BY run_id, sequence",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            (0..11)
                .map(|index| {
                    row.get_ref(index).map(|value| match value {
                        rusqlite::types::ValueRef::Null => "null".to_string(),
                        rusqlite::types::ValueRef::Integer(number) => number.to_string(),
                        rusqlite::types::ValueRef::Text(text)
                        | rusqlite::types::ValueRef::Blob(text) => {
                            String::from_utf8_lossy(text).into_owned()
                        }
                        rusqlite::types::ValueRef::Real(number) => number.to_string(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Every object of the Store's CAS, by its digest's hex, with its bytes.
fn cas_objects(state: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut objects = std::collections::BTreeMap::new();
    for prefix in std::fs::read_dir(state.join("cas/objects"))
        .unwrap()
        .flatten()
    {
        let head = prefix.file_name().to_string_lossy().into_owned();
        for object in std::fs::read_dir(prefix.path()).unwrap().flatten() {
            let name = object.file_name().to_string_lossy().into_owned();
            if !name.starts_with('.') && object.path().is_file() {
                objects.insert(head.clone() + &name, std::fs::read(object.path()).unwrap());
            }
        }
    }
    objects
}

/// Every entry beneath `path`, `path` included, children before their directory, symlinks
/// reported as neither file nor directory and never followed.
fn walk_entries(path: &Path, visit: &mut dyn FnMut(&Path, std::fs::FileType)) {
    let kind = std::fs::symlink_metadata(path).unwrap().file_type();
    if kind.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            walk_entries(&entry.unwrap().path(), visit);
        }
    }
    visit(path, kind);
}

/// The fixture repository `source` copied to `repo` and committed there. The gate materializes
/// the source tree read-only and `cp -R` keeps those modes, so before anything writes into the
/// copy or runs git in it every directory gets `0o700` and every file `0o600` on top of its
/// mode, symlinks untouched (`copy_writable` of `scripts/telemetry-overhead.py`).
fn fixture_repo(source: &Path, repo: &Path) {
    use std::os::unix::fs::PermissionsExt;
    assert!(
        std::process::Command::new("cp")
            .arg("-R")
            .arg(source)
            .arg(repo)
            .status()
            .unwrap()
            .success()
    );
    walk_entries(repo, &mut |path, kind| {
        let wanted = if kind.is_dir() {
            0o700
        } else if kind.is_file() {
            0o600
        } else {
            return;
        };
        let mode = std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | wanted)).unwrap();
    });
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "fixture"],
    ] {
        let done = std::process::Command::new("git")
            .current_dir(repo)
            .args(&args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .env("GIT_AUTHOR_DATE", "2026-10-21T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-10-21T00:00:00Z")
            .status()
            .unwrap();
        assert!(done.success(), "git {args:?} in {}", repo.display());
    }
}

/// The one-Attempt invocation fixture's Task run once at `level`, its Claude Worker the scripted
/// harness of `fixtures/telemetry-bench/invocation-claude.py` printing the shape af asks for, with
/// three chunks of intermediate tool output: the machine, the run's Store and its inspection.
fn claude_task(level: &str) -> (crate::telemetry_runs::Machine, PathBuf, Value) {
    use std::os::unix::fs::PermissionsExt;
    let machine = crate::telemetry_runs::Machine::new();
    let fixture = workspace().join("fixtures/telemetry-bench/telemetry-bench-invocation-one");
    let repo = machine.root.join("repo");
    fixture_repo(&fixture.join("repo"), &repo);
    let bin = machine.root.join("bin");
    let auth = machine.root.join("auth");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&auth).unwrap();
    std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::copy(
        workspace().join("fixtures/telemetry-bench/invocation-claude.py"),
        bin.join("claude"),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        auth.join("bench.json"),
        json!({"harness":{"shape":"requested","tool_output_bytes":3 * 65536,"chunk_bytes":65536,
                          "report":false}})
        .to_string(),
    )
    .unwrap();
    let providers = machine.root.join("providers.toml");
    std::fs::write(
        &providers,
        format!(
            "version = 1\n\n[[providers]]\nid = \"claude-bench\"\nkind = \"claude\"\nauth_dir = \"{}\"\n",
            auth.display()
        ),
    )
    .unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let state = machine.root.join("store");
    let output = machine
        .command(
            &repo,
            &[
                ("AF_TELEMETRY__LEVEL", level),
                ("AF_PROVIDERS_FILE", providers.to_str().unwrap()),
                ("PATH", path.to_str().unwrap()),
                ("USER", "fixture"),
            ],
            &[
                "task",
                "start",
                "--execute",
                "--file",
                fixture.join("task.json").to_str().unwrap(),
                "--state",
                state.to_str().unwrap(),
                "--json",
            ],
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{level}: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let inspection: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(inspection["phase"]["kind"], "finished", "{level}");
    (machine, state, inspection)
}

/// The gate materializes the source tree read-only: `claude_task`'s copy of a read-only fixture
/// repository accepts `git init`, `add` and `commit` (`fixture_repo` runs them) and a written
/// file, and every directory and file of the copy outside git's own objects carries user write.
#[test]
fn telemetry_streams_fixture_copies_are_writable() {
    use std::os::unix::fs::PermissionsExt;
    let machine = crate::telemetry_runs::Machine::new();
    let source = machine.root.join("read-only/repo");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    assert!(
        std::process::Command::new("cp")
            .arg("-R")
            .arg(workspace().join("fixtures/telemetry-bench/telemetry-bench-invocation-one/repo"))
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    let mut files = 0;
    walk_entries(&source, &mut |path, kind| {
        let mode = if kind.is_dir() {
            0o555
        } else if kind.is_file() {
            files += 1;
            0o444
        } else {
            return;
        };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    });
    assert!(files > 0, "the fixture repository has files");
    assert!(std::fs::write(source.join("written.txt"), b"x").is_err());
    let repo = machine.root.join("repo");
    fixture_repo(&source, &repo);
    std::fs::write(repo.join("written.txt"), b"written\n").unwrap();
    let head = std::process::Command::new("git")
        .current_dir(&repo)
        .args(["rev-parse", "--verify", "HEAD"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(head.status.success(), "{head:?}");
    walk_entries(&repo, &mut |path, kind| {
        if path.strip_prefix(&repo).unwrap().starts_with(".git") {
            return;
        }
        let wanted = if kind.is_dir() {
            0o700
        } else if kind.is_file() {
            0o600
        } else {
            return;
        };
        let mode = std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & wanted, wanted, "{}: {mode:o}", path.display());
    });
    walk_entries(&source, &mut |path, kind| {
        let mode = if kind.is_dir() { 0o755 } else { 0o644 };
        if kind.is_dir() || kind.is_file() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
    });
}

/// The raw evidence the Store records for the Claude harness's Attempts (the Worker's and the
/// Provider admission probe's), in order: the CAS ids its usage and settlement records name
/// whose object is the harness's `result` line.
fn raw_evidence(inspection: &Value, state: &Path) -> Vec<String> {
    let objects = cas_objects(state);
    inspection["execution_records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|record| record["record"].get("raw_artifact_ids"))
        .flat_map(|ids| ids.as_array().unwrap().clone())
        .filter_map(|id| id.as_str().map(str::to_string))
        .filter(|id| {
            objects
                .get(id.strip_prefix("sha256:").unwrap_or(id))
                .is_some_and(|bytes| bytes.starts_with(b"{\"type\":\"result\""))
        })
        .collect()
}

/// Design §7.3: on the Claude fake through the Task runtime, every level gives the Store the
/// same raw evidence for every Attempt — the `result` line alone (the harness writes no stderr
/// here), never a stream message, so the same CAS ids — the same accounting and the same event
/// kinds in the same order; and at `spans` and `content` `af telemetry turns` shows one row per
/// assistant message with its usage and stop reason and `tools` one row per tool call with its
/// input and output bytes.
#[test]
fn telemetry_store_bytes_do_not_grow_with_level() {
    let mut stores = Vec::new();
    for level in ["off", "metrics", "spans", "content"] {
        let (machine, state, inspection) = claude_task(level);
        let objects = cas_objects(&state);
        for (name, bytes) in &objects {
            let text = String::from_utf8_lossy(bytes);
            assert!(
                !text.contains("\"type\":\"assistant\"") && !text.contains("tool_result"),
                "{level}: {name} holds a stream message"
            );
        }
        assert!(
            objects.values().any(|bytes| bytes
                .starts_with(b"{\"type\":\"result\",\"subtype\":\"success\"")
                && bytes.windows(15).any(|window| window == b"af.worker-reply")),
            "{level}: the result line is kept"
        );
        if matches!(level, "spans" | "content") {
            let root = machine.telemetry();
            let runs = json_of(&af(&root, &["telemetry", "runs", "--json"]));
            let run = runs["runs"][0]["run_id"].as_str().unwrap().to_string();
            let turns = json_of(&af(&root, &["telemetry", "turns", &run, "--json"]));
            crate::schemas::valid(
                &crate::schemas::validator("telemetry-turns-v1.json"),
                &turns,
            );
            let worker: Vec<&Value> = turns["turns"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|turn| turn["harness"]["node_id"] == "root.nodes.stream")
                .collect();
            // Three tool-calling messages and the final one.
            assert_eq!(worker.len(), 4, "{level}: {turns}");
            assert_eq!(worker[0]["usage"]["input_tokens"], "10");
            assert_eq!(worker[0]["finish_reasons"], json!(["tool_use"]));
            assert_eq!(worker[3]["finish_reasons"], json!(["end_turn"]));
            assert_eq!(worker[0]["harness"]["analysis"], "complete");
            let tools = json_of(&af(&root, &["telemetry", "tools", &run, "--json"]));
            let calls: Vec<&Value> = tools["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|tool| tool["harness"]["node_id"] == "root.nodes.stream")
                .collect();
            assert_eq!(calls.len(), 3, "{level}: {tools}");
            for call in &calls {
                assert_eq!(call["output_bytes"], 65536, "{call}");
                assert_eq!(call["input_bytes"], 22, "{call}");
                assert_eq!(call["name"], "Read", "{call}");
            }
        }
        let kinds: Vec<String> = store_events(&state)
            .into_iter()
            .map(|event| event[3].clone())
            .collect();
        stores.push((level, kinds, raw_evidence(&inspection, &state), inspection));
    }
    let (_, kinds, evidence, inspection) = &stores[0];
    assert_eq!(
        evidence.len(),
        4,
        "the probe's and the Worker's usage and settlement"
    );
    for (level, other_kinds, other_evidence, other_inspection) in &stores[1..] {
        assert_eq!(other_kinds, kinds, "the Store's event kinds at {level}");
        assert_eq!(other_evidence, evidence, "the raw evidence at {level}");
        for field in ["chargeable_tokens", "attempts"] {
            assert_eq!(
                other_inspection[field], inspection[field],
                "{field} at {level}"
            );
        }
    }
}
