//! Parity of the published schemas with what the crate writes: `af/telemetry-status@1`,
//! `af/telemetry-reindex@1`, `af/TelemetryDirs@1` and the segment records (ADR-0145).

use std::path::PathBuf;

use review_telemetry::blob::Media;
use review_telemetry::dirs;
use review_telemetry::level::Level;
use review_telemetry::record::{
    Attrs, ConfigRow, Demand, Finding, Node, ProblemWhat, SealExit, SpanKind, SpanStatus,
};
use review_telemetry::status::{self, StatusInput};
use serde_json::Value;

use crate::{sealed_lines, writer};

fn workspace_root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

pub(crate) fn validator(name: &str) -> jsonschema::Validator {
    let path = workspace_root().join("schemas").join(name);
    let schema: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

pub(crate) fn assert_valid(validator: &jsonschema::Validator, instance: &Value) {
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|error| format!("{error} at {}", error.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{}\n{instance:#}", errors.join("\n"));
}

/// A segment holding every record kind.
fn every_kind(root: &std::path::Path) -> std::path::PathBuf {
    let writer = writer(root, Level::Content);
    let command = writer
        .span_start(
            None,
            None,
            "af.command",
            SpanKind::Internal,
            Attrs::new().text("af.argv", "x".repeat(2000)),
        )
        .unwrap();
    writer.event_with_content(
        &command,
        "af.config.text",
        Attrs::new(),
        b"{\"a\":1}",
        Media::Json,
    );
    writer.config(
        "exec",
        vec![ConfigRow {
            key: "k".into(),
            value: "v".into(),
            layer: "user".into(),
            origin: Some("~/.config/af/config.toml:3".into()),
        }],
    );
    writer.node(Node {
        seq: 0,
        schema: String::new(),
        execution_id: "exec".into(),
        plan_digest: format!("sha256:{}", "1".repeat(64)),
        node_id: "implement".into(),
        kind: "model".into(),
        package: Some("implementer@1.0.0".into()),
        depends_on: vec!["capture".into()],
        expected_nodes: 2,
    });
    writer.finding(Finding {
        seq: 0,
        schema: String::new(),
        span_id: command.clone(),
        key: "f1".into(),
        severity: "major".into(),
        effective_severity: "minor".into(),
        status: "open".into(),
        scope: "line".into(),
        file: Some("src/lib.rs".into()),
        line: Some(3),
        title: "A title".into(),
        report_artifact_id: format!("sha256:{}", "2".repeat(64)),
        node_id: "review".into(),
        worker: "correctness@1.0.0".into(),
        model: Some("model".into()),
        round: Some(1),
        epoch: None,
    });
    writer.demand(Demand {
        seq: 0,
        schema: String::new(),
        span_id: command.clone(),
        demand_id: "d1".into(),
        kind: "evidence".into(),
        required: true,
        subject_id: "s".into(),
        snapshot_id: "n".into(),
        status: "open".into(),
    });
    writer.problem(ProblemWhat::Redaction, "a value was withheld");
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    writer.finish(SealExit::Ok).unwrap().unwrap().path
}

#[test]
fn every_record_kind_matches_the_record_schema() {
    let temp = tempfile::tempdir().unwrap();
    let path = every_kind(temp.path());
    let schema = validator("telemetry-record-v1.json");
    let lines = sealed_lines(&path);
    let kinds: std::collections::BTreeSet<String> = lines
        .iter()
        .map(|line| line["t"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds.len(), 12, "{kinds:?}");
    for line in &lines {
        assert_valid(&schema, line);
    }
    // Another version of a shape fails closed, in the schema and in the reader.
    let mut foreign = lines[1].clone();
    foreign["schema"] = Value::from("af/TelemetrySpan@2");
    assert!(!schema.is_valid(&foreign));
    assert!(review_telemetry::record::Record::parse_line(foreign.to_string().as_bytes()).is_err());
}

#[test]
fn status_and_reindex_documents_match_their_schemas() {
    let status_schema = validator("telemetry-status-v1.json");
    let reindex_schema = validator("telemetry-reindex-v1.json");
    let temp = tempfile::tempdir().unwrap();
    let input = |root: PathBuf| StatusInput {
        root,
        level: Level::Spans,
        max_bytes: 4 << 30,
        registry: Some(dirs::location(&temp.path().join("state"))),
        accounted: Vec::new(),
    };

    // A directory that does not exist, and an empty one.
    let (absent, _) = status::status(&input(temp.path().join("absent"))).unwrap();
    assert!(!absent.exists);
    assert_valid(&status_schema, &serde_json::to_value(&absent).unwrap());
    assert!(
        !temp.path().join("absent").exists(),
        "status creates nothing"
    );
    let empty = temp.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let (document, _) = status::status(&input(empty.clone())).unwrap();
    assert_eq!(document.bytes.total, 0);
    assert_valid(&status_schema, &serde_json::to_value(&document).unwrap());

    // Filled by the crate's own writer; then a live, a sealed and a corrupt segment, and a
    // directory of another version beside v1/.
    let filled = temp.path().join("filled");
    every_kind(&filled);
    let (document, _) = status::status(&input(filled.clone())).unwrap();
    assert_eq!(document.segments.sealed, 1);
    assert!(document.bytes.total > 0);
    assert_valid(&status_schema, &serde_json::to_value(&document).unwrap());

    let corrupt = writer(&filled, Level::Spans);
    corrupt.span_start(None, None, "af.command", SpanKind::Internal, Attrs::new());
    corrupt.span_start(None, None, "af.command", SpanKind::Internal, Attrs::new());
    corrupt.flush();
    let corrupt_path = corrupt.path().unwrap();
    let corrupt_id = corrupt.segment_id().unwrap();
    corrupt.abandon();
    let mut bytes = std::fs::read(&corrupt_path).unwrap();
    let second = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    bytes[second] = b'#';
    std::fs::write(&corrupt_path, bytes).unwrap();
    let layout = review_telemetry::layout::Layout::new(&filled);
    review_telemetry::segment::reader::recover(&layout, &corrupt_id).unwrap();
    let live = writer(&filled, Level::Spans);
    live.span_start(None, None, "af.command", SpanKind::Internal, Attrs::new());
    live.flush();
    std::fs::create_dir_all(filled.join("v0/segments")).unwrap();
    std::fs::write(filled.join("v0/segments/garbage.jsonl"), b"not a record\n").unwrap();
    let (document, _) = status::status(&input(filled.clone())).unwrap();
    assert_eq!(
        (
            document.segments.live,
            document.segments.sealed,
            document.segments.corrupt
        ),
        (1, 1, 1)
    );
    assert_eq!(document.unreadable.len(), 1);
    assert!(document.unreadable[0].path.ends_with("v0"));
    assert_valid(&status_schema, &serde_json::to_value(&document).unwrap());

    let reindexed = status::reindex(&filled).unwrap();
    assert_eq!(reindexed.segments, 2);
    assert_valid(&reindex_schema, &serde_json::to_value(&reindexed).unwrap());
    live.finish(SealExit::Ok).unwrap();
}

#[test]
fn the_registry_document_matches_its_schema() {
    let temp = tempfile::tempdir().unwrap();
    let location = dirs::location(temp.path());
    dirs::register(&location, &temp.path().join("telemetry"), &[]).unwrap();
    let document: Value = serde_json::from_slice(&std::fs::read(&location).unwrap()).unwrap();
    assert_eq!(document["schema"], "af/TelemetryDirs@1");
    assert_valid(&validator("telemetry-dirs-v1.json"), &document);
    // Another version is refused, never read.
    std::fs::write(&location, br#"{"schema":"af/TelemetryDirs@2","dirs":[]}"#).unwrap();
    assert!(dirs::read(&location).is_err());
}
