//! The configuration record (`af/TelemetryConfig@2`, design §7.4): written section by section
//! as bounded parts in the rows' order, each part naming the run that owns it, and assembled by
//! the reader for that run, a section `incomplete` when a part is missing and `unknown` when
//! none was written; its
//! identity rows keep their digests; the assembled document matches its schema; and every span
//! of a run names the run, taking what it does not name from its parent.

use review_telemetry::level::Level;
use review_telemetry::record::{
    Attrs, ConfigRow, RECORD_MAX_BYTES, SealExit, SpanKind, SpanStatus,
};
use review_telemetry::trace::Execution;
use review_telemetry::view;

use crate::schemas::{assert_valid, validator};
use crate::writer;

const STORE: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const RUN: &str = "task:sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn row(layer: &str, key: &str, value: &str) -> ConfigRow {
    ConfigRow {
        key: key.into(),
        value: value.into(),
        layer: layer.into(),
        origin: None,
    }
}

/// A configuration whose rows exceed the record bound: a 64-node plan with its bindings and a
/// long configuration, in the fixed section order.
fn rows() -> Vec<ConfigRow> {
    let mut rows = vec![row("version", "version.version", "0.0.0-fixture")];
    for index in 0..80 {
        rows.push(ConfigRow {
            key: format!("table.option_{index:02}"),
            value: format!("\"value {index} of a configuration long enough to need parts\""),
            layer: "user".into(),
            origin: Some(format!("/etc/af/config.toml:{}", index + 1)),
        });
    }
    rows.push(row(
        "policy",
        "af.policy_rev",
        &format!("sha256:{}", "a".repeat(64)),
    ));
    rows.push(row(
        "policy",
        "af.af_toml.digest",
        &format!("sha256:{}", "b".repeat(64)),
    ));
    for index in 0..64 {
        rows.push(row(
            "binding",
            &format!("root.nodes.n{index:02}"),
            "worker=fixture/implementer@1 provider=claude-main model=claude-opus-5-5 effort=high",
        ));
    }
    rows.push(row("variable", "AF_TELEMETRY__LEVEL", ""));
    rows
}

/// Write one execution of the run with the configuration record whose `configuration` section
/// is `rows` (and a one-row `plan` and `machine` section unless `plan_resolved` is false),
/// sealed; the execution and the parts of the `configuration` section.
fn execution(root: &std::path::Path, rows: Vec<ConfigRow>) -> (String, u32) {
    execution_with(root, rows, true)
}

fn execution_with(
    root: &std::path::Path,
    rows: Vec<ConfigRow>,
    plan_resolved: bool,
) -> (String, u32) {
    let writer = writer(root, Level::Spans);
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let execution_id = writer.segment_id().unwrap();
    let run = writer
        .span_start(
            Some(&review_telemetry::trace::trace_id(STORE, RUN)),
            Some(&command),
            "af.run",
            SpanKind::Internal,
            Execution {
                store_id: STORE.into(),
                run_id: RUN.into(),
                execution_id: execution_id.clone(),
                ordinal: 1,
                store_sequence_at_start: 1,
            }
            .attributes(),
        )
        .unwrap();
    let owner = review_telemetry::segment::writer::ConfigOwner {
        execution_id: &execution_id,
        run_id: RUN,
        run_span_id: &run,
    };
    let parts = writer.config(&owner, "configuration", rows).unwrap();
    writer.config(&owner, "machine", vec![row("host", "host.os", "macos")]);
    if plan_resolved {
        writer.config(&owner, "plan", vec![row("policy", "af.task_id", "t")]);
    }
    writer.span_end(&run, SpanStatus::Ok, Attrs::new(), 1);
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    writer.finish(SealExit::Ok).unwrap();
    (execution_id, parts)
}

fn assembled(root: &std::path::Path, execution: Option<&str>) -> view::ConfigRecord {
    let (index, read, _) = view::open(root).unwrap();
    let trace = review_telemetry::trace::trace_id(STORE, RUN);
    view::config(&index, root, read, (STORE, RUN, &trace), execution)
        .unwrap()
        .unwrap()
}

/// Design §7.4 and §15 P3: a configuration whose rows exceed 16 KiB is written as several parts,
/// each within the record bound and carrying its execution, ordinal, count and whether it is the
/// last; the reader assembles them in the rows' order; with one part gone it says `incomplete`
/// and names the part, never presenting what is left as the whole record.
#[test]
fn telemetry_config_parts_assemble() {
    let temp = tempfile::tempdir().unwrap();
    let rows = rows();
    let total: usize = rows
        .iter()
        .map(|row| serde_json::to_vec(row).unwrap().len())
        .sum();
    assert!(total > RECORD_MAX_BYTES, "{total}");
    let (execution_id, parts) = execution(temp.path(), rows.clone());
    assert!(parts >= 2, "{parts}");

    let lines = crate::sealed_lines(
        &std::fs::read_dir(temp.path().join("v1/segments"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.to_string_lossy().ends_with(".jsonl.gz"))
            .unwrap(),
    );
    let written: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|line| line["t"] == "config" && line["section"] == "configuration")
        .collect();
    assert_eq!(written.len() as u32, parts);
    let run_span = lines
        .iter()
        .find(|line| line["t"] == "span_start" && line["name"] == "af.run")
        .unwrap()["span_id"]
        .clone();
    for (index, part) in written.iter().enumerate() {
        assert!(serde_json::to_vec(part).unwrap().len() <= RECORD_MAX_BYTES);
        assert_eq!(part["schema"], "af/TelemetryConfig@2");
        assert_eq!(part["execution_id"], execution_id.as_str());
        assert_eq!(part["run_id"], RUN);
        assert_eq!(part["run_span_id"], run_span);
        assert_eq!(part["part"], index as u64 + 1);
        assert_eq!(part["parts"], u64::from(parts));
        assert_eq!(part["complete"], index as u32 + 1 == parts);
    }

    let record = assembled(temp.path(), None);
    assert_eq!(record.state, "complete");
    assert_eq!(record.sections[0].section, "configuration");
    assert_eq!(record.sections[0].parts, Some(parts));
    assert_eq!(record.execution_id, execution_id);
    assert_eq!(record.run_span_id, run_span.as_str().unwrap());
    assert_eq!(record.ordinal, Some(1));
    let mut expected = rows.clone();
    expected.push(row("policy", "af.task_id", "t"));
    expected.push(row("host", "host.os", "macos"));
    assert_eq!(record.rows, expected, "every row, in the sections' order");
    // An identity row keeps its digest; nothing else of a row is exempt.
    let digest = |key: &str| {
        record
            .rows
            .iter()
            .find(|row| row.key == key)
            .unwrap()
            .value
            .clone()
    };
    assert_eq!(
        digest("af.policy_rev"),
        format!("sha256:{}", "a".repeat(64))
    );
    assert_eq!(
        digest("af.af_toml.digest"),
        format!("sha256:{}", "b".repeat(64))
    );
    assert_valid(
        &validator("telemetry-config-v2.json"),
        &serde_json::to_value(&record).unwrap(),
    );

    // Part 2 lost: the record is incomplete, the missing part named.
    let (index, _, _) = view::open(temp.path()).unwrap();
    index
        .connection()
        .execute(
            "DELETE FROM configs WHERE part = 2 AND section = 'configuration'",
            [],
        )
        .unwrap();
    drop(index);
    let partial = assembled(temp.path(), Some(&execution_id));
    assert_eq!(partial.state, "incomplete");
    assert_eq!(partial.sections[0].state, "incomplete");
    assert_eq!(partial.sections[0].missing_parts, vec![2]);
    assert!(partial.rows.len() < expected.len());
    assert_valid(
        &validator("telemetry-config-v2.json"),
        &serde_json::to_value(&partial).unwrap(),
    );
    // An execution the run does not have is named, never answered with another's record.
    let (index, read, _) = view::open(temp.path()).unwrap();
    let trace = review_telemetry::trace::trace_id(STORE, RUN);
    let unknown = view::config(
        &index,
        temp.path(),
        read,
        (STORE, RUN, &trace),
        Some("nope"),
    )
    .unwrap()
    .unwrap_err();
    assert!(unknown.contains("no execution nope"), "{unknown}");
}

/// Design §7.4: a run refused before its plan resolved still has the sections its `af.run`
/// opened with; the plan section is `unknown` and the record `incomplete`, never presented as
/// whole.
#[test]
fn an_unwritten_section_is_unknown() {
    let temp = tempfile::tempdir().unwrap();
    execution_with(
        temp.path(),
        vec![row("version", "version.version", "1")],
        false,
    );
    let record = assembled(temp.path(), None);
    assert_eq!(record.state, "incomplete");
    let states: Vec<(&str, &str)> = record
        .sections
        .iter()
        .map(|section| (section.section.as_str(), section.state.as_str()))
        .collect();
    assert_eq!(
        states,
        [
            ("configuration", "complete"),
            ("plan", "unknown"),
            ("machine", "complete")
        ]
    );
    assert_valid(
        &validator("telemetry-config-v2.json"),
        &serde_json::to_value(&record).unwrap(),
    );
}

/// ADR-0145: the assembled document refuses a member its schema does not name, and an
/// execution without a record is `absent`.
#[test]
fn the_config_document_matches_its_schema() {
    let temp = tempfile::tempdir().unwrap();
    execution(temp.path(), vec![row("version", "version.version", "1")]);
    let record = assembled(temp.path(), None);
    let validator = validator("telemetry-config-v2.json");
    let mut value = serde_json::to_value(&record).unwrap();
    assert_valid(&validator, &value);
    value["environment"] = serde_json::json!({"PATH": "/usr/bin"});
    assert!(!validator.is_valid(&value));
    let mut row = serde_json::to_value(&record).unwrap();
    row["rows"][0]["secret"] = "x".into();
    assert!(!validator.is_valid(&row));
}

/// Design §5: a span that does not name the run's identity takes it from its parent, written
/// first, so a phase recorded by a crate below the runtime names the Store, run, kind and
/// execution it worked for; a span that names its own keeps its own.
#[test]
fn a_span_takes_the_run_identity_it_does_not_name() {
    use review_telemetry::subscriber::{TelemetrySubscriber, Typed};
    let temp = tempfile::tempdir().unwrap();
    let writer = writer(temp.path(), Level::Spans);
    let dispatch = tracing::Dispatch::new(TelemetrySubscriber::new(writer.clone()));
    tracing::dispatcher::with_default(&dispatch, || {
        assert!(!review_telemetry::subscriber::in_run());
        let command = tracing::info_span!("af.command");
        let _command = command.enter();
        assert!(!review_telemetry::subscriber::in_run());
        let run = tracing::info_span!(
            "af.run",
            af.store_id = Typed::identity(STORE).unwrap().field(),
            af.run_id = Typed::identity(RUN).unwrap().field(),
            af.run.kind = Typed::enumerated("task").field(),
            af.execution_id = "execution-1",
        );
        let _run = run.enter();
        assert!(review_telemetry::subscriber::in_run());
        let phase = tracing::info_span!("af.git", af.git.op = "cat-file");
        let _phase = phase.enter();
        let measure = tracing::info_span!(
            "af.run",
            af.run_id = Typed::identity(format!("sha256:{}", "c".repeat(64)))
                .unwrap()
                .field(),
            af.run.kind = Typed::enumerated("measure").field(),
        );
        let _measure = measure.enter();
        let repetition = tracing::info_span!("af.measurement.repetition");
        drop(repetition);
    });
    writer.flush();
    let text = std::fs::read_to_string(writer.path().unwrap()).unwrap();
    writer.finish(SealExit::Ok).unwrap();
    let starts: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|line| line["t"] == "span_start")
        .collect();
    let git = &starts[2];
    assert_eq!(git["name"], "af.git");
    let keys: Vec<&String> = git["attributes"].as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "af.store_id",
            "af.run_id",
            "af.run.kind",
            "af.execution_id",
            "af.git.op"
        ]
    );
    assert_eq!(git["attributes"]["af.run_id"], RUN);
    assert_eq!(
        git["trace_id"],
        review_telemetry::trace::trace_id(STORE, RUN).as_str()
    );
    // A measurement is a run of its own in the same Store and execution.
    let measure = &starts[3];
    let measured = format!("sha256:{}", "c".repeat(64));
    assert_eq!(measure["attributes"]["af.run_id"], measured.as_str());
    assert_eq!(measure["attributes"]["af.run.kind"], "measure");
    assert_eq!(measure["attributes"]["af.store_id"], STORE);
    assert_eq!(measure["attributes"]["af.execution_id"], "execution-1");
    assert_eq!(
        measure["trace_id"],
        review_telemetry::trace::trace_id(STORE, &measured).as_str()
    );
    let repetition = &starts[4];
    assert_eq!(repetition["attributes"]["af.run_id"], measured.as_str());
    assert_eq!(repetition["trace_id"], measure["trace_id"]);
}
