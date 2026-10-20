//! Segments: recovery to the last complete line, interior corruption, the lock as the only
//! sign of life, the segment limit, the record bound and the persisted sequence (design §5, §6).

use std::io::Write as _;

use review_telemetry::blob::Media;
use review_telemetry::index::Index;
use review_telemetry::layout::Layout;
use review_telemetry::level::Level;
use review_telemetry::record::{
    Attrs, ConfigRow, Demand, Finding, Node, OCCURRENCE_PREFIX, ProblemWhat, RECORD_MAX_BYTES,
    Record, SealExit, SpanKind, SpanStatus,
};
use review_telemetry::segment::TERMINAL_RESERVE_BYTES;
use review_telemetry::segment::reader::{self, Recovery};

use crate::{options, sealed_lines, sealed_records, writer};

/// Three spans, one left open, and events, flushed and then abandoned as a killed process.
fn killed_writer(root: &std::path::Path) -> (String, std::path::PathBuf) {
    let writer = writer(root, Level::Spans);
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let run = writer
        .span_start(
            None,
            Some(&command),
            "af.run",
            SpanKind::Internal,
            Attrs::new(),
        )
        .unwrap();
    for index in 0..5 {
        writer.event(&run, "af.store.event", Attrs::new().int("index", index));
    }
    let node = writer
        .span_start(
            None,
            Some(&run),
            "af.node",
            SpanKind::Internal,
            Attrs::new(),
        )
        .unwrap();
    writer.span_end(&node, SpanStatus::Ok, Attrs::new(), 2);
    writer.flush();
    let path = writer.path().unwrap();
    let segment_id = writer.segment_id().unwrap();
    writer.abandon();
    (segment_id, path)
}

#[test]
fn telemetry_segment_recovery_keeps_complete_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());

    // While the writer lives, its lock says so and no reader recovers it.
    let live = writer(temp.path(), Level::Spans);
    live.span_start(None, None, "af.command", SpanKind::Internal, Attrs::new());
    live.flush();
    let live_id = live.segment_id().unwrap();
    assert_eq!(reader::recover(&layout, &live_id).unwrap(), Recovery::Live);
    assert!(layout.live_segment(&live_id).is_file());

    let (segment_id, path) = killed_writer(temp.path());
    let complete = std::fs::read(&path).unwrap();
    // The process was killed in the middle of its next record.
    let partial = br#"{"seq":13,"t":"event","schema":"af/TelemetryEvent@1","span_id":"#;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(partial)
        .unwrap();

    let recovery = reader::recover(&layout, &segment_id).unwrap();
    assert_eq!(
        recovery,
        Recovery::Recovered {
            segment_id: segment_id.clone(),
            discarded_bytes: partial.len() as u64,
            unended_spans: 2,
        }
    );
    assert!(
        !path.exists(),
        "the .jsonl is replaced by the sealed segment"
    );
    assert!(layout.refs(&segment_id).is_file());
    let records = sealed_records(&layout.sealed_segment(&segment_id));
    let kept: Vec<u8> = complete.clone();
    let complete_lines = kept.iter().filter(|byte| **byte == b'\n').count();
    // Every complete line kept, then one span_end per open span, then the seal.
    assert_eq!(records.len(), complete_lines + 3);
    let Some(Record::Seal(seal)) = records.last() else {
        panic!("no seal: {records:?}");
    };
    assert_eq!(seal.exit, SealExit::Recovered);
    assert_eq!(seal.discarded_bytes, partial.len() as u64);
    let unended: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            Record::SpanEnd(end) if end.end_unix_ns.is_none() => Some(end.status),
            _ => None,
        })
        .collect();
    assert_eq!(unended, [SpanStatus::Unset, SpanStatus::Unset]);
    // Recovering again finds nothing to do.
    assert_eq!(
        reader::recover(&layout, &segment_id).unwrap(),
        Recovery::AlreadySealed
    );
    live.finish(SealExit::Ok).unwrap();
}

#[test]
fn telemetry_segment_recovery_refuses_interior_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let (segment_id, path) = killed_writer(temp.path());
    let mut bytes = std::fs::read(&path).unwrap();
    // The same bytes with a corrupt interior line: the third line loses its opening brace.
    let starts: Vec<usize> = std::iter::once(0)
        .chain(
            bytes
                .iter()
                .enumerate()
                .filter(|(_, byte)| **byte == b'\n')
                .map(|(at, _)| at + 1),
        )
        .collect();
    bytes[starts[2]] = b'#';
    std::fs::write(&path, &bytes).unwrap();

    assert_eq!(
        reader::recover(&layout, &segment_id).unwrap(),
        Recovery::Corrupt {
            segment_id: segment_id.clone()
        }
    );
    assert!(layout.corrupt_segment(&segment_id).is_file());
    assert!(!path.exists());
    let listing = reader::list(&layout).unwrap();
    assert_eq!(listing.corrupt, [segment_id.clone()]);
    // No query reads it: the index holds nothing of it.
    let mut index = review_telemetry::index::Index::open(&layout).unwrap();
    index.ingest().unwrap();
    assert_eq!(index.count("SELECT count(*) FROM spans").unwrap(), 0);
    assert_eq!(index.count("SELECT count(*) FROM segments").unwrap(), 0);
}

#[test]
fn telemetry_writer_lock_survives_pid_reuse() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());

    // A live writer whose file name carries a PID no process has: the lock says it is alive.
    let live = writer(temp.path(), Level::Spans);
    live.span_start(None, None, "af.command", SpanKind::Internal, Attrs::new());
    live.flush();
    let path = live.path().unwrap();
    let foreign = format!(
        "{}-999999999-{}",
        live.segment_id().unwrap().split('-').next().unwrap(),
        "00c0ffee"
    );
    std::fs::rename(&path, layout.live_segment(&foreign)).unwrap();
    assert_eq!(reader::list(&layout).unwrap().live, [foreign.clone()]);
    assert_eq!(reader::recover(&layout, &foreign).unwrap(), Recovery::Live);

    // A dead writer whose PID was reused — by this very process, which is alive — is judged by
    // its lock alone: nobody holds it, so it is unsealed and recovered.
    let (dead, dead_path) = killed_writer(temp.path());
    assert!(
        dead.contains(&format!("-{}-", std::process::id())),
        "{dead}"
    );
    assert!(dead_path.is_file());
    assert!(reader::list(&layout).unwrap().unsealed.contains(&dead));
    assert!(matches!(
        reader::recover(&layout, &dead).unwrap(),
        Recovery::Recovered { .. }
    ));
    drop(live);
}

#[test]
fn telemetry_segment_limit_bounds_every_record_kind() {
    let temp = tempfile::tempdir().unwrap();
    let limit: u64 = 192 * 1024;
    let mut options = options(temp.path(), Level::Content);
    options.segment_max_bytes = limit;
    options.content_max_blob_bytes = 64 * 1024;
    let writer = review_telemetry::segment::writer::Writer::new(options).unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let mut round = 0_u64;
    // Every kind, until the limit refuses records.
    while writer.dropped() == 0 {
        round += 1;
        let span = writer.span_start(
            None,
            Some(&command),
            "af.node",
            SpanKind::Internal,
            Attrs::new().text("af.note", "x".repeat(800)),
        );
        if let Some(span) = &span {
            writer.event(
                span,
                "af.store.event",
                Attrs::new().int("round", round as i64),
            );
            writer.event_with_content(
                span,
                "gen_ai.content.input",
                Attrs::new(),
                format!("content {round}\n").repeat(40).as_bytes(),
                Media::Text,
            );
            writer.span_end(span, SpanStatus::Ok, Attrs::new(), 1);
        }
        writer.config(
            "exec",
            vec![ConfigRow {
                key: format!("telemetry.key{round}"),
                value: "v".into(),
                layer: "built-in".into(),
                origin: None,
            }],
        );
        writer.node(Node {
            seq: 0,
            schema: String::new(),
            execution_id: "exec".into(),
            plan_digest: format!("sha256:{}", "a".repeat(64)),
            node_id: format!("node-{round}"),
            kind: "model".into(),
            package: None,
            depends_on: vec!["root".into()],
            expected_nodes: 64,
        });
        writer.finding(Finding {
            seq: 0,
            schema: String::new(),
            span_id: command.clone(),
            key: format!("finding-{round}"),
            severity: "major".into(),
            effective_severity: "major".into(),
            status: "open".into(),
            scope: "file".into(),
            file: Some("src/lib.rs".into()),
            line: Some(round),
            title: "a title".into(),
            report_artifact_id: format!("sha256:{}", "b".repeat(64)),
            node_id: "review".into(),
            worker: "correctness@1.0.0".into(),
            model: None,
            round: Some(1),
            epoch: Some(1),
        });
        writer.demand(Demand {
            seq: 0,
            schema: String::new(),
            span_id: command.clone(),
            demand_id: format!("demand-{round}"),
            kind: "evidence".into(),
            required: true,
            subject_id: "subject".into(),
            snapshot_id: format!("sha256:{}", "c".repeat(64)),
            status: "open".into(),
        });
        writer.problem(ProblemWhat::Redaction, &format!("problem {}", round % 3));
        assert!(round < 10_000, "the limit never bound");
    }
    // Sustained shallow events after exhaustion: all dropped, all counted, nothing written.
    for index in 0..20_000 {
        writer.event(&command, "af.store.event", Attrs::new().int("late", index));
    }
    let dropped_before_seal = writer.dropped();
    assert!(dropped_before_seal >= 20_000, "{dropped_before_seal}");
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    let live_bytes = std::fs::metadata(writer.path().unwrap()).unwrap().len();
    assert!(
        live_bytes <= limit - TERMINAL_RESERVE_BYTES + 8192,
        "{live_bytes}"
    );
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();

    let lines = sealed_lines(&sealed.path);
    let text = crate::sealed_bytes(&sealed.path);
    assert!(text.len() as u64 <= limit, "{} > {limit}", text.len());
    let kinds: std::collections::BTreeSet<&str> = lines
        .iter()
        .map(|line| line["t"].as_str().unwrap())
        .collect();
    for kind in [
        "segment",
        "span_start",
        "span_end",
        "event",
        "config",
        "node",
        "finding",
        "demand",
        "blob",
        "blob_ref",
        "problem",
        "seal",
    ] {
        assert!(kinds.contains(kind), "{kind} missing: {kinds:?}");
    }
    let budget: Vec<_> = lines
        .iter()
        .filter(|line| line["t"] == "problem" && line["what"] == "budget")
        .collect();
    assert_eq!(budget.len(), 1, "one problem records the exhaustion");
    let seal = lines.last().unwrap();
    assert_eq!(seal["t"], "seal");
    assert_eq!(seal["exit"], "ok");
    assert!(seal["dropped"].as_u64().unwrap() >= dropped_before_seal);
}

#[test]
fn telemetry_record_bound_holds() {
    for level in [Level::Metrics, Level::Spans, Level::Content] {
        let temp = tempfile::tempdir().unwrap();
        let writer = writer(temp.path(), level);
        let mut attrs = Attrs::new();
        for index in 0..200 {
            attrs = attrs.text(&format!("af.many.{index:03}"), "y".repeat(300));
        }
        let span = writer
            .span_start(None, None, "af.command", SpanKind::Internal, attrs)
            .unwrap();
        let huge = Attrs::new()
            .text("af.failure.reason", "z".repeat(100 * 1024))
            .texts("af.list", (0..100).map(|n| format!("item {n}")).collect())
            .text(&"k".repeat(300), "long key")
            .decimal("af.usage.input_tokens", "12.5")
            .decimal(
                "af.usage.output_tokens",
                "340282366920938463463374607431768211455",
            );
        writer.event(&span, "af.big", huge);
        // Records around the bound: each attribute within its own bound, the record not.
        for count in [10, 15, 16, 17, 40] {
            let mut edge = Attrs::new();
            for index in 0..count {
                edge = edge.text(&format!("af.edge.{index}"), "e".repeat(1000));
            }
            writer.event(&span, "af.edge", edge);
        }
        writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
        let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
        let lines = sealed_lines(&sealed.path);
        for line in &lines {
            let encoded = serde_json::to_vec(line).unwrap();
            assert!(
                encoded.len() <= RECORD_MAX_BYTES,
                "{level:?}: {}",
                encoded.len()
            );
            if let Some(attributes) = line.get("attributes").and_then(|a| a.as_object()) {
                assert!(attributes.len() <= 64, "{level:?}: {}", attributes.len());
                for (key, value) in attributes {
                    assert!(key.len() <= 128);
                    if let Some(text) = value.as_str() {
                        assert!(text.len() <= 1024, "{level:?} {key}: {}", text.len());
                    }
                    if let Some(items) = value.as_array() {
                        assert!(items.len() <= 32);
                        let bytes: usize =
                            items.iter().map(|item| item.as_str().unwrap().len()).sum();
                        assert!(bytes <= 4096);
                    }
                }
            }
        }
        let start = lines.iter().find(|line| line["t"] == "span_start").unwrap();
        assert!(start["attributes"]["af.dropped_fields"].as_u64().unwrap() >= 139);
        let big = lines
            .iter()
            .find(|line| line["t"] == "event" && line["name"] == "af.big")
            .unwrap();
        let attributes = &big["attributes"];
        assert_eq!(
            attributes["af.usage.output_tokens"],
            "340282366920938463463374607431768211455"
        );
        assert!(attributes.get("af.usage.input_tokens").is_none());
        assert!(attributes["af.dropped_fields"].as_u64().unwrap() >= 2);
        let reason = attributes["af.failure.reason"].as_str().unwrap();
        if level == Level::Content {
            assert!(reason.starts_with(OCCURRENCE_PREFIX), "{reason}");
            // An array over its bound becomes a blob occurrence at `content` too.
            let list = attributes["af.list"].as_str().unwrap();
            assert!(list.starts_with(OCCURRENCE_PREFIX), "{list}");
            assert!(lines.iter().any(|line| line["t"] == "blob_ref"));
        } else {
            assert!(attributes["af.list"].as_array().unwrap().last().unwrap() == "…");
            assert!(reason.ends_with('…'), "{reason}");
            assert!(attributes["af.truncated_fields"].as_u64().unwrap() >= 2);
            assert!(!lines.iter().any(|line| line["t"] == "blob_ref"));
        }
    }
}

#[test]
fn telemetry_record_sequence_is_persisted() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let writer = writer(&source, Level::Content);
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let mut kept_spans = Vec::new();
    for index in 0..6 {
        let span = writer
            .span_start(
                None,
                Some(&command),
                "af.node",
                SpanKind::Internal,
                Attrs::new(),
            )
            .unwrap();
        writer.event_with_content(
            &span,
            "gen_ai.content.output",
            Attrs::new(),
            format!("output {index}\n").as_bytes(),
            Media::Text,
        );
        writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 1);
        if index % 2 == 0 {
            kept_spans.push(span);
        }
    }
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let lines = sealed_lines(&sealed.path);
    // Every record carries seq: 1 for the header, one more for each record after it.
    for (index, line) in lines.iter().enumerate() {
        assert_eq!(line["seq"].as_u64(), Some(index as u64 + 1), "{line}");
        assert!(line["t"].is_string() && line["schema"].is_string());
    }

    // A derived segment keeps the header, the command span, every second node with its
    // occurrences and objects, and the seal, with their seq unchanged: a sequence with gaps.
    let keep = |line: &serde_json::Value| -> bool {
        let span = line["span_id"].as_str().unwrap_or_default();
        matches!(line["t"].as_str(), Some("segment" | "seal" | "blob"))
            || span == command
            || kept_spans.iter().any(|kept| kept == span)
    };
    let derived: Vec<&serde_json::Value> = lines.iter().filter(|line| keep(line)).collect();
    assert!(derived.len() < lines.len());
    let derived_root = temp.path().join("derived");
    let layout = Layout::new(&derived_root);
    std::fs::create_dir_all(layout.segments()).unwrap();
    let mut bytes = Vec::new();
    for line in &derived {
        bytes.extend(serde_json::to_vec(line).unwrap());
        bytes.push(b'\n');
    }
    std::fs::write(
        layout.sealed_segment(&sealed.segment_id),
        review_telemetry::blob::compress(&bytes),
    )
    .unwrap();
    std::fs::copy(&sealed.refs, layout.refs(&sealed.segment_id)).unwrap();

    let ids = |root: &std::path::Path| {
        review_telemetry::index::replace(&Layout::new(root)).unwrap();
        let index = review_telemetry::index::Index::open(&Layout::new(root)).unwrap();
        let dump = index.dump().unwrap();
        (
            dump["spans"].clone(),
            dump["blob_refs"].clone(),
            dump["events"].clone(),
        )
    };
    let (source_spans, source_refs, source_events) = ids(&source);
    let (derived_spans, derived_refs, derived_events) = ids(&derived_root);
    assert_eq!(derived_spans.len(), 1 + kept_spans.len());
    // The same span rows apart from the self time, which depends on the children present.
    let without_self = |row: &Vec<String>| row[..row.len() - 1].to_vec();
    let source_rows: Vec<Vec<String>> = source_spans.iter().map(without_self).collect();
    for span in &derived_spans {
        assert!(source_rows.contains(&without_self(span)), "{span:?}");
    }
    assert_eq!(derived_refs.len(), kept_spans.len());
    for occurrence in &derived_refs {
        assert!(source_refs.contains(occurrence), "{occurrence:?}");
    }
    for event in &derived_events {
        assert!(source_events.contains(event), "{event:?}");
    }
}

/// `status` over `root`, without a registry.
fn status_of(root: &std::path::Path) -> review_telemetry::status::Status {
    review_telemetry::status::status(&review_telemetry::status::StatusInput {
        root: root.to_path_buf(),
        level: Level::Spans,
        max_bytes: 4 << 30,
        registry: None,
        accounted: Vec::new(),
        accounted_unknown: None,
    })
    .unwrap()
    .0
}

/// The status entry of segment `segment_id`.
fn entry_of(
    status: &review_telemetry::status::Status,
    segment_id: &str,
) -> review_telemetry::status::Entry {
    status
        .bytes
        .entries
        .iter()
        .find(|entry| entry.kind == "segment" && entry.name == segment_id)
        .unwrap_or_else(|| panic!("no entry for {segment_id}: {:?}", status.bytes.entries))
        .clone()
}

/// What every query of the index can see after an ingest.
fn dump(layout: &Layout) -> std::collections::BTreeMap<String, Vec<Vec<String>>> {
    let mut index = Index::open(layout).unwrap();
    index.ingest().unwrap();
    index.dump().unwrap()
}

#[test]
fn telemetry_live_ingest_refuses_interior_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    // A clean sealed segment beside the one that turns corrupt.
    let clean = writer(temp.path(), Level::Spans);
    let span = clean
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    clean.event(&span, "af.note", Attrs::new().int("n", 1));
    clean.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    clean.finish(SealExit::Ok).unwrap();
    let (segment_id, path) = killed_writer(temp.path());
    let spans_of = |index: &Index| {
        index
            .count(&format!(
                "SELECT count(*) FROM spans WHERE segment_id = '{segment_id}'"
            ))
            .unwrap()
    };

    // The valid prefix is ingested.
    let mut index = Index::open(&layout).unwrap();
    let (_, refused) = index.ingest().unwrap();
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(spans_of(&index), 3);
    drop(index);

    // Then an invalid line and a valid one are appended: interior corruption.
    let bytes = std::fs::read(&path).unwrap();
    let valid = bytes
        .split(|byte| *byte == b'\n')
        .find(|line| line.windows(11).any(|window| window == b"\"t\":\"event\""))
        .unwrap()
        .to_vec();
    let mut appended = b"{\"seq\":99,\"t\":\"event\",\"broken\n".to_vec();
    appended.extend(&valid);
    appended.push(b'\n');
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&appended)
        .unwrap();
    let mut index = Index::open(&layout).unwrap();
    let (_, refused) = index.ingest().unwrap();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].0, path.display().to_string());
    assert!(refused[0].1.contains("interior corruption"), "{refused:?}");
    assert!(refused[0].1.contains("unreadable"), "{refused:?}");
    // Its previously indexed rows are forgotten; the clean segment's stay.
    assert_eq!(spans_of(&index), 0);
    assert_eq!(index.count("SELECT count(*) FROM segments").unwrap(), 1);
    drop(index);
    let before_dump = dump(&layout);
    let before = status_of(temp.path());
    assert!(
        before
            .unreadable
            .iter()
            .any(|unreadable| unreadable.path == path.display().to_string()
                && unreadable.reason.contains("interior corruption")),
        "{:?}",
        before.unreadable
    );

    // The reindex renames it `.corrupt`; every query answers as before.
    let reindexed = review_telemetry::status::reindex(temp.path()).unwrap();
    assert_eq!(reindexed.corrupt, [segment_id.clone()]);
    assert!(layout.corrupt_segment(&segment_id).is_file());
    assert_eq!(dump(&layout), before_dump);
    let after = status_of(temp.path());
    assert_eq!(after.runs, before.runs);
    assert_eq!(after.problems, before.problems);
    assert_eq!(after.missing_blobs, before.missing_blobs);
    assert_eq!(after.withheld_attempts, before.withheld_attempts);
    assert_eq!(after.segments.corrupt, 1);
}

#[test]
fn telemetry_recovery_supersedes_a_stranded_seal() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let writer = writer(temp.path(), Level::Content);
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event(&span, "af.note", Attrs::new().int("n", 1));
    writer.event_with_content(
        &span,
        "gen_ai.content.output",
        Attrs::new(),
        b"an output\n",
        Media::Text,
    );
    writer.count_dropped_chunks(3);
    writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let original = sealed_records(&sealed.path);
    let Some(Record::Seal(writer_seal)) = original.last().cloned() else {
        panic!("no seal");
    };
    assert_eq!(writer_seal.exit, SealExit::Ok);
    let manifest = std::fs::read(&sealed.refs).unwrap();

    // The writer died after flushing its seal and before compressing: the `.jsonl` ends with
    // its seal, and there is no `.jsonl.gz` and no manifest.
    let id = sealed.segment_id.clone();
    std::fs::write(layout.live_segment(&id), crate::sealed_bytes(&sealed.path)).unwrap();
    std::fs::remove_file(&sealed.path).unwrap();
    std::fs::remove_file(&sealed.refs).unwrap();
    let unsealed = status_of(temp.path());
    assert_eq!(unsealed.segments.unsealed, 1);
    let entry = entry_of(&unsealed, &id);
    assert_eq!(entry.exit, None, "an unsealed segment shows no exit");

    assert_eq!(
        reader::recover(&layout, &id).unwrap(),
        Recovery::Recovered {
            segment_id: id.clone(),
            discarded_bytes: 0,
            unended_spans: 0,
        }
    );
    let records = sealed_records(&layout.sealed_segment(&id));
    let seals: Vec<_> = records
        .iter()
        .filter(|record| matches!(record, Record::Seal(_)))
        .collect();
    assert_eq!(seals.len(), 1, "exactly one seal: {records:?}");
    assert_eq!(records.len(), original.len());
    let Some(Record::Seal(seal)) = records.last() else {
        panic!("the seal is not last");
    };
    assert_eq!(seal.exit, SealExit::Recovered);
    assert_eq!(seal.discarded_bytes, 0);
    assert_eq!(
        (
            seal.spans,
            seal.events,
            seal.blobs,
            seal.dropped,
            seal.dropped_chunks
        ),
        (
            writer_seal.spans,
            writer_seal.events,
            writer_seal.blobs,
            writer_seal.dropped,
            writer_seal.dropped_chunks
        ),
        "the writer's counts are known and kept"
    );
    assert_eq!(seal.dropped_chunks, Some(3));
    assert_eq!(seal.ended_unix_ns, writer_seal.ended_unix_ns);
    assert_eq!(std::fs::read(layout.refs(&id)).unwrap(), manifest);

    // Every reader shows a recovered execution, never a clean exit.
    let status = status_of(temp.path());
    let entry = entry_of(&status, &id);
    assert_eq!(entry.state.as_deref(), Some("sealed"));
    assert_eq!(entry.exit.as_deref(), Some("recovered"));
    assert_eq!(entry.capture.as_deref(), Some("recovered"));
    let mut index = Index::open(&layout).unwrap();
    index.ingest().unwrap();
    assert_eq!(
        index.seals().unwrap(),
        [(id.clone(), "recovered".to_string(), "recovered".to_string())]
    );
    drop(index);
    review_telemetry::status::reindex(temp.path()).unwrap();
    assert_eq!(
        entry_of(&status_of(temp.path()), &id).exit.as_deref(),
        Some("recovered")
    );
}

#[test]
fn telemetry_recovered_seal_never_invents_counts() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let mut options = options(temp.path(), Level::Spans);
    options.segment_max_bytes = 128 * 1024;
    let writer = review_telemetry::segment::writer::Writer::new(options).unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    // An exhausted writer that dropped records, flushed its prefix and died before `finish`.
    let mut round = 0;
    while writer.dropped() == 0 {
        round += 1;
        writer.event(
            &command,
            "af.store.event",
            Attrs::new().text("af.note", "x".repeat(800)),
        );
        assert!(round < 10_000, "the limit never bound");
    }
    for index in 0..100 {
        writer.event(&command, "af.store.event", Attrs::new().int("late", index));
    }
    assert!(writer.dropped() > 100);
    writer.flush();
    let id = writer.segment_id().unwrap();
    writer.abandon();
    let kept = reader::read_live(&layout.live_segment(&id)).unwrap();
    assert!(
        kept.records.iter().any(|record| matches!(
            record,
            Record::Problem(problem) if problem.what == ProblemWhat::Budget
        )),
        "the kept prefix holds the exhaustion problem"
    );

    assert!(matches!(
        reader::recover(&layout, &id).unwrap(),
        Recovery::Recovered { .. }
    ));
    let path = layout.sealed_segment(&id);
    let Some(Record::Seal(seal)) = sealed_records(&path).last().cloned() else {
        panic!("no seal");
    };
    assert_eq!(seal.exit, SealExit::Recovered);
    assert_eq!(
        (seal.dropped, seal.dropped_chunks),
        (None, None),
        "unknown, never 0"
    );
    assert!(!seal.complete());
    assert_eq!(seal.capture(), "recovered, drops unknown");
    // The published shape says so: null only beside `exit = recovered`.
    let schema = crate::schemas::validator("telemetry-record-v1.json");
    let line = sealed_lines(&path).last().unwrap().clone();
    assert!(line["dropped"].is_null() && line["dropped_chunks"].is_null());
    crate::schemas::assert_valid(&schema, &line);
    let mut forged = line.clone();
    forged["exit"] = serde_json::Value::from("ok");
    assert!(!schema.is_valid(&forged));
    assert!(Record::parse_line(forged.to_string().as_bytes()).is_err());

    // Every reader treats the capture as incomplete.
    let status = status_of(temp.path());
    let entry = entry_of(&status, &id);
    assert_eq!(entry.exit.as_deref(), Some("recovered"));
    assert_eq!(entry.capture.as_deref(), Some("recovered, drops unknown"));
    let mut index = Index::open(&layout).unwrap();
    index.ingest().unwrap();
    assert_eq!(
        index
            .count("SELECT count(*) FROM segments WHERE dropped IS NULL AND exit = 'recovered'")
            .unwrap(),
        1
    );
    assert_eq!(
        index.seals().unwrap()[0].2,
        "recovered, drops unknown".to_string()
    );
}

#[test]
fn telemetry_reader_refuses_oversized_records() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let writer = writer(temp.path(), Level::Spans);
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event(&span, "af.big", Attrs::new().int("n", 1));
    writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();

    // The same sealed segment copied in with an otherwise valid event over 16 KiB.
    let mut lines = sealed_lines(&sealed.path);
    let at = lines.iter().position(|line| line["t"] == "event").unwrap();
    lines[at]["attributes"]["af.note"] = serde_json::Value::from("x".repeat(RECORD_MAX_BYTES));
    let oversized = serde_json::to_vec(&lines[at]).unwrap();
    assert!(oversized.len() > RECORD_MAX_BYTES);
    let record: Record = serde_json::from_slice(&oversized).unwrap();
    record.validate().unwrap();
    let error = Record::parse_line(&oversized).unwrap_err();
    assert!(error.contains(&oversized.len().to_string()), "{error}");
    assert!(error.contains(&RECORD_MAX_BYTES.to_string()), "{error}");
    let mut bytes = Vec::new();
    for line in &lines {
        bytes.extend(serde_json::to_vec(line).unwrap());
        bytes.push(b'\n');
    }
    std::fs::write(&sealed.path, review_telemetry::blob::compress(&bytes)).unwrap();

    // The scanner treats it as any invalid line: the prefix before it kept, and interior
    // corruption because a valid line follows.
    let prefix = reader::scan(&bytes);
    assert_eq!(prefix.records.len(), at);
    assert!(prefix.interior_corruption);
    assert!(prefix.invalid.as_deref().unwrap().contains("record bound"));
    let cut = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .take(at + 1)
        .flatten()
        .copied()
        .collect::<Vec<u8>>();
    let tail = reader::scan(&cut);
    assert_eq!(tail.records.len(), at);
    assert!(!tail.interior_corruption, "nothing valid follows");

    // Refused at read, never indexed, listed like any invalid line.
    let refusal = reader::read_sealed(&sealed.path).unwrap_err();
    assert!(refusal.contains("record bound"), "{refusal}");
    let mut index = Index::open(&layout).unwrap();
    let (_, refused) = index.ingest().unwrap();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].1.contains("record bound"), "{refused:?}");
    for table in ["segments", "spans", "events"] {
        assert_eq!(
            index
                .count(&format!("SELECT count(*) FROM {table}"))
                .unwrap(),
            0,
            "{table}"
        );
    }
    drop(index);
    let listed = |unreadable: &[review_telemetry::status::Unreadable]| {
        unreadable.iter().any(|unreadable| {
            unreadable.path == sealed.path.display().to_string()
                && unreadable.reason.contains("record bound")
        })
    };
    assert!(listed(&status_of(temp.path()).unreadable));
    let reindexed = review_telemetry::status::reindex(temp.path()).unwrap();
    assert!(listed(&reindexed.unreadable), "{:?}", reindexed.unreadable);
    assert_eq!(reindexed.segments, 0);
}

/// Every bound is judged before reading: a `.jsonl` segment longer than any segment this
/// release writes is refused by its length, never loaded, by the reader and by the index.
#[test]
fn telemetry_reads_refuse_files_past_their_bound() {
    use review_telemetry::level::SEGMENT_MAX_BYTES_BOUND;
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    std::fs::create_dir_all(layout.segments()).unwrap();
    let path = layout.live_segment("1700000000000-1-0a1b2c3d");
    // Sparse: the length is the bound plus one, with nothing allocated.
    std::fs::File::create(&path)
        .unwrap()
        .set_len(SEGMENT_MAX_BYTES_BOUND + 1)
        .unwrap();
    let refusal = reader::read_live(&path).unwrap_err();
    assert!(refusal.contains("segment bound"), "{refusal}");
    let mut index = Index::open(&layout).unwrap();
    let (_, refused) = index.ingest().unwrap();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].1.contains("segment bound"), "{refused:?}");
}

/// Every step of sealing is relative to the `segments/` directory the writer opened: after the
/// writer opened its live segment, `v1/segments` is renamed and a link to an outside directory
/// holding a same-named live file put in its place; `finish` writes the manifest and the sealed
/// segment and removes the live file in the directory it opened, and nothing outside the root
/// is written or removed.
#[test]
fn telemetry_sealing_stays_within_the_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let outside = temp.path().join("outside");
    let layout = Layout::new(&root);
    let writer = writer(&root, Level::Spans);
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event(&span, "af.before", Attrs::new().int("af.n", 1));
    writer.flush();
    let segment_id = writer.segment_id().unwrap();
    let live_name = format!("{segment_id}.jsonl");
    assert!(layout.live_segment(&segment_id).is_file());

    let moved = layout.versioned().join("segments.moved");
    std::fs::rename(layout.segments(), &moved).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let decoy = b"outside: not this writer's segment\n";
    std::fs::write(outside.join(&live_name), decoy).unwrap();
    std::os::unix::fs::symlink(&outside, layout.segments()).unwrap();

    writer.event(&span, "af.after", Attrs::new().int("af.n", 2));
    writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    assert_eq!(sealed.segment_id, segment_id);

    // Outside: exactly the decoy, untouched.
    let mut names: Vec<String> = std::fs::read_dir(&outside)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, [live_name.clone()]);
    assert_eq!(std::fs::read(outside.join(&live_name)).unwrap(), decoy);
    // Inside: the segment sealed where the writer opened it, every record kept.
    assert!(!moved.join(&live_name).exists(), "the live file is removed");
    assert!(moved.join(format!("{segment_id}.refs")).is_file());
    let records = sealed_records(&moved.join(format!("{segment_id}.jsonl.gz")));
    let events: Vec<&str> = records
        .iter()
        .filter_map(|record| match record {
            Record::Event(event) => Some(event.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(events, ["af.before", "af.after"]);
    assert!(matches!(records.last(), Some(Record::Seal(seal)) if seal.exit == SealExit::Ok));
    let leftovers: Vec<String> = std::fs::read_dir(&moved)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// The index records each ingested sealed file's identity and compares it on every ingest: a
/// sealed segment replaced under its name with other valid content is read again, and one
/// corrupted under its name has its earlier rows forgotten and is listed unreadable, so
/// `status` answers exactly as `reindex` does.
#[test]
fn telemetry_changed_sealed_segment_is_revalidated() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let layout = Layout::new(root);
    let writer = writer(root, Level::Spans);
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event(&span, "af.first", Attrs::new());
    writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let kept = crate::writer(root, Level::Spans);
    let other = kept
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    kept.span_end(&other, SpanStatus::Ok, Attrs::new(), 0);
    kept.finish(SealExit::Ok).unwrap().unwrap();

    let indexed = status_of(root);
    assert!(indexed.unreadable.is_empty(), "{:?}", indexed.unreadable);
    let event_names = |dump: &std::collections::BTreeMap<String, Vec<Vec<String>>>| {
        dump["events"]
            .iter()
            .map(|row| row[4].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(event_names(&dump(&layout)), ["'af.first'"]);

    // Replaced under its name (a new file renamed into place) with other valid records.
    let text = String::from_utf8(crate::sealed_bytes(&sealed.path)).unwrap();
    let changed = text.replace("\"af.first\"", "\"af.other\"");
    assert_ne!(changed, text);
    let staged = sealed.path.with_extension("staged");
    std::fs::write(
        &staged,
        review_telemetry::blob::compress(changed.as_bytes()),
    )
    .unwrap();
    std::fs::rename(&staged, &sealed.path).unwrap();
    let revalidated = status_of(root);
    assert!(revalidated.unreadable.is_empty());
    let after_status = dump(&layout);
    assert_eq!(event_names(&after_status), ["'af.other'"]);
    review_telemetry::status::reindex(root).unwrap();
    assert_eq!(dump(&layout), after_status, "status answers as reindex");

    // Corrupted in place: its rows are forgotten and it is listed unreadable.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&sealed.path)
        .unwrap();
    file.write_all(b"not a gzip stream at all").unwrap();
    drop(file);
    let refused = status_of(root);
    let path = sealed.path.display().to_string();
    assert!(
        refused.unreadable.iter().any(|entry| entry.path == path),
        "{:?}",
        refused.unreadable
    );
    let after_status = dump(&layout);
    assert!(event_names(&after_status).is_empty());
    assert_eq!(after_status["segments"].len(), 1, "the other segment stays");
    let reindexed = review_telemetry::status::reindex(root).unwrap();
    assert!(reindexed.unreadable.iter().any(|entry| entry.path == path));
    assert_eq!(dump(&layout), after_status, "status answers as reindex");
    let again = status_of(root);
    assert_eq!(again.unreadable, refused.unreadable);
    assert_eq!(again.segments, refused.segments);
}

/// Design §6: the publication's last step removes the live `.jsonl`, so a `.jsonl` beside a
/// `.jsonl.gz` (and `.refs`) of the same id is the unsealed segment and those files are stale
/// staging. Every view — the listing, `status`, the index, `runs` and `show` — and the storage
/// inventory say unsealed and read nothing sealed; recovery replaces the stale files.
#[test]
fn telemetry_live_segment_beside_a_stale_seal_is_unsealed() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let writer = writer(temp.path(), Level::Spans);
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let id = writer.segment_id().unwrap();
    let execution = review_telemetry::trace::Execution {
        store_id: "d".repeat(64),
        run_id: "task:stale".into(),
        execution_id: id.clone(),
        ordinal: 1,
        store_sequence_at_start: 0,
    };
    let run = writer
        .span_start(
            Some(&execution.trace_id()),
            Some(&command),
            "af.run",
            SpanKind::Internal,
            execution.attributes().enumerated("af.run.kind", "task"),
        )
        .unwrap();
    writer.span_end(
        &run,
        SpanStatus::Ok,
        Attrs::new().text("af.outcome", "finished"),
        1,
    );
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    // The publication renamed both files into place and stopped before the unlink: the live
    // file, ending with the writer's seal, is still beside them.
    std::fs::write(layout.live_segment(&id), crate::sealed_bytes(&sealed.path)).unwrap();
    assert!(sealed.path.exists() && sealed.refs.exists());

    let listing = reader::list(&layout).unwrap();
    assert_eq!(listing.unsealed, [id.clone()]);
    assert!(
        listing.sealed.is_empty() && listing.refs.is_empty(),
        "{listing:?}"
    );
    assert_eq!(listing.stale, [id.clone()]);

    let status = status_of(temp.path());
    assert_eq!((status.segments.unsealed, status.segments.sealed), (1, 0));
    let entry = entry_of(&status, &id);
    assert_eq!(entry.state.as_deref(), Some("unsealed"));
    assert_eq!(entry.exit, None, "an unsealed segment shows no exit");

    // The index ingests the `.jsonl`'s prefix, never the stale `.jsonl.gz`.
    let segments = &dump(&layout)["segments"];
    let [row] = segments.as_slice() else {
        panic!("{segments:?}");
    };
    assert_eq!(row[0], format!("'{id}'"));
    assert_ne!(row[1], "'sealed'", "nothing sealed is read: {row:?}");

    let (index, read, unreadable) = review_telemetry::view::open(temp.path()).unwrap();
    let runs = review_telemetry::view::runs(
        &index,
        temp.path(),
        read.clone(),
        unreadable.clone(),
        None,
        None,
    )
    .unwrap();
    let [listed] = runs.runs.as_slice() else {
        panic!("{runs:?}");
    };
    assert_eq!(listed.executions.list[0].segment, "unsealed");
    assert!(listed.open, "an unsealed execution is open");
    assert!(listed.executions.sealed.is_empty());
    let shown = review_telemetry::view::show(
        &index,
        temp.path(),
        read,
        unreadable,
        (
            &listed.store_id,
            &listed.run_id,
            &listed.trace_id,
            listed.kind.clone(),
        ),
        &review_telemetry::view::ShowSelection::default(),
    )
    .unwrap();
    assert_eq!(shown.run.executions.open.len(), 1);
    assert_eq!(shown.run.executions.open[0].segment, "unsealed");
    drop(index);

    let directory = review_telemetry::budget::Directory::measure(temp.path(), false)
        .unwrap()
        .unwrap();
    assert_eq!(
        directory.segments[&id].state,
        review_telemetry::budget::State::Unsealed
    );
    let entries = directory.entries();
    let inventoried = entries
        .iter()
        .find(|entry| entry.segment_id.as_deref() == Some(id.as_str()))
        .unwrap();
    assert_eq!(inventoried.state, "unsealed");
    assert!(inventoried.in_use.is_some() && inventoried.found.is_none());
    assert!(directory.target(&id).is_none(), "never evicted as sealed");

    // Recovery takes the `.jsonl` and replaces the stale files.
    assert!(matches!(
        reader::recover(&layout, &id).unwrap(),
        Recovery::Recovered { .. }
    ));
    assert!(!layout.live_segment(&id).exists());
    let Some(Record::Seal(seal)) = sealed_records(&layout.sealed_segment(&id)).pop() else {
        panic!("no seal");
    };
    assert_eq!(seal.exit, SealExit::Recovered);

    // A live file recovery rejects never leaves the stale seal readable: the same-id
    // `.jsonl.gz` and `.refs` go before the `.jsonl` is removed (empty) or renamed `.corrupt`
    // (an invalid line before a valid one), so no listing takes them as sealed afterwards.
    let sealed_beside = |live: &dyn Fn(&[u8]) -> Vec<u8>| {
        let writer = crate::writer(temp.path(), Level::Spans);
        let span = writer
            .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
            .unwrap();
        writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
        let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
        let id = sealed.segment_id.clone();
        std::fs::write(
            layout.live_segment(&id),
            live(&crate::sealed_bytes(&sealed.path)),
        )
        .unwrap();
        assert!(sealed.path.exists() && sealed.refs.exists());
        assert_eq!(reader::list(&layout).unwrap().stale, [id.clone()]);
        id
    };
    let empty = sealed_beside(&|_| Vec::new());
    assert_eq!(reader::recover(&layout, &empty).unwrap(), Recovery::Empty);
    let corrupt = sealed_beside(&|bytes| {
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        format!("{}\nnot a record\n{}\n", lines[0], lines[1]).into_bytes()
    });
    assert_eq!(
        reader::recover(&layout, &corrupt).unwrap(),
        Recovery::Corrupt {
            segment_id: corrupt.clone()
        }
    );
    assert!(layout.corrupt_segment(&corrupt).exists());
    for rejected in [&empty, &corrupt] {
        assert!(!layout.live_segment(rejected).exists(), "{rejected}");
        assert!(
            !layout.sealed_segment(rejected).exists(),
            "{rejected}: the stale seal is gone"
        );
        assert!(
            !layout.refs(rejected).exists(),
            "{rejected}: the stale manifest is gone"
        );
    }
    let listing = reader::list(&layout).unwrap();
    assert_eq!(listing.sealed, [id.clone()], "{listing:?}");
    assert_eq!(listing.refs, [id.clone()], "{listing:?}");
    assert_eq!(listing.corrupt, [corrupt.clone()], "{listing:?}");
    assert!(listing.stale.is_empty(), "{listing:?}");
    let status = status_of(temp.path());
    assert_eq!((status.segments.sealed, status.segments.unsealed), (1, 0));
}
