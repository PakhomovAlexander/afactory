//! Structured redaction (design §8, ADR-0153 clause 6): credentials withheld on every path,
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

/// Every file below `root`, decompressed when it is gzip.
fn every_file_text(root: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
    use std::io::Read as _;
    let mut all = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let mut text = Vec::new();
            if flate2::read::GzDecoder::new(bytes.as_slice())
                .read_to_end(&mut text)
                .is_err()
            {
                text = bytes;
            }
            all.push((path, String::from_utf8_lossy(&text).to_string()));
        }
    }
    all
}

#[test]
fn telemetry_redaction_covers_event_names_and_header_fields() {
    use review_telemetry::record::ConfigRow;
    use review_telemetry::subscriber::TelemetrySubscriber;
    const PLANTED: &str = "API_KEY=abc123";
    for level in [Level::Metrics, Level::Spans, Level::Content] {
        let temp = tempfile::tempdir().unwrap();
        let mut options = options(temp.path(), level);
        options.cwd = std::path::PathBuf::from(format!("/work/{PLANTED}/repo"));
        let writer = review_telemetry::segment::writer::Writer::new(options).unwrap();
        let dispatch = tracing::Dispatch::new(TelemetrySubscriber::new(writer.clone()));
        tracing::dispatcher::with_default(&dispatch, || {
            let command = tracing::info_span!("af.command");
            let _entered = command.enter();
            // A `tracing` event named by `af.event`, and one named by its message.
            tracing::info!(af.event = PLANTED, af.attempt = 1_u64);
            tracing::info!("{PLANTED}");
        });
        // The writer's own entry points: a span and an event named with a credential, and a
        // configuration row whose origin carries one.
        let span = writer
            .span_start(None, None, PLANTED, SpanKind::Internal, Attrs::new())
            .unwrap();
        writer.event(&span, PLANTED, Attrs::new());
        writer.config(
            "exec",
            vec![ConfigRow {
                key: "telemetry.level".into(),
                value: "spans".into(),
                layer: "user".into(),
                origin: Some(format!("/etc/af/af.toml {PLANTED}")),
            }],
        );
        writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 1);
        let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();

        // Nothing of the credential persists anywhere under the directory.
        for (path, text) in every_file_text(temp.path()) {
            assert!(
                !text.contains("abc123"),
                "{level:?}: the credential survived in {}: {text}",
                path.display()
            );
        }
        // Each field carries the withheld marker instead.
        let lines = sealed_lines(&sealed.path);
        let header = lines.iter().find(|line| line["t"] == "segment").unwrap();
        assert_eq!(header["cwd"], WITHHELD, "{level:?}");
        let events: Vec<&Value> = lines.iter().filter(|line| line["t"] == "event").collect();
        assert_eq!(events.len(), 3, "{level:?}");
        for event in &events {
            assert_eq!(event["name"], WITHHELD, "{level:?}: {event}");
            assert_eq!(event["attributes"]["af.redacted"], 1, "{level:?}: {event}");
        }
        assert_eq!(events[0]["attributes"]["af.attempt"], 1, "{level:?}");
        let started: Vec<&Value> = lines
            .iter()
            .filter(|line| line["t"] == "span_start")
            .collect();
        assert_eq!(started[0]["name"], "af.command", "{level:?}");
        assert_eq!(started[1]["name"], WITHHELD, "{level:?}");
        let config = lines.iter().find(|line| line["t"] == "config").unwrap();
        assert_eq!(config["rows"][0]["origin"], WITHHELD, "{level:?}");
        assert_eq!(config["rows"][0]["key"], "telemetry.level", "{level:?}");
        assert_eq!(config["rows"][0]["value"], "spans", "{level:?}");
    }
}

#[test]
fn telemetry_config_rows_withhold_secret_named_keys() {
    let row = |key: &str, value: &str| review_telemetry::record::ConfigRow {
        key: key.into(),
        value: value.into(),
        layer: "user".into(),
        origin: None,
    };
    for level in [Level::Metrics, Level::Spans, Level::Content] {
        let temp = tempfile::tempdir().unwrap();
        let writer =
            review_telemetry::segment::writer::Writer::new(options(temp.path(), level)).unwrap();
        let span = writer
            .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
            .unwrap();
        // Secret-named keys: the value is withheld whatever its length or shape — a short
        // word, a number, an empty string, a table, a value longer than the string bound.
        let parts = writer.config(
            "exec",
            vec![
                row("provider.password", "violet"),
                row("provider.api_key", "9081726354"),
                row("sink.auth_token", ""),
                row("provider.credentials", "{ file = \"~/.secret\" }"),
                row("provider.SECRET", &"s".repeat(4096)),
                // Ordinary keys keep their values under the ordinary rules.
                row("telemetry.level", "\"spans\""),
                row("telemetry.flush_bytes", "65536"),
                row("telemetry.notes", "sk-proj-FIXTURE"),
                row("telemetry.long", &"v".repeat(4096)),
            ],
        );
        assert!(parts.is_some(), "{level:?}: the config record is written");
        writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
        let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
        let lines = sealed_lines(&sealed.path);
        let configs: Vec<&Value> = lines.iter().filter(|line| line["t"] == "config").collect();
        assert!(!configs.is_empty(), "{level:?}");
        let rows: Vec<&Value> = configs
            .iter()
            .flat_map(|config| config["rows"].as_array().unwrap())
            .collect();
        let value = |key: &str| {
            rows.iter()
                .find(|row| row["key"] == key)
                .unwrap_or_else(|| panic!("{level:?}: no row {key}"))["value"]
                .as_str()
                .unwrap()
                .to_string()
        };
        for key in [
            "provider.password",
            "provider.api_key",
            "sink.auth_token",
            "provider.credentials",
            "provider.SECRET",
        ] {
            assert_eq!(value(key), WITHHELD, "{level:?}: {key}");
        }
        assert_eq!(value("telemetry.level"), "\"spans\"", "{level:?}");
        assert_eq!(value("telemetry.flush_bytes"), "65536", "{level:?}");
        assert_eq!(value("telemetry.notes"), WITHHELD, "{level:?}");
        let long = value("telemetry.long");
        assert!(long.starts_with("vvv") && long.ends_with('…'), "{level:?}");
        // Five secret-named rows and one credential-shaped value, counted on the parts.
        let redacted: u64 = configs
            .iter()
            .map(|config| config["af.redacted"].as_u64().unwrap_or(0))
            .sum();
        assert_eq!(redacted, 6, "{level:?}");
        for (path, text) in every_file_text(temp.path()) {
            for planted in ["violet", "9081726354", "~/.secret", "ssss", "FIXTURE"] {
                assert!(
                    !text.contains(planted),
                    "{level:?}: {planted} survived in {}",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn telemetry_credential_shaped_attribute_names_are_withheld() {
    // Every name the value rules judge a credential: a token prefix, a JWT, a long secret run,
    // and assignment-shaped names (`API_KEY=hunter2`, `password: hunter2`).
    let credential_names = [
        "sk-proj-Abc123",
        JWT,
        HEX_RUN,
        "API_KEY=hunter2",
        "password: hunter2",
    ];
    let attrs = || {
        credential_names.iter().fold(
            Attrs::new()
                .text("af.failure.reason", "plain words")
                .id("af.context.manifest", DIGEST)
                .int("af.count", 3),
            |attrs, name| attrs.text(name, "ok"),
        )
    };
    for level in [Level::Metrics, Level::Spans, Level::Content] {
        let temp = tempfile::tempdir().unwrap();
        let writer = crate::writer(temp.path(), level);
        let span = writer
            .span_start(None, None, "af.command", SpanKind::Internal, attrs())
            .unwrap();
        writer.event(&span, "af.note", attrs());
        writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
        let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
        let text = String::from_utf8(crate::sealed_bytes(&sealed.path)).unwrap();
        for name in credential_names {
            let encoded = serde_json::to_string(name).unwrap();
            assert!(!text.contains(&encoded), "{level:?}: {name} written");
        }
        assert!(!text.contains("hunter2"), "{level:?}: hunter2 written");
        let lines = sealed_lines(&sealed.path);
        let mut judged = 0;
        for line in lines.iter().filter(|line| {
            (line["t"] == "span_start" && line["name"] == "af.command")
                || (line["t"] == "event" && line["name"] == "af.note")
        }) {
            let attributes = line["attributes"].as_object().unwrap();
            // The other attributes, the typed identity included, are kept; every
            // credential-shaped name is omitted and counted alike.
            assert_eq!(attributes["af.failure.reason"], "plain words", "{level:?}");
            assert_eq!(attributes["af.context.manifest"], DIGEST, "{level:?}");
            assert_eq!(attributes["af.count"], 3, "{level:?}");
            assert_eq!(
                attributes["af.redacted"],
                credential_names.len(),
                "{level:?}: {line}"
            );
            assert_eq!(attributes.len(), 4, "{level:?}: {attributes:?}");
            judged += 1;
        }
        assert!(judged >= 1, "{level:?}: no record carried the attributes");
    }
}

/// The secret-name rule applies to every attribute key at every level: the key stays, its value
/// is withheld whatever its shape (text, typed or array), and `af.redacted` counts it; the
/// counters af's own schema types under a `token` name keep their values.
#[test]
fn telemetry_secret_named_attribute_keys_withhold_values() {
    for level in [Level::Metrics, Level::Spans, Level::Content] {
        let temp = tempfile::tempdir().unwrap();
        let writer =
            review_telemetry::segment::writer::Writer::new(options(temp.path(), level)).unwrap();
        let attrs = || {
            Attrs::new()
                .text("api_key", "violet")
                .id("session_token", "violet-id")
                .int("db_password", 4242)
                .decimal("client_secret", "9876543210")
                .boolean("auth", true)
                .enumerated("x-auth-mode", "violet")
                .texts("credentials", vec!["violet".into(), "indigo".into()])
                .text("api_key_long", "violet ".repeat(400))
                .decimal("af.usage.input_tokens", "12")
                .text("af.usage.output_tokens", "violet")
                .id("af.node_id", "n1")
        };
        let span = writer
            .span_start(None, None, "af.command", SpanKind::Internal, attrs())
            .unwrap();
        writer.event(&span, "af.note", attrs());
        writer.span_end(&span, SpanStatus::Ok, attrs(), 0);
        let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
        let lines = sealed_lines(&sealed.path);
        let mut checked = 0;
        for line in &lines {
            let Some(attributes) = line.get("attributes") else {
                continue;
            };
            for key in [
                "api_key",
                "session_token",
                "db_password",
                "client_secret",
                "auth",
                "x-auth-mode",
                "credentials",
                "api_key_long",
                "af.usage.output_tokens",
            ] {
                assert_eq!(attributes[key], WITHHELD, "{level:?} {key}: {line}");
            }
            assert_eq!(attributes["af.usage.input_tokens"], "12", "{line}");
            assert_eq!(attributes["af.node_id"], "n1", "{line}");
            assert_eq!(attributes["af.redacted"], 9, "{line}");
            checked += 1;
        }
        assert_eq!(checked, 3, "{level:?}");
        let text = String::from_utf8(crate::sealed_bytes(&sealed.path)).unwrap();
        for planted in ["violet", "indigo", "4242", "9876543210"] {
            assert!(!text.contains(planted), "{level:?}: {planted} in {text}");
        }
        // Nothing became a blob: a withheld value is never content.
        assert!(!Layout::new(temp.path()).blobs().exists(), "{level:?}");
    }
}

/// The credential rule reaches every field of every record kind: a typed field of a `node`,
/// `finding`, `demand` or `config` record keeps a value that validates as an identity and is
/// judged whole otherwise, and a header's `argv` withholds the value of an option under a
/// secret name.
#[test]
fn telemetry_record_kinds_judge_every_field() {
    use review_telemetry::record::{ConfigRow, Demand, Finding, Node, schema};
    let planted = "API_KEY=violet";
    let temp = tempfile::tempdir().unwrap();
    let mut options = options(temp.path(), Level::Spans);
    options.argv = ["af", "task", "--api-key", "violet", "--model", "opus"]
        .map(str::to_string)
        .to_vec();
    let writer = review_telemetry::segment::writer::Writer::new(options).unwrap();
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.node(Node {
        seq: 0,
        schema: schema::NODE.into(),
        execution_id: planted.into(),
        plan_digest: DIGEST.into(),
        node_id: "n1".into(),
        kind: planted.into(),
        package: Some(planted.into()),
        depends_on: vec!["n0".into(), planted.into()],
        expected_nodes: 2,
    });
    writer.finding(Finding {
        seq: 0,
        schema: schema::FINDING.into(),
        span_id: span.clone(),
        key: "correctness:src/lib.rs:12".into(),
        severity: "high".into(),
        effective_severity: planted.into(),
        status: "open".into(),
        scope: planted.into(),
        file: None,
        line: Some(12),
        title: "a title".into(),
        report_artifact_id: DIGEST.into(),
        node_id: "n1".into(),
        worker: planted.into(),
        model: Some(planted.into()),
        round: None,
        epoch: None,
    });
    writer.demand(Demand {
        seq: 0,
        schema: schema::DEMAND.into(),
        span_id: span.clone(),
        demand_id: "d-1".into(),
        kind: planted.into(),
        required: true,
        subject_id: planted.into(),
        snapshot_id: DIGEST.into(),
        status: planted.into(),
    });
    writer.config(
        planted,
        vec![ConfigRow {
            key: "telemetry.level".into(),
            value: "\"spans\"".into(),
            layer: "built-in".into(),
            origin: None,
        }],
    );
    writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let text = String::from_utf8(crate::sealed_bytes(&sealed.path)).unwrap();
    assert!(!text.contains("violet"), "{text}");
    let lines = sealed_lines(&sealed.path);
    let of = |kind: &str| {
        lines
            .iter()
            .find(|line| line["t"] == kind)
            .unwrap_or_else(|| panic!("no {kind}"))
            .clone()
    };
    assert_eq!(
        of("segment")["argv"],
        json!(["af", "task", "--api-key", WITHHELD, "--model", "opus"])
    );
    let node = of("node");
    assert_eq!(node["execution_id"], WITHHELD);
    assert_eq!(node["kind"], WITHHELD);
    assert_eq!(node["package"], WITHHELD);
    assert_eq!(node["depends_on"], json!(["n0", WITHHELD]));
    assert_eq!(
        (node["plan_digest"].as_str(), node["node_id"].as_str()),
        (Some(DIGEST), Some("n1"))
    );
    let finding = of("finding");
    for field in ["effective_severity", "scope", "worker", "model"] {
        assert_eq!(finding[field], WITHHELD, "{field}");
    }
    assert_eq!(finding["key"], "correctness:src/lib.rs:12");
    assert_eq!(finding["report_artifact_id"], DIGEST);
    assert_eq!(finding["span_id"], span.as_str());
    let demand = of("demand");
    for field in ["kind", "subject_id", "status"] {
        assert_eq!(demand[field], WITHHELD, "{field}");
    }
    assert_eq!(demand["snapshot_id"], DIGEST);
    assert_eq!(demand["demand_id"], "d-1");
    assert_eq!(of("config")["execution_id"], WITHHELD);
}

/// A `HOME` the redactor cannot use is never a silent omission: the writer records one
/// `redaction` problem right after the header naming why home paths are not replaced.
#[test]
fn telemetry_unknown_home_is_reported() {
    for (home, why) in [
        (None, "HOME is not set"),
        (Some("relative/home"), "HOME is not absolute"),
        (Some("/"), "HOME is `/`"),
    ] {
        let redactor = Redactor::with_home_variable(home.map(std::ffi::OsStr::new));
        assert!(redactor.home_unknown().unwrap().contains(why), "{why}");
        let temp = tempfile::tempdir().unwrap();
        let mut options = options(temp.path(), Level::Spans);
        options.redactor = redactor;
        let writer = review_telemetry::segment::writer::Writer::new(options).unwrap();
        let span = writer
            .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
            .unwrap();
        writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
        let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
        let lines = sealed_lines(&sealed.path);
        assert_eq!(lines[1]["t"], "problem", "{lines:?}");
        assert_eq!(lines[1]["seq"], 2);
        assert_eq!(lines[1]["what"], "redaction");
        assert!(lines[1]["reason"].as_str().unwrap().contains(why));
        assert_eq!(
            lines.iter().filter(|line| line["t"] == "problem").count(),
            1,
            "{why}"
        );
    }
    let usable = Redactor::with_home_variable(Some(std::ffi::OsStr::new("/home/violet")));
    assert_eq!(usable.home_unknown(), None);
    assert_eq!(usable.replace("/home/violet/src"), "~/src");
}
