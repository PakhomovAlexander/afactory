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

use crate::{allocated, sealed_lines, writer};

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
        &crate::config_owner("exec"),
        "configuration",
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
        log: "campaign-1".into(),
        sequence: 3,
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
        log: "campaign-1".into(),
        sequence: 3,
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
    // Another version of a shape fails closed, in the schema and in the reader: a later one, and
    // the earlier shapes of the records whose shape changed (ADR-0145).
    let mut foreign = lines[1].clone();
    foreign["schema"] = Value::from("af/TelemetrySpan@2");
    assert!(!schema.is_valid(&foreign));
    assert!(review_telemetry::record::Record::parse_line(foreign.to_string().as_bytes()).is_err());
    for (kind, earlier) in [
        ("config", "af/TelemetryConfig@1"),
        ("finding", "af/TelemetryFinding@1"),
        ("demand", "af/TelemetryDemand@1"),
    ] {
        let mut old = lines.iter().find(|line| line["t"] == kind).unwrap().clone();
        old["schema"] = Value::from(earlier);
        assert!(!schema.is_valid(&old), "{earlier}");
        assert!(
            review_telemetry::record::Record::parse_line(old.to_string().as_bytes()).is_err(),
            "{earlier} is read"
        );
    }
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
        accounted_unknown: None,
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

#[test]
fn telemetry_status_accounts_every_byte_under_the_root() {
    let temp = tempfile::tempdir().unwrap();
    let input = |root: PathBuf| StatusInput {
        root,
        level: Level::Spans,
        max_bytes: 4 << 30,
        registry: Some(dirs::location(&temp.path().join("state"))),
        accounted: Vec::new(),
        accounted_unknown: None,
    };
    let schema = validator("telemetry-status-v1.json");
    let root = temp.path().join("telemetry");
    every_kind(&root);
    let planted = [
        ("v1/segments/unknown.bin", vec![7_u8; 10_000]),
        ("v1/stray/deeper/file.txt", vec![1_u8; 5_000]),
        ("notes.txt", b"left here".to_vec()),
        ("v0/segments/old.jsonl", b"not read\n".to_vec()),
        ("v1/blobs/unknown.bin", vec![3_u8; 6_000]),
        ("v1/blobs/ab/stray.txt", vec![4_u8; 7_000]),
    ];
    for (path, bytes) in &planted {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
    }
    let (document, _) = status::status(&input(root.clone())).unwrap();
    assert_valid(&schema, &serde_json::to_value(&document).unwrap());
    assert_eq!(document.bytes.total, allocated(&root));
    // One shape, one version: the document with its `unrecognized` entry validates against
    // `af/telemetry-status@1`, and a field the schema does not name is rejected wherever it is
    // added, in the schema and in the reader.
    let value = serde_json::to_value(&document).unwrap();
    let position = value["bytes"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .position(|entry| entry["kind"] == "unrecognized")
        .unwrap();
    for pointer in ["", "/bytes", &format!("/bytes/entries/{position}")] {
        let mut extended = value.clone();
        extended
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unnamed".into(), Value::from(1));
        assert!(
            !schema.is_valid(&extended),
            "{pointer}: an unnamed field passed"
        );
        assert!(
            serde_json::from_value::<status::Status>(extended).is_err(),
            "{pointer}: the reader accepted an unnamed field"
        );
    }
    // The schema names exactly the fields an entry carries.
    let every_field = serde_json::to_value(status::Entry {
        kind: "unrecognized".into(),
        name: "unrecognized".into(),
        state: Some("sealed".into()),
        exit: Some("ok".into()),
        capture: Some("complete".into()),
        count: Some(1),
        names: Some(vec!["notes.txt".into()]),
        bytes: 1,
    })
    .unwrap();
    let schema_document: Value = serde_json::from_slice(
        &std::fs::read(workspace_root().join("schemas/telemetry-status-v1.json")).unwrap(),
    )
    .unwrap();
    let named: std::collections::BTreeSet<&String> =
        schema_document["properties"]["bytes"]["properties"]["entries"]["items"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
    let carried: std::collections::BTreeSet<&String> =
        every_field.as_object().unwrap().keys().collect();
    assert_eq!(named, carried);
    let unrecognized: Vec<_> = document
        .bytes
        .entries
        .iter()
        .filter(|entry| entry.kind == "unrecognized")
        .collect();
    assert_eq!(unrecognized.len(), 1, "{:?}", document.bytes.entries);
    let entry = unrecognized[0];
    assert_eq!(entry.count, Some(5));
    assert_eq!(
        entry.names.as_deref().unwrap(),
        [
            "notes.txt",
            "v1/blobs/ab/stray.txt",
            "v1/blobs/unknown.bin",
            "v1/segments/unknown.bin",
            "v1/stray"
        ]
    );
    assert!(entry.bytes > 0);
    // A file beneath `blobs/` that is neither a prefix directory, an object nor a temporary is
    // charged to `unrecognized` once, never to `blobs`.
    let beneath = |path: &str| allocated(&root.join(path));
    let blobs = document
        .bytes
        .entries
        .iter()
        .find(|entry| entry.kind == "blobs")
        .unwrap();
    assert_eq!(
        blobs.bytes,
        beneath("v1/blobs") - beneath("v1/blobs/unknown.bin") - beneath("v1/blobs/ab/stray.txt")
    );
    assert!(blobs.bytes > 0, "the objects are counted");
    // Never read as a segment, never removed.
    assert_eq!(document.segments.sealed, 1);
    for (path, bytes) in &planted {
        assert_eq!(&std::fs::read(root.join(path)).unwrap(), bytes, "{path}");
    }

    // A directory with no `v1/` at all is accounted the same way.
    let bare = temp.path().join("bare");
    std::fs::create_dir_all(bare.join("stray")).unwrap();
    std::fs::write(bare.join("stray/file"), vec![2_u8; 9_000]).unwrap();
    let (document, _) = status::status(&input(bare.clone())).unwrap();
    assert_valid(&schema, &serde_json::to_value(&document).unwrap());
    assert_eq!(document.bytes.total, allocated(&bare));
    assert_eq!(document.bytes.entries.len(), 1);
    assert_eq!(document.bytes.entries[0].count, Some(1));
}
