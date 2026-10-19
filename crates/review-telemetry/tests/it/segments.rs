//! Segments: recovery to the last complete line, interior corruption, the lock as the only
//! sign of life, the segment limit, the record bound and the persisted sequence (design §5, §6).

use std::io::Write as _;

use review_telemetry::blob::Media;
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
        assert!(attributes["af.list"].as_array().unwrap().last().unwrap() == "…");
        let reason = attributes["af.failure.reason"].as_str().unwrap();
        if level == Level::Content {
            assert!(reason.starts_with(OCCURRENCE_PREFIX), "{reason}");
            assert!(lines.iter().any(|line| line["t"] == "blob_ref"));
        } else {
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
