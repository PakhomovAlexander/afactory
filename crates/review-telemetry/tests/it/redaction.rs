//! Structured redaction (design §8, ADR-0151 clause 6): credentials withheld on every path,
//! before any truncation; identities and counters intact; every JSON document still parses.

use review_telemetry::blob::{self, Media};
use review_telemetry::layout::Layout;
use review_telemetry::level::Level;
use review_telemetry::record::{Attrs, SealExit, SpanKind, SpanStatus};
use review_telemetry::redact::{
    OVERSIZED_LINE, Redactor, WITHHELD, WITHHELD_LINE, redact_json, redact_text,
};
use serde_json::{Value, json};

use crate::{options, sealed_lines};

const DIGEST: &str = "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJlc2lnbmF0dXJl";
const HEX_RUN: &str = "3f9a1c0b7e6d5a4f3e2d1c0b9a8f7e6d5c4b3a2f";

/// Every credential kind of design §8, as text lines.
fn credential_lines() -> Vec<String> {
    vec![
        "OPENAI_API_KEY=sk-proj-FIXTURE".into(),
        "db password = pw".into(),
        "Authorization: Bearer FIXTUREFIXTURE".into(),
        "token ghp_FIXTURE from the environment".into(),
        format!("session {JWT}"),
        format!("fetch failed for {HEX_RUN}"),
        "key aGVsbG8gd29ybGQgdGhpcyBpcyBhIGZpeHR1cmU=".into(),
        "visit https://auth.openai.com/codex/device and enter user_code ABCD".into(),
    ]
}

fn assert_no_fixture(text: &str) {
    for marker in [
        "FIXTURE",
        "sk-proj",
        "ghp_",
        JWT,
        HEX_RUN,
        "aGVsbG8",
        "user_code",
        "= pw",
    ] {
        assert!(!text.contains(marker), "`{marker}` survived in {text}");
    }
}

#[test]
fn telemetry_redaction_keeps_identities() {
    let temp = tempfile::tempdir().unwrap();
    let writer =
        review_telemetry::segment::writer::Writer::new(options(temp.path(), Level::Content))
            .unwrap();
    let trace = review_telemetry::trace::trace_id(&"a".repeat(64), "run-1");
    let run = writer
        .span_start(
            Some(&trace),
            None,
            "af.run",
            SpanKind::Internal,
            Attrs::new()
                .id("af.store_id", "a".repeat(64))
                .id("af.run_id", "run-1")
                .id("af.context.manifest", DIGEST)
                .id("af.attempt_id", "attempt-7")
                .decimal("af.usage.input_tokens", "18446744073709551616")
                .enumerated("af.outcome", "failed")
                .text("af.failure.reason", credential_lines().join("\n")),
        )
        .unwrap();
    // The same digest in untrusted text earns no exemption.
    writer.event(
        &run,
        "af.note",
        Attrs::new().text("af.text", format!("digest {DIGEST}")),
    );
    let text: String = credential_lines()
        .into_iter()
        .chain([format!("blob {DIGEST}"), "an ordinary line".into()])
        .map(|line| format!("{line}\n"))
        .collect();
    writer.event_with_content(
        &run,
        "gen_ai.content.output",
        Attrs::new(),
        text.as_bytes(),
        Media::Text,
    );
    writer.span_end(&run, SpanStatus::Error, Attrs::new(), 1);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let lines = sealed_lines(&sealed.path);
    let start = lines.iter().find(|line| line["t"] == "span_start").unwrap();
    let attributes = &start["attributes"];
    assert_eq!(start["trace_id"], trace.as_str());
    assert_eq!(start["span_id"].as_str().unwrap().len(), 16);
    assert_eq!(attributes["af.store_id"], "a".repeat(64).as_str());
    assert_eq!(attributes["af.run_id"], "run-1");
    assert_eq!(attributes["af.context.manifest"], DIGEST);
    assert_eq!(attributes["af.attempt_id"], "attempt-7");
    assert_eq!(attributes["af.usage.input_tokens"], "18446744073709551616");
    assert_eq!(attributes["af.outcome"], "failed");
    assert_eq!(attributes["af.failure.reason"], WITHHELD);
    assert_eq!(attributes["af.redacted"], 1);
    let note = lines.iter().find(|line| line["name"] == "af.note").unwrap();
    assert_eq!(note["attributes"]["af.text"], WITHHELD);
    for line in &lines {
        assert_no_fixture(&line.to_string());
        if let Some(time) = line.get("start_unix_ns").or(line.get("time_unix_ns")) {
            assert!(time.as_str().unwrap().bytes().all(|b| b.is_ascii_digit()));
        }
    }
    let blob = lines.iter().find(|line| line["t"] == "blob").unwrap();
    let digest = blob["digest"].as_str().unwrap();
    assert_eq!(blob["redaction"], "af-redact/1");
    let review_telemetry::blob::reader::Content::Bytes(bytes) =
        review_telemetry::blob::reader::read(&Layout::new(temp.path()), digest)
    else {
        panic!("blob unreadable");
    };
    let content = String::from_utf8(bytes).unwrap();
    assert_no_fixture(&content);
    assert_eq!(
        content.matches(WITHHELD_LINE).count(),
        credential_lines().len() + 1
    );
    assert!(content.contains("an ordinary line\n"), "{content}");
}

#[test]
fn telemetry_redaction_withholds_secret_members() {
    let mut document = json!({
        "password": "violet",
        "PASSWD": "pw",
        "config": {"db": {"password": "x", "host": "localhost"}, "api_key": 12345678},
        "pin_secret": 4321,
        "auth": {"user": "u", "pass": "p"},
        "items": [{"access_token": "short"}, {"name": "kept"}],
        "credentials": null,
        "id": DIGEST,
        "count": 3
    });
    let withheld = redact_json(&mut document);
    let text = serde_json::to_string(&document).unwrap();
    let reparsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(reparsed, document);
    assert_eq!(document["password"], WITHHELD);
    assert_eq!(document["PASSWD"], WITHHELD);
    assert_eq!(document["config"]["db"]["password"], WITHHELD);
    assert_eq!(document["config"]["db"]["host"], "localhost");
    assert_eq!(document["config"]["api_key"], WITHHELD);
    assert_eq!(document["pin_secret"], WITHHELD);
    assert_eq!(document["auth"], WITHHELD);
    assert_eq!(document["items"][0]["access_token"], WITHHELD);
    assert_eq!(document["items"][1]["name"], "kept");
    assert_eq!(document["credentials"], Value::Null);
    // A harness document has no exempt field: a digest-shaped value is a secret run there.
    assert_eq!(document["id"], WITHHELD);
    assert_eq!(document["count"], 3);
    assert_eq!(withheld, 8);
    for secret in ["violet", "\"pw\"", "12345678", "4321", "short"] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    // A numeric credential with no telling name is withheld by its shape: a long digit run.
    let mut numeric = json!({"value": "340282366920938463463374607431768211455", "small": 42});
    redact_json(&mut numeric);
    assert_eq!(numeric, json!({"value": WITHHELD, "small": 42}));

    // Kernel JSON keeps the identities af's schema types, by position, and nothing else.
    let mut kernel = json!({
        "artifact_id": DIGEST,
        "refs": [DIGEST],
        "payload": {"secret": DIGEST, "note": DIGEST}
    });
    Redactor::new().kernel_json(&mut kernel, &["/artifact_id", "/refs/*"]);
    assert_eq!(kernel["artifact_id"], DIGEST);
    assert_eq!(kernel["refs"][0], DIGEST);
    assert_eq!(kernel["payload"]["secret"], WITHHELD);
    assert_eq!(kernel["payload"]["note"], WITHHELD);
}

#[test]
fn telemetry_redaction_withholds_credential_member_names() {
    let names = [
        "sk-proj-abcdef".to_string(),
        "ghp_abcdef".to_string(),
        JWT.to_string(),
        HEX_RUN.to_string(),
        "Ab3dEf6hIj9lMn2pQr5tUv8xYz1BcD4fGh7jKl0nOp".to_string(),
    ];
    for name in &names {
        // At the top: the whole document becomes the string.
        let mut top = json!({ name.clone(): "ok", "other": "kept" });
        redact_json(&mut top);
        assert_eq!(top, json!(WITHHELD), "{name}");
        // One level down: only the containing object goes.
        let mut nested = json!({"outer": { name.clone(): 1, "sibling": "gone" }, "kept": "yes"});
        redact_json(&mut nested);
        assert_eq!(nested, json!({"outer": WITHHELD, "kept": "yes"}), "{name}");
        // Deeper, inside arrays.
        let mut deep = json!({"a": [{"b": {"c": { name.clone(): true }}, "d": "kept"}]});
        redact_json(&mut deep);
        assert_eq!(
            deep,
            json!({"a": [{"b": {"c": WITHHELD}, "d": "kept"}]}),
            "{name}"
        );
        for document in [&top, &nested, &deep] {
            let text = serde_json::to_string(document).unwrap();
            assert!(!text.contains(name.as_str()), "{text}");
            serde_json::from_str::<Value>(&text).unwrap();
        }
    }
    // Through a blob: the document is redacted whole and still parses.
    let source =
        serde_json::to_vec(&json!({"tools": [{"sk-live-1234": "x"}], "ok": DIGEST})).unwrap();
    let prepared = blob::prepare(&source, Media::Json, 64 * 1024, &Redactor::new());
    let retained: Value = serde_json::from_slice(&prepared.retained).unwrap();
    assert_eq!(retained, json!({"tools": [WITHHELD], "ok": WITHHELD}));
}

#[test]
fn telemetry_redaction_precedes_truncation() {
    let max = 2048;
    // A credential line placed across each truncation boundary: the head's end, the tail's start
    // and the middle that is cut away.
    for at in [0, 31, 32, 33, 34, 64, 93, 94, 95, 96, 127] {
        let mut lines: Vec<String> = (0..128)
            .map(|n| format!("ordinary line {n:03} padding"))
            .collect();
        lines[at] = format!("refresh_token: FIXTURE{}", "Q".repeat(40));
        let text = lines.join("\n");
        let prepared = blob::prepare(text.as_bytes(), Media::Text, max, &Redactor::new());
        let retained = String::from_utf8(prepared.retained.clone()).unwrap();
        assert!(prepared.truncated, "{at}");
        assert!(prepared.retained.len() <= max as usize, "{at}");
        assert!(
            !retained.contains("FIXTURE") && !retained.contains("QQQQ"),
            "{at}: {retained}"
        );
    }
    // An oversized line cannot be judged whole and is withheld whole, never cut.
    let oversized = format!("short line\nAPI_KEY={}\nlast line\n", "x".repeat(4096));
    let prepared = blob::prepare(oversized.as_bytes(), Media::Text, max, &Redactor::new());
    let retained = String::from_utf8(prepared.retained.clone()).unwrap();
    assert_eq!(
        retained,
        format!("short line\n{OVERSIZED_LINE}\nlast line\n")
    );
    assert!(prepared.withheld_whole);
    // An oversized JSON document is withheld whole too, and stays JSON.
    let document = serde_json::to_vec(&json!({"a": "x".repeat(4096)})).unwrap();
    let prepared = blob::prepare(&document, Media::Json, max, &Redactor::new());
    assert_eq!(
        serde_json::from_slice::<Value>(&prepared.retained).unwrap(),
        json!(WITHHELD)
    );
    // An attribute value is judged whole before the 1 KiB bound cuts it.
    let value = format!("{}token=FIXTURE{}", "p".repeat(1018), "Z".repeat(64));
    let (attributes, counters) = review_telemetry::record::bound_attributes(
        Attrs::new().text("af.failure.reason", value),
        &Redactor::new(),
        &mut |_, _| None,
    );
    assert_eq!(attributes["af.failure.reason"], WITHHELD);
    assert_eq!(counters.redacted, 1);
    // Text redaction keeps the review-runner rule for every line.
    let (text, withheld) = redact_text(&credential_lines().join("\n"));
    assert_eq!(withheld as usize, credential_lines().len());
    assert_no_fixture(&text);
}
