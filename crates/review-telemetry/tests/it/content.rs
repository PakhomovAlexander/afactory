//! The content level (design §6 Blobs, §8, §15 P7): content blobs redacted whole before any
//! truncation with their source identity intact, the Attempt and process allowances, the content
//! switches, the full-text index kept at ingest, and credentials withheld on every blob path
//! while the identities beside them survive.

use std::path::Path;

use review_telemetry::blob::{Document, Media};
use review_telemetry::content::{self, Scope, event};
use review_telemetry::layout::Layout;
use review_telemetry::level::Level;
use review_telemetry::record::{Attrs, BlobRef, BlobState, Record, SealExit, SpanKind, SpanStatus};
use review_telemetry::redact::{OVERSIZED_LINE, WITHHELD, WITHHELD_LINE};
use review_telemetry::segment::writer::Writer;
use review_telemetry::stream::{Harness, Release, Stream, Tap, TapOptions};
use review_telemetry::subscriber::{RecordedSpan, SpanIdentity};
use review_telemetry::trace::{Execution, sha256_digest};
use serde_json::{Value, json};

use crate::{options, sealed_records};

const STORE: &str = "5555555555555555555555555555555555555555555555555555555555555555";
const RUN: &str = "task:sha256:7777777777777777777777777777777777777777777777777777777777777777";
const ATTEMPT: &str = "content-attempt-1";
/// A 64-character lowercase hex credential, the shape a digest has without its `sha256:`.
const TOKEN_ID: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
/// A digest the fixtures plant beside the credentials: an identity, kept wherever it is one.
const DIGEST: &str = "sha256:abababababababababababababababababababababababababababababababab";

/// Credential material: an assignment under a secret name, a token prefix and a JSON Web Token.
fn credentials() -> [&'static str; 3] {
    [
        "password = hunter2-planted",
        "export OPENAI_KEY=sk-proj-plantedplantedplanted0123",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJwbGFudGVkIn0.c2lnbmF0dXJlLXBsYW50ZWQtaGVyZQ",
    ]
}

/// A run's spans in one segment at `level`: command, run, node, Attempt; their ids.
struct Run {
    writer: Writer,
    trace: String,
    identity: Attrs,
    run: String,
    attempt: String,
    command: String,
    node: String,
}

fn run(
    root: &Path,
    level: Level,
    configure: impl FnOnce(&mut review_telemetry::segment::writer::WriterOptions),
) -> Run {
    let mut options = options(root, level);
    configure(&mut options);
    let writer = Writer::new(options).unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let execution = Execution {
        store_id: STORE.into(),
        run_id: RUN.into(),
        execution_id: writer.segment_id().unwrap(),
        ordinal: 1,
        store_sequence_at_start: 1,
    };
    let trace = execution.trace_id();
    let identity = execution.attributes().enumerated("af.run.kind", "task");
    let run = writer
        .span_start(
            Some(&trace),
            Some(&command),
            "af.run",
            SpanKind::Internal,
            identity.clone(),
        )
        .unwrap();
    let node = writer
        .span_start(
            Some(&trace),
            Some(&run),
            "af.node",
            SpanKind::Internal,
            Attrs::new().id("af.node_id", "root.nodes.review"),
        )
        .unwrap();
    let attempt = writer
        .span_start(
            Some(&trace),
            Some(&node),
            "af.attempt",
            SpanKind::Internal,
            Attrs::new()
                .id("af.attempt_id", ATTEMPT)
                .id("af.node_id", "root.nodes.review"),
        )
        .unwrap();
    Run {
        writer,
        trace,
        identity,
        run,
        attempt,
        command,
        node,
    }
}

impl Run {
    fn span(&self, parent: &str, name: &str, kind: SpanKind) -> String {
        self.writer
            .span_start(Some(&self.trace), Some(parent), name, kind, Attrs::new())
            .unwrap()
    }

    fn finish(self) -> std::path::PathBuf {
        let writer = self.writer;
        writer.span_end(&self.attempt, SpanStatus::Ok, Attrs::new(), 3);
        writer.span_end(&self.node, SpanStatus::Ok, Attrs::new(), 2);
        writer.span_end(&self.run, SpanStatus::Ok, Attrs::new(), 1);
        writer.span_end(&self.command, SpanStatus::Ok, Attrs::new(), 0);
        writer.finish(SealExit::Ok).unwrap().unwrap().path
    }
}

fn occurrences(records: &[Record]) -> Vec<BlobRef> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::BlobRef(occurrence) => Some(occurrence.clone()),
            _ => None,
        })
        .collect()
}

fn event_named<'a>(records: &'a [Record], name: &str) -> Vec<&'a review_telemetry::record::Event> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::Event(event) if event.name == name => Some(event),
            _ => None,
        })
        .collect()
}

/// The retained bytes of the occurrence an event names.
fn retained(root: &Path, records: &[Record], event: &review_telemetry::record::Event) -> Vec<u8> {
    let occurrence = occurrences(records)
        .into_iter()
        .find(|occurrence| Some(&occurrence.occurrence_id) == event.blob.as_ref())
        .expect("the event names an occurrence");
    match review_telemetry::blob::reader::content(&Layout::new(root), &occurrence) {
        review_telemetry::blob::reader::Content::Bytes(bytes) => bytes,
        review_telemetry::blob::reader::Content::Unavailable(reason) => panic!("{reason}"),
    }
}

/// Design §6 Blobs: content is redacted whole — per line for text, as one document for JSON —
/// and only then cut head and tail to `content_max_blob_bytes`, so a credential placed across
/// either truncation boundary is withheld and never cut into view; a line or document longer
/// than the bound is withheld whole; the object's identity is the retained bytes', the
/// occurrence's `source_digest` and `original_bytes` the original's.
#[test]
fn telemetry_content_blob_bounds_and_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let max = 64 * 1024;
    let run = run(dir.path(), Level::Content, |options| {
        options.content_max_blob_bytes = max as u64;
    });
    let check = run.span(&run.node, "af.check", SpanKind::Process);
    // Filler lines around credentials placed where the head ends and the tail begins.
    let mut text = String::new();
    let mut line = 0;
    while text.len() < 3 * max {
        let at = text.len();
        let planted = (max / 2 - 300..max / 2 + 100).contains(&at)
            || (3 * max - max / 2 - 300..3 * max - max / 2 + 100).contains(&at);
        if planted {
            for credential in credentials() {
                text.push_str(&format!("near the cut {credential} beside {DIGEST}\n"));
            }
        }
        text.push_str(&format!(
            "check output line {line:06} carries ordinary words\n"
        ));
        line += 1;
    }
    // A line longer than the bound, judged never: withheld whole.
    let oversized = format!("oversized-start {} oversized-end\n", "y".repeat(max));
    let text = format!("{oversized}{text}");
    run.writer.content_event(
        &check,
        event::CHECK_OUTPUT,
        Attrs::new().enumerated("af.stream", "stderr"),
        text.as_bytes(),
        Media::Text,
        &Scope::Process,
    );
    // A JSON document over the bound is withheld whole; one within it keeps its structure.
    let big = json!({"payload": "z".repeat(max)}).to_string();
    run.writer.content_event(
        &run.run,
        "af.store.event",
        Attrs::new(),
        big.as_bytes(),
        Media::Kernel(Document::Record),
        &Scope::Process,
    );
    run.writer.span_end(&check, SpanStatus::Ok, Attrs::new(), 3);
    let path = run.finish();
    let records = sealed_records(&path);
    let output = &event_named(&records, event::CHECK_OUTPUT)[0];
    let bytes = retained(dir.path(), &records, output);
    let kept = String::from_utf8(bytes.clone()).unwrap();
    assert!(kept.len() <= max, "{}", kept.len());
    assert!(kept.starts_with(OVERSIZED_LINE), "{}", &kept[..80]);
    assert!(!kept.contains("oversized-start") && !kept.contains("yyyy"));
    assert!(kept.contains("bytes truncated"));
    for secret in ["hunter2", "sk-proj-planted", "eyJhbGciOiJIUzI1NiJ9"] {
        assert!(!kept.contains(secret), "{secret} survived a cut");
    }
    assert!(
        kept.contains(WITHHELD_LINE),
        "the planted lines are withheld lines"
    );
    let reference = occurrences(&records)
        .into_iter()
        .find(|occurrence| Some(&occurrence.occurrence_id) == output.blob.as_ref())
        .unwrap();
    assert!(reference.truncated);
    assert_eq!(reference.state, BlobState::Stored);
    assert_eq!(
        reference.digest.as_deref(),
        Some(sha256_digest(&bytes).as_str())
    );
    assert_eq!(reference.source_digest, sha256_digest(text.as_bytes()));
    assert_eq!(reference.original_bytes, text.len() as u64);
    assert_eq!(reference.media_type, "text/plain; charset=utf-8");
    let document = &event_named(&records, "af.store.event")[0];
    let kept: Value = serde_json::from_slice(&retained(dir.path(), &records, document)).unwrap();
    assert_eq!(
        kept,
        Value::String(WITHHELD.into()),
        "too long to judge whole"
    );
    // The same source under another bound is another object, related by its source digest.
    let other = tempfile::tempdir().unwrap();
    let run = run_at(other.path(), 128 * 1024);
    run.writer.content_event(
        &run.run,
        event::CHECK_OUTPUT,
        Attrs::new(),
        text.as_bytes(),
        Media::Text,
        &Scope::Process,
    );
    let records = sealed_records(&run.finish());
    let again = occurrences(&records).pop().unwrap();
    assert_eq!(again.source_digest, reference.source_digest);
    assert_ne!(again.digest, reference.digest);
}

fn run_at(root: &Path, max: u64) -> Run {
    run(root, Level::Content, |options| {
        options.content_max_blob_bytes = max
    })
}

/// Design §8 and §6: past `content_attempt_max_bytes` an Attempt's further content is dropped,
/// and past `content_process_max_bytes` the content outside any Attempt: each a `dropped`
/// occurrence with no object, never staged, counted in the seal's `dropped`, with one `budget`
/// problem per allowance; another Attempt keeps its own allowance, and every other record is
/// still written.
#[test]
fn content_allowances_drop_and_count_past_each_bound() {
    let dir = tempfile::tempdir().unwrap();
    let run = run(dir.path(), Level::Content, |options| {
        options.content_attempt_max_bytes = 10_000;
        options.content_process_max_bytes = 5_000;
    });
    let block = |tag: &str| format!("{tag} {}\n", "w".repeat(3_990));
    let attempt = Scope::Attempt(run.attempt.clone());
    let other = Scope::Attempt("another-attempt-span".into());
    for (index, scope) in [&attempt, &attempt, &attempt, &attempt, &other, &other]
        .into_iter()
        .enumerate()
    {
        run.writer.content_event(
            &run.attempt,
            event::OUTPUT,
            Attrs::new().int("index", index as i64),
            block(&format!("attempt-{index}")).as_bytes(),
            Media::Text,
            scope,
        );
    }
    for index in 0..3 {
        run.writer.content_event(
            &run.run,
            event::CONFIG_TEXT,
            Attrs::new().int("index", index),
            block(&format!("process-{index}")).as_bytes(),
            Media::Text,
            &Scope::Process,
        );
    }
    // The run continues: a later record is written as ever.
    run.writer
        .event(&run.run, "af.after", Attrs::new().int("still", 1));
    let path = run.finish();
    let records = sealed_records(&path);
    let states: Vec<(String, BlobState, bool)> = occurrences(&records)
        .into_iter()
        .map(|occurrence| {
            (
                occurrence.event_seq.to_string(),
                occurrence.state,
                occurrence.digest.is_some(),
            )
        })
        .collect();
    let stored = states
        .iter()
        .filter(|(_, state, _)| *state == BlobState::Stored)
        .count();
    let dropped: Vec<_> = states
        .iter()
        .filter(|(_, state, _)| *state == BlobState::Dropped)
        .collect();
    // Attempt: 2 of 4 fit 10,000 bytes; the other Attempt both; the process 1 of 3.
    assert_eq!(stored, 5, "{states:?}");
    assert_eq!(dropped.len(), 4, "{states:?}");
    assert!(
        dropped.iter().all(|(_, _, digest)| !digest),
        "never digested"
    );
    let seal = records
        .iter()
        .find_map(|record| match record {
            Record::Seal(seal) => Some(seal.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(seal.dropped, Some(4));
    let problems: Vec<String> = records
        .iter()
        .filter_map(|record| match record {
            Record::Problem(problem) => Some(problem.reason.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        problems,
        [content::ATTEMPT_EXHAUSTED, content::PROCESS_EXHAUSTED],
        "one problem per allowance"
    );
    assert_eq!(event_named(&records, "af.after").len(), 1);
    // Only what was stored reached `blobs/`.
    let files = walk(&dir.path().join("v1/blobs"));
    assert_eq!(files, 5, "one blob file per stored content");
}

fn walk(path: &Path) -> usize {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| {
            if entry.path().is_dir() {
                walk(&entry.path())
            } else {
                usize::from(entry.file_name().to_string_lossy().ends_with(".gz"))
            }
        })
        .sum()
}

/// The tap's options below `harness`, at `level`, with the content switches.
fn tap(run: &Run, harness: &str, reasoning: bool, tool_io: bool) -> Tap {
    Tap::new(
        &run.writer,
        TapOptions {
            harness: Harness::Claude,
            parent: SpanIdentity {
                span: RecordedSpan {
                    span_id: harness.to_string(),
                    trace_id: Some(run.trace.clone()),
                },
                identity: run.identity.0.clone(),
                depth: 4,
            },
            provider: "anthropic",
            bound_model: Some("claude-fixture-1".into()),
            tools: vec!["Read".into()],
            frame_max_bytes: 64 * 1024,
            content_allowance_bytes: 16 << 20,
            grants: vec![("HOME".into(), "/grants/home-value".into())],
            content_reasoning: reasoning,
            content_tool_io: tool_io,
            scope: Scope::Attempt(run.attempt.clone()),
        },
    )
}

/// A Claude stream whose thinking block and tool result carry `credential` beside `DIGEST`.
fn stream_lines(credential: &str) -> Vec<String> {
    [
        json!({"type":"system","subtype":"init","session_id":"session-content","model":"claude-fixture-1"}),
        json!({"type":"assistant","message":{"id":"msg_content_1","model":"claude-fixture-1",
               "stop_reason":"tool_use","content":[
                   {"type":"thinking","thinking":format!("weigh the lamp\n{credential} {DIGEST}")},
                   {"type":"tool_use","id":"toolu_content_1","name":"Read",
                    "input":{"file_path":"/grants/home-value/src/lib.rs"}}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result",
               "tool_use_id":"toolu_content_1","content":format!("fn lamp() {{}}\n{credential}\n")}]}}),
        json!({"type":"assistant","message":{"id":"msg_content_2","model":"claude-fixture-1",
               "stop_reason":"end_turn","content":[
                   {"type":"thinking","thinking":"trust the tide tables"},
                   {"type":"text","text":"the lighthouse answered"}]}}),
    ]
    .iter()
    .map(Value::to_string)
    .collect()
}

fn feed(tap: &mut Tap, writer: &Writer, lines: &[String]) -> u64 {
    let mut offset = 0;
    for (index, line) in lines.iter().enumerate() {
        let bytes = format!("{line}\n");
        tap.chunk(
            writer,
            Stream::Stdout,
            offset,
            bytes.as_bytes(),
            10 + index as u128,
        );
        offset += bytes.len() as u64;
    }
    offset
}

/// Design §8 and §15 P7: with `content_reasoning` off no thinking block is pended or kept, with
/// `content_tool_io` off no tool argument or result; the text is kept either way, and the
/// switches the content was kept under are on `af.harness.ids`.
#[test]
fn content_switches_keep_nothing_of_a_switched_off_kind() {
    for (reasoning, tool_io) in [(true, true), (false, true), (true, false), (false, false)] {
        let dir = tempfile::tempdir().unwrap();
        let run = run(dir.path(), Level::Content, |_| {});
        let harness = run.span(&run.attempt, "af.harness", SpanKind::Process);
        let mut tap = tap(&run, &harness, reasoning, tool_io);
        let bytes = feed(&mut tap, &run.writer, &stream_lines("plain words"));
        let released = tap.finish(
            &run.writer,
            &Release {
                stdout_bytes: bytes,
                ..Release::default()
            },
        );
        let kinds: Vec<&str> = released
            .contents
            .iter()
            .map(|content| content.kind)
            .collect();
        assert_eq!(kinds.contains(&"thinking"), reasoning, "{kinds:?}");
        assert_eq!(kinds.contains(&"tool_input"), tool_io, "{kinds:?}");
        assert_eq!(kinds.contains(&"tool_output"), tool_io, "{kinds:?}");
        assert!(kinds.contains(&"text"));
        run.writer
            .span_end(&harness, SpanStatus::Ok, Attrs::new(), 4);
        let records = sealed_records(&run.finish());
        assert_eq!(
            event_named(&records, event::REASONING).len(),
            if reasoning { 2 } else { 0 }
        );
        assert_eq!(
            event_named(&records, event::TOOL_INPUT).len()
                + event_named(&records, event::TOOL_OUTPUT).len(),
            if tool_io { 2 } else { 0 }
        );
        // Text: the assistant's block, and the result line's would be too.
        assert_eq!(event_named(&records, event::OUTPUT).len(), 1);
        let ids = &event_named(&records, "af.harness.ids")[0].attributes;
        assert_eq!(ids["af.content.reasoning"], reasoning);
        assert_eq!(ids["af.content.tool_io"], tool_io);
        let joined = walk_texts(dir.path());
        assert_eq!(joined.contains("tide tables"), reasoning);
        assert_eq!(joined.contains("fn lamp()"), tool_io);
    }
}

/// Every blob's retained text beneath `root`, joined.
fn walk_texts(root: &Path) -> String {
    let layout = Layout::new(root);
    let mut texts = String::new();
    for prefix in std::fs::read_dir(root.join("v1/blobs"))
        .into_iter()
        .flatten()
        .flatten()
    {
        for file in std::fs::read_dir(prefix.path()).unwrap().flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if let Some(hex) = name.strip_suffix(".gz")
                && let review_telemetry::blob::reader::Content::Bytes(bytes) =
                    review_telemetry::blob::reader::read(&layout, &format!("sha256:{hex}"))
            {
                texts.push_str(&String::from_utf8_lossy(&bytes));
                texts.push('\n');
            }
        }
    }
    texts
}

/// Design §16 item 5 through every blob path of P7: credentials planted in a tool result, a
/// thinking block, a check's stderr, a Store payload, a Finding body, a configuration text and
/// the composed Worker input are withheld in the blob, in the search index's excerpts and in
/// `context` and `reasoning`, while the identities beside them — the request's artifact and
/// Snapshot ids, the payload's identity members, every blob and source digest, the correlation
/// ids — are intact and every JSON document still parses.
#[test]
fn telemetry_content_credentials_withheld_on_every_blob_path() {
    let dir = tempfile::tempdir().unwrap();
    let run = run(dir.path(), Level::Content, |_| {});
    let [password, token, jwt] = credentials();
    // The composed Worker input and its manifest.
    let compose = run.span(&run.attempt, "af.context.compose", SpanKind::Internal);
    let request = json!({
        "schema": "af.worker-request/1",
        "instructions": format!("review the lamp\n{token}"),
        "inputs": {"source": [{"artifact_id": DIGEST, "artifact_type": "af/SourceTree@1",
                               "snapshot_id": DIGEST,
                               "payload": {"snapshot_id": DIGEST, "note": password}}],
                   "requirements": [{"artifact_id": DIGEST, "artifact_type": "af/Requirements@1",
                               "payload": {"specification": {"api_token_id": TOKEN_ID,
                                                             "package": "harbour"}}}]},
        "feedback": [],
    })
    .to_string();
    run.writer.content_event(
        &compose,
        event::INPUT,
        Attrs::new(),
        request.as_bytes(),
        Media::Kernel(Document::WorkerRequest),
        &Scope::Attempt(run.attempt.clone()),
    );
    run.writer.content_event(
        &compose,
        event::SYSTEM,
        Attrs::new(),
        json!({"entries": [{"name": "source", "artifact_id": DIGEST, "required_by": jwt}]})
            .to_string()
            .as_bytes(),
        Media::Kernel(Document::Record),
        &Scope::Attempt(run.attempt.clone()),
    );
    // The harness: a thinking block and a tool result.
    let harness = run.span(&run.attempt, "af.harness", SpanKind::Process);
    let mut tap = tap(&run, &harness, true, true);
    let bytes = feed(&mut tap, &run.writer, &stream_lines(password));
    tap.finish(
        &run.writer,
        &Release {
            stdout_bytes: bytes,
            ..Release::default()
        },
    );
    run.writer
        .span_end(&harness, SpanStatus::Ok, Attrs::new(), 4);
    // A check's stderr, a Store payload, a Finding body, a configuration text.
    let check = run.span(&run.node, "af.check", SpanKind::Process);
    run.writer.content_event(
        &check,
        event::CHECK_OUTPUT,
        Attrs::new().enumerated("af.stream", "stderr"),
        format!("error: lamp failed\n{token}\n").as_bytes(),
        Media::Text,
        &Scope::Process,
    );
    run.writer
        .span_end(&check, SpanStatus::Error, Attrs::new(), 3);
    run.writer.content_event(
        &run.run,
        "af.store.event",
        Attrs::new().id("af.store.event.id", "event-content-1"),
        json!({"artifact_id": DIGEST, "plan_id": DIGEST, "diagnostic": password,
               "nested": {"api_token": "violet", "subject_id": DIGEST}})
        .to_string()
        .as_bytes(),
        Media::Kernel(Document::Record),
        &Scope::Process,
    );
    run.writer.content_event(
        &run.run,
        event::FINDING_BODY,
        Attrs::new().id("af.finding.id", "finding-lamp"),
        format!("The lamp leaks.\n{jwt}\n").as_bytes(),
        Media::Text,
        &Scope::Process,
    );
    run.writer.content_event(
        &run.run,
        event::CONFIG_TEXT,
        Attrs::new().enumerated("af.config.text.kind", "af_toml"),
        format!("[review]\nmodel = \"lamp\"\n{password}\n").as_bytes(),
        Media::Text,
        &Scope::Process,
    );
    run.writer
        .span_end(&compose, SpanStatus::Ok, Attrs::new(), 4);
    let path = run.finish();
    let records = sealed_records(&path);
    // No blob holds a credential; every JSON blob parses; the identities beside them survive.
    let texts = walk_texts(dir.path());
    for secret in [
        "hunter2",
        "sk-proj-planted",
        "eyJhbGciOiJIUzI1NiJ9",
        "violet",
        "/grants/home-value",
        TOKEN_ID,
    ] {
        assert!(!texts.contains(secret), "{secret} reached a blob");
    }
    for occurrence in occurrences(&records) {
        assert_eq!(occurrence.state, BlobState::Stored, "{occurrence:?}");
        let bytes =
            match review_telemetry::blob::reader::content(&Layout::new(dir.path()), &occurrence) {
                review_telemetry::blob::reader::Content::Bytes(bytes) => bytes,
                review_telemetry::blob::reader::Content::Unavailable(reason) => panic!("{reason}"),
            };
        if occurrence.media_type == "application/json" {
            serde_json::from_slice::<Value>(&bytes).expect("a JSON blob still parses");
        }
        assert!(
            occurrence
                .digest
                .as_deref()
                .is_some_and(review_telemetry::redact::is_digest)
        );
        assert!(review_telemetry::redact::is_digest(
            &occurrence.source_digest
        ));
    }
    let input: Value = serde_json::from_slice(&retained(
        dir.path(),
        &records,
        event_named(&records, event::INPUT)[0],
    ))
    .unwrap();
    let source = &input["inputs"]["source"][0];
    assert_eq!(source["artifact_id"], DIGEST);
    assert_eq!(source["snapshot_id"], DIGEST);
    assert_eq!(source["payload"]["snapshot_id"], DIGEST);
    assert_eq!(source["payload"]["note"], WITHHELD);
    assert_eq!(input["instructions"], WITHHELD, "judged whole as one value");
    // The requirements are untrusted JSON: a credential under an `_id` name is no identity.
    let specification = &input["inputs"]["requirements"][0]["payload"]["specification"];
    assert_eq!(specification["api_token_id"], WITHHELD);
    assert_eq!(specification["package"], "harbour");
    assert_eq!(input["inputs"]["requirements"][0]["artifact_id"], DIGEST);
    let payload: Value = serde_json::from_slice(&retained(
        dir.path(),
        &records,
        event_named(&records, "af.store.event")[0],
    ))
    .unwrap();
    assert_eq!(payload["artifact_id"], DIGEST);
    assert_eq!(payload["plan_id"], DIGEST);
    assert_eq!(payload["nested"]["subject_id"], DIGEST);
    assert_eq!(payload["diagnostic"], WITHHELD);
    assert_eq!(payload["nested"]["api_token"], WITHHELD);
    assert_eq!(
        event_named(&records, "af.store.event")[0].attributes["af.store.event.id"],
        "event-content-1"
    );
    // The index's excerpts and the readers.
    let (index, read, unreadable) = review_telemetry::view::open(dir.path()).unwrap();
    let found = review_telemetry::content_view::search(
        &index,
        dir.path(),
        read.clone(),
        unreadable.clone(),
        "lamp",
        None,
        None,
    )
    .unwrap();
    // The Finding body, the tool result, the check's stderr, the thought and the configuration
    // text say it.
    assert_eq!(found.matches.len(), 5, "{found:?}");
    for found in &found.matches {
        for secret in ["hunter2", "sk-proj", "eyJhbGci"] {
            assert!(!found.excerpt.contains(secret), "{}", found.excerpt);
        }
        assert!(review_telemetry::redact::is_digest(&found.digest));
    }
    for secret in [
        "hunter2",
        "plantedplanted",
        "eyJhbGciOiJIUzI1NiJ9",
        TOKEN_ID,
    ] {
        let none = review_telemetry::content_view::search(
            &index,
            dir.path(),
            read.clone(),
            unreadable.clone(),
            secret,
            None,
            None,
        )
        .unwrap();
        assert!(none.matches.is_empty(), "{secret}: {none:?}");
    }
    let trace = run_trace();
    let run_of = (STORE, RUN, trace.as_str(), Some("task".to_string()));
    let context = review_telemetry::content_view::context(
        &index,
        dir.path(),
        read.clone(),
        unreadable.clone(),
        (run_of.0, run_of.1, run_of.2, run_of.3.clone()),
        ATTEMPT,
    )
    .unwrap();
    assert_eq!(context.state, "captured");
    let shown = serde_json::to_string(&context).unwrap();
    assert!(
        !shown.contains("hunter2") && !shown.contains("sk-proj") && !shown.contains("eyJhbGci")
    );
    assert!(!shown.contains(TOKEN_ID), "{shown}");
    assert!(shown.contains(DIGEST));
    let reasoning = review_telemetry::content_view::reasoning(
        &index,
        dir.path(),
        read,
        unreadable,
        (run_of.0, run_of.1, run_of.2, run_of.3),
        ATTEMPT,
    )
    .unwrap();
    assert_eq!(reasoning.thoughts.len(), 2);
    assert_eq!(
        reasoning.thoughts[0].part.content.as_deref(),
        Some(format!("weigh the lamp\n{WITHHELD_LINE}").as_str())
    );
    assert_eq!(
        reasoning.thoughts[1].part.content.as_deref(),
        Some("trust the tide tables")
    );
}

fn run_trace() -> String {
    review_telemetry::trace::trace_id(STORE, RUN)
}

/// Design §6 Index: `content_fts` is kept at ingest over the text blobs the retained
/// occurrences reference, identical after a reindex; with `content_index` off ingest adds
/// nothing to it; a blob no occurrence references any more leaves it.
#[test]
fn content_index_is_kept_at_ingest_and_rebuilt_identically() {
    let dir = tempfile::tempdir().unwrap();
    let run = run(dir.path(), Level::Content, |_| {});
    for words in [
        "the periwinkle lamp",
        "the tide tables",
        "the periwinkle lamp",
    ] {
        run.writer.content_event(
            &run.attempt,
            event::OUTPUT,
            Attrs::new(),
            words.as_bytes(),
            Media::Text,
            &Scope::Attempt(run.attempt.clone()),
        );
    }
    run.finish();
    let layout = Layout::new(dir.path());
    let mut index = review_telemetry::index::Index::open(&layout).unwrap();
    index.keep_content_index(false);
    index.ingest().unwrap();
    assert_eq!(index.count("SELECT count(*) FROM content_fts").unwrap(), 0);
    assert!(index.count("SELECT count(*) FROM blob_refs").unwrap() > 0);
    drop(index);
    let (index, read, unreadable) = review_telemetry::view::open(dir.path()).unwrap();
    assert_eq!(
        index.count("SELECT count(*) FROM content_fts").unwrap(),
        2,
        "one row per object"
    );
    let found = review_telemetry::content_view::search(
        &index,
        dir.path(),
        read,
        unreadable,
        "periwinkle lamp",
        None,
        None,
    )
    .unwrap();
    assert_eq!(found.matches.len(), 1);
    assert_eq!(
        found.matches[0].occurrences.len(),
        2,
        "both occurrences of the one object"
    );
    let before = index.dump().unwrap();
    drop(index);
    review_telemetry::status::reindex(dir.path()).unwrap();
    let (index, _, _) = review_telemetry::view::open(dir.path()).unwrap();
    assert_eq!(index.dump().unwrap(), before, "identical after a reindex");
    // Content an exhausted allowance or a sweep dropped is never indexed: an empty phrase or a
    // phrase of no blob finds nothing.
    let (index, read, unreadable) = review_telemetry::view::open(dir.path()).unwrap();
    let none = review_telemetry::content_view::search(
        &index,
        dir.path(),
        read,
        unreadable,
        "lantern",
        None,
        None,
    )
    .unwrap();
    assert!(none.matches.is_empty());
}

/// Design §7.3 and §16 item 5 at `content`: a stream whose release decision withholds it — the
/// adapter replaced it as login-challenge material, or Telemetry found such material itself —
/// leaves no content event, no blob and no search hit, its text, thinking and tool content
/// included; the structure recorded live stays.
#[test]
fn a_withheld_stream_leaves_no_blob_and_no_search_hit() {
    for adapter_replaced in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let run = run(dir.path(), Level::Content, |_| {});
        let harness = run.span(&run.attempt, "af.harness", SpanKind::Process);
        let mut tap = tap(&run, &harness, true, true);
        let lines = stream_lines("plain words");
        let bytes = feed(&mut tap, &run.writer, &lines);
        let stdout = lines.join("\n");
        let stderr = if adapter_replaced {
            String::new()
        } else {
            "Open https://claude.ai/oauth/authorize?code=true and enter the code ABCD-EFGH\n"
                .to_string()
        };
        let release = Release::decide(
            stdout.as_bytes(),
            stderr.as_bytes(),
            &[],
            adapter_replaced,
            false,
        )
        .with_sizes(bytes, 0);
        assert!(release.withheld, "{adapter_replaced}");
        let released = tap.finish(&run.writer, &release);
        assert!(released.withheld && released.contents.is_empty());
        run.writer
            .span_end(&harness, SpanStatus::Ok, Attrs::new(), 4);
        let records = sealed_records(&run.finish());
        for name in [
            event::OUTPUT,
            event::REASONING,
            event::TOOL_INPUT,
            event::TOOL_OUTPUT,
        ] {
            assert!(event_named(&records, name).is_empty(), "{name}");
        }
        assert!(occurrences(&records).is_empty());
        assert_eq!(walk(&dir.path().join("v1/blobs")), 0);
        assert_eq!(event_named(&records, "af.turn.api_retry").len(), 0);
        assert_eq!(
            event_named(&records, "af.harness.ids")[0].attributes["af.stream.withheld"],
            true
        );
        let (index, read, unreadable) = review_telemetry::view::open(dir.path()).unwrap();
        for words in ["lighthouse", "tide tables", "fn lamp"] {
            let none = review_telemetry::content_view::search(
                &index,
                dir.path(),
                read.clone(),
                unreadable.clone(),
                words,
                None,
                None,
            )
            .unwrap();
            assert!(none.matches.is_empty(), "{words}");
        }
        let trace = run_trace();
        let reasoning = review_telemetry::content_view::reasoning(
            &index,
            dir.path(),
            read,
            unreadable,
            (STORE, RUN, trace.as_str(), Some("task".into())),
            ATTEMPT,
        )
        .unwrap();
        assert_eq!(reasoning.state, "withheld");
        assert!(reasoning.thoughts.is_empty());
    }
}

/// One search over `root`, as `af telemetry search TEXT` runs it.
fn search_in(root: &Path, text: &str) -> review_telemetry::content_view::Search {
    let (index, read, unreadable) = review_telemetry::view::open(root).unwrap();
    review_telemetry::content_view::search(&index, root, read, unreadable, text, None, None)
        .unwrap()
}

/// Design §10 `search` is bounded whatever the corpus: with more matching blobs than it lists it
/// keeps [`SEARCH_LIMIT`] and says `more`, with more occurrences of one blob than it lists it
/// keeps [`OCCURRENCE_LIMIT`] and says so, and it reads no candidate past the one that says
/// `more` — the blob files of every later candidate are gone, and none of them is read (none is
/// reported unreadable, none leaves the index). The `--since` and `--run` filters are applied in
/// the query: a window after every occurrence and another run find nothing.
#[test]
fn telemetry_search_reads_a_bounded_number_of_matches() {
    use review_telemetry::content_view::{OCCURRENCE_LIMIT, SEARCH_LIMIT};
    let dir = tempfile::tempdir().unwrap();
    let run = run(dir.path(), Level::Content, |_| {});
    for number in 0..SEARCH_LIMIT + 20 {
        run.writer.content_event(
            &run.attempt,
            event::OUTPUT,
            Attrs::new(),
            format!("the periwinkle lamp number {number}").as_bytes(),
            Media::Text,
            &Scope::Attempt(run.attempt.clone()),
        );
    }
    for _ in 0..OCCURRENCE_LIMIT + 5 {
        run.writer.content_event(
            &run.attempt,
            event::OUTPUT,
            Attrs::new(),
            b"the periwinkle lamp, said again",
            Media::Text,
            &Scope::Attempt(run.attempt.clone()),
        );
    }
    run.finish();
    let found = search_in(dir.path(), "periwinkle lamp");
    assert_eq!(found.matches.len(), SEARCH_LIMIT);
    assert!(found.more);
    assert!(found.unreadable.is_empty(), "{:?}", found.unreadable);
    let repeated = search_in(dir.path(), "said again");
    assert_eq!(repeated.matches.len(), 1);
    assert!(!repeated.more);
    assert_eq!(repeated.matches[0].occurrences.len(), OCCURRENCE_LIMIT);
    assert!(repeated.matches[0].more_occurrences);
    for found in &found.matches {
        assert!(found.occurrences.len() <= OCCURRENCE_LIMIT);
    }
    // Every candidate after the one that said `more`, in digest order, loses its blob file: a
    // search that read past the bound would report them and take them out of the index.
    let layout = Layout::new(dir.path());
    let (index, _, _) = review_telemetry::view::open(dir.path()).unwrap();
    let mut digests: Vec<String> = {
        let mut statement = index
            .connection()
            .prepare("SELECT digest FROM content_fts ORDER BY digest")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let indexed = digests.len() as u64;
    assert_eq!(indexed, SEARCH_LIMIT as u64 + 21);
    drop(index);
    for digest in digests.split_off(SEARCH_LIMIT + 1) {
        std::fs::remove_file(layout.blob(&digest)).unwrap();
    }
    let again = search_in(dir.path(), "periwinkle lamp");
    assert_eq!(again.matches.len(), SEARCH_LIMIT);
    assert!(again.more);
    assert!(again.unreadable.is_empty(), "{:?}", again.unreadable);
    let (index, read, unreadable) = review_telemetry::view::open(dir.path()).unwrap();
    assert_eq!(
        index.count("SELECT count(*) FROM content_fts").unwrap(),
        indexed
    );
    // The filters are part of the query.
    let later = review_telemetry::content_view::search(
        &index,
        dir.path(),
        read.clone(),
        unreadable.clone(),
        "periwinkle lamp",
        Some(i64::MAX),
        None,
    )
    .unwrap();
    assert!(later.matches.is_empty() && !later.more, "{later:?}");
    let other = review_telemetry::content_view::search(
        &index,
        dir.path(),
        read,
        unreadable,
        "periwinkle lamp",
        None,
        Some(("task:other", &"0".repeat(32))),
    )
    .unwrap();
    assert!(other.matches.is_empty() && !other.more, "{other:?}");
}

/// Design §6 Blobs: a search shows an excerpt only of a blob it read and verified under
/// `gc.lock`, as the content readers do. Once a referenced blob file is removed, or its bytes no
/// longer match its digest, the search that finds it in `content_fts` lists it unreadable with
/// the reader's reason, shows nothing of it, and takes its text out of the index (its
/// `content_index` row says `unavailable`), so a later search returns nothing stale either.
#[test]
fn telemetry_search_verifies_each_blob_before_its_excerpt() {
    let dir = tempfile::tempdir().unwrap();
    let run = run(dir.path(), Level::Content, |_| {});
    for words in [
        "the amber lamp is lit",
        "the amber lamp is dim",
        "the amber lamp is out",
    ] {
        run.writer.content_event(
            &run.attempt,
            event::OUTPUT,
            Attrs::new(),
            words.as_bytes(),
            Media::Text,
            &Scope::Attempt(run.attempt.clone()),
        );
    }
    run.finish();
    assert_eq!(search_in(dir.path(), "amber lamp").matches.len(), 3);
    let layout = Layout::new(dir.path());
    let missing = sha256_digest(b"the amber lamp is dim");
    let corrupt = sha256_digest(b"the amber lamp is out");
    std::fs::remove_file(layout.blob(&missing)).unwrap();
    std::fs::write(layout.blob(&corrupt), b"not a gzip stream").unwrap();
    for (digest, reason) in [
        (&missing, review_telemetry::blob::reader::MISSING),
        (&corrupt, "content unavailable: corrupt blob"),
    ] {
        assert_eq!(
            review_telemetry::blob::reader::read(&layout, digest),
            review_telemetry::blob::reader::Content::Unavailable(reason.into()),
            "the reader's answer"
        );
    }
    let found = search_in(dir.path(), "amber lamp");
    assert_eq!(found.matches.len(), 1, "{found:?}");
    assert_eq!(
        found.matches[0].digest,
        sha256_digest(b"the amber lamp is lit")
    );
    let shown = serde_json::to_string(&found.matches).unwrap();
    assert!(
        !shown.contains("dim") && !shown.contains("is out"),
        "{shown}"
    );
    let reported: std::collections::BTreeSet<(String, String)> = found
        .unreadable
        .iter()
        .map(|unreadable| (unreadable.path.clone(), unreadable.reason.clone()))
        .collect();
    let expected: std::collections::BTreeSet<(String, String)> = [
        (&missing, review_telemetry::blob::reader::MISSING),
        (&corrupt, "content unavailable: corrupt blob"),
    ]
    .into_iter()
    .map(|(digest, reason)| {
        (
            layout.blob(digest).display().to_string(),
            reason.to_string(),
        )
    })
    .collect();
    assert_eq!(reported, expected);
    assert_eq!(found.unreadable.len(), 2);
    // The index is in step: neither text is searchable any more, and both are `unavailable`.
    let (index, _, _) = review_telemetry::view::open(dir.path()).unwrap();
    assert_eq!(index.count("SELECT count(*) FROM content_fts").unwrap(), 1);
    assert_eq!(
        index
            .count("SELECT count(*) FROM content_index WHERE state = 'unavailable'")
            .unwrap(),
        2
    );
    drop(index);
    let later = search_in(dir.path(), "amber lamp");
    assert_eq!(later.matches.len(), 1);
    assert!(later.unreadable.is_empty(), "{:?}", later.unreadable);
    assert!(search_in(dir.path(), "lamp is dim").matches.is_empty());
}

/// Design §8 `content_index`: with it off, ingest keeps no full-text index — `content_fts` and
/// `content_index` are emptied though blobs a content run indexed are still referenced — and
/// with it on again ingest rebuilds both from the retained blobs, as they were.
#[test]
fn telemetry_content_index_off_clears_the_derived_tables() {
    let dir = tempfile::tempdir().unwrap();
    let run = run(dir.path(), Level::Content, |_| {});
    for words in ["the periwinkle lamp", "the tide tables"] {
        run.writer.content_event(
            &run.attempt,
            event::OUTPUT,
            Attrs::new(),
            words.as_bytes(),
            Media::Text,
            &Scope::Attempt(run.attempt.clone()),
        );
    }
    run.finish();
    let layout = Layout::new(dir.path());
    let tables = |index: &review_telemetry::index::Index| {
        let dump = index.dump().unwrap();
        (dump["content_fts"].clone(), dump["content_index"].clone())
    };
    let mut index = review_telemetry::index::Index::open(&layout).unwrap();
    index.keep_content_index(true);
    index.ingest().unwrap();
    let kept = tables(&index);
    assert_eq!(kept.0.len(), 2);
    assert_eq!(kept.1.len(), 2);
    drop(index);
    let mut index = review_telemetry::index::Index::open(&layout).unwrap();
    index.keep_content_index(false);
    index.ingest().unwrap();
    assert_eq!(index.count("SELECT count(*) FROM content_fts").unwrap(), 0);
    assert_eq!(
        index.count("SELECT count(*) FROM content_index").unwrap(),
        0
    );
    assert!(index.count("SELECT count(*) FROM blob_refs").unwrap() > 0);
    drop(index);
    let mut index = review_telemetry::index::Index::open(&layout).unwrap();
    index.keep_content_index(true);
    index.ingest().unwrap();
    assert_eq!(tables(&index), kept, "rebuilt from the retained blobs");
}

/// Design §8: an allowance counts the content Telemetry kept, so a piece that fits but that
/// publication drops — a sweep holds `gc.lock` — is refunded: once the lock is released another
/// piece that fits beside the first is stored, for an Attempt's allowance and the process's
/// alike, and neither allowance records itself exhausted.
#[test]
fn telemetry_content_dropped_by_publication_is_refunded() {
    let dir = tempfile::tempdir().unwrap();
    let run = run(dir.path(), Level::Content, |options| {
        options.content_attempt_max_bytes = 100;
        options.content_process_max_bytes = 100;
    });
    let layout = Layout::new(dir.path());
    for scope in [Scope::Attempt(run.attempt.clone()), Scope::Process] {
        for (piece, held) in [(1, true), (2, false)] {
            let sweep = held.then(|| {
                review_telemetry::layout::exclusive_within(
                    &layout.gc_lock(),
                    std::time::Duration::from_secs(3600),
                )
                .unwrap()
                .unwrap()
            });
            run.writer.content_event(
                &run.attempt,
                event::OUTPUT,
                Attrs::new(),
                format!("{} {scope:?} {piece}", "x".repeat(55)).as_bytes(),
                Media::Text,
                &scope,
            );
            drop(sweep);
        }
    }
    let path = run.finish();
    let records = sealed_records(&path);
    let states: Vec<BlobState> = occurrences(&records)
        .iter()
        .map(|occurrence| occurrence.state)
        .collect();
    assert_eq!(
        states,
        [
            BlobState::Dropped,
            BlobState::Stored,
            BlobState::Dropped,
            BlobState::Stored
        ]
    );
    // Dropped for the lock, with a digest: never for the allowance.
    for occurrence in occurrences(&records) {
        assert!(occurrence.digest.is_some(), "{occurrence:?}");
    }
    assert!(
        !records.iter().any(|record| matches!(record,
            Record::Problem(problem)
                if problem.what == review_telemetry::record::ProblemWhat::Budget)),
        "no allowance was exhausted"
    );
}
