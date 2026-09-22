use review_core::{Arg, Command};
use review_runner::task::{
    MAX_WORKER_BYTES, WORKER_REPLY_FORMAT, WorkerContract, WorkerModelAdapter,
};
use review_runner_claude::task::ClaudeTaskAdapter;
use review_store::Cas;
use serde_json::{Value, json};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, time::Duration};

fn request(payload: Value) -> Value {
    json!({"schema":"af.worker-request/1","reply_format":WORKER_REPLY_FORMAT,
        "instructions":"Write the declared document", "inputs":{},"feedback":[],
        "output_schemas":{"document":payload}})
}

fn invoke(
    input: &Value,
    envelope: &Value,
    writable: bool,
) -> (review_runner::task::ModelWorkerReturn, String, Vec<u8>) {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let program = temp.path().join("provider");
    let quoted = envelope.to_string().replace('\'', "'\\''");
    std::fs::write(
        &program,
        format!("#!/bin/sh\ncat > received\nprintf '%s\\n' \"$@\" >&2\nprintf '%s' '{quoted}'\n"),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = ClaudeTaskAdapter::new(&Command::new(
        program.to_str().unwrap(),
        vec![Arg::literal("--model"), Arg::literal("claude-fixture-1")],
    ))
    .unwrap();
    let returned = adapter.invoke(
        &cas,
        temp.path(),
        input.to_string().into_bytes(),
        Duration::from_secs(5),
        writable,
    );
    let flags = returned
        .raw_artifact_ids
        .get(1)
        .map(|id| String::from_utf8(cas.get(id).unwrap()).unwrap())
        .unwrap_or_default();
    let received = std::fs::read(temp.path().join("received")).unwrap_or_default();
    if let Some(raw) = returned.raw_artifact_ids.first() {
        assert_eq!(cas.get(raw).unwrap(), envelope.to_string().as_bytes());
    }
    (returned, flags, received)
}

#[test]
fn typed_native_command_preserves_input_schema_and_role_permissions() {
    let payload = json!({"type":"object","required":["text"],"additionalProperties":false,
        "properties":{"text":{"type":"string"}}});
    let input = request(payload.clone());
    let reply = json!({"schema":"af.worker-reply/1","outputs":{"document":[{"text":"Hello"}]}});
    for writable in [false, true] {
        let (returned, flags, received) = invoke(
            &input,
            &json!({"is_error":false,"result":"```json\nignored prose\n```",
            "structured_output":reply,"usage":{"input_tokens":10,"output_tokens":5}}),
            writable,
        );
        assert_eq!(received, input.to_string().as_bytes());
        assert_eq!(
            serde_json::from_slice::<Value>(&returned.message.unwrap()).unwrap(),
            reply
        );
        assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 15);
        let flags: Vec<_> = flags.lines().collect();
        assert_eq!(flags.iter().filter(|v| **v == "--json-schema").count(), 1);
        let at = flags.iter().position(|v| *v == "--json-schema").unwrap();
        let schema: Value = serde_json::from_str(flags[at + 1]).unwrap();
        let mut nested = schema["properties"]["outputs"]["properties"]["document"]["items"].clone();
        assert_eq!(
            nested.as_object_mut().unwrap().remove("$id").unwrap(),
            "urn:afactory:worker-output:0"
        );
        assert_eq!(nested, payload);
        assert_eq!(
            schema["properties"]["outputs"]["additionalProperties"],
            false
        );
        for flag in ["--safe-mode", "--restricted", "--strict-mcp-config"] {
            assert!(flags.contains(&flag));
        }
        for (flag, expected) in [
            ("--permission-mode", "dontAsk"),
            (
                "--tools",
                if writable {
                    "Read,Glob,Grep,Edit,Write"
                } else {
                    "Read,Glob,Grep"
                },
            ),
            (
                "--allowedTools",
                if writable {
                    "Read,Glob,Grep,Edit,Write"
                } else {
                    "Read,Glob,Grep"
                },
            ),
        ] {
            let at = flags.iter().position(|v| *v == flag).unwrap();
            assert_eq!(flags[at + 1], expected);
        }
    }
}

#[test]
fn typed_reply_never_falls_back_and_failed_outputs_keep_accounting() {
    let payload = json!({"type":"object","required":["text"],"additionalProperties":false,"properties":{"text":{"type":"string"}}});
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let contract = WorkerContract::capture(
        &cas,
        json!({"type":"object"}),
        BTreeMap::from([("document".into(), payload.clone())]),
    )
    .unwrap();
    let good = json!({"schema":"af.worker-reply/1","outputs":{"document":[{"text":"Hello"}]}});
    for (structured, native_error, valid) in [
        (Some(good.clone()), false, true),
        (None, false, false),
        (Some(Value::Null), false, false),
        (Some(json!(good.to_string())), false, false),
        (Some(json!([good.clone()])), false, false),
        (
            Some(json!({"schema":"af.worker-reply/1","outputs":{"document":{"text":"Hello"}}})),
            false,
            false,
        ),
        (
            Some(json!({"schema":"af.worker-reply/1","outputs":{"document":[{"text":42}]}})),
            false,
            false,
        ),
        (
            Some(json!({"schema":"af.worker-reply/1","outputs":{"foreign":[]}})),
            false,
            false,
        ),
        (Some(good.clone()), true, false),
    ] {
        let mut envelope = json!({"is_error":native_error,"result":good.to_string(),"usage":{"input_tokens":11,"output_tokens":7,"cache_creation_input_tokens":3,"cache_read_input_tokens":5}});
        if let Some(structured) = structured {
            envelope["structured_output"] = structured;
        }
        let (returned, _, _) = invoke(&request(payload.clone()), &envelope, false);
        assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 21);
        assert!(returned.usage_observation.is_none());
        assert_eq!(
            returned
                .message
                .and_then(|m| contract.validate_reply(&m))
                .is_ok(),
            valid
        );
    }
}

#[test]
fn malformed_or_oversized_typed_requests_do_not_spawn_and_legacy_stays_textual() {
    let envelope = json!({"is_error":false,"result":"legacy result","structured_output":{"ignored":true},"usage":{"input_tokens":1,"output_tokens":1}});
    for input in [
        json!({"schema":"af.worker-request/1"}),
        request(json!({"description":"x".repeat(65536)})),
        request(json!({"$id":"https://untrusted.invalid","type":"object"})),
        request(json!({"description":"x".repeat(MAX_WORKER_BYTES)})),
    ] {
        let (returned, flags, received) = invoke(&input, &envelope, false);
        assert!(returned.message.is_err());
        assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 0);
        assert!(returned.raw_artifact_ids.is_empty());
        assert!(flags.is_empty() && received.is_empty());
    }
    let (returned, flags, _) = invoke(&json!({"legacy":"review"}), &envelope, false);
    assert_eq!(returned.message.unwrap(), b"legacy result");
    assert!(!flags.lines().any(|arg| arg == "--json-schema"));
}

#[test]
fn nested_payload_references_keep_their_own_roots_and_literal_values() {
    let first = json!({"type":"object","required":["text","literal"],
        "$defs":{"text":{"type":"string"}},
        "properties":{"text":{"$ref":"#/$defs/text"},"literal":{"const":{"$ref":"#/$defs/text"}}}});
    let second = json!({"type":"object","required":["text"],
        "$defs":{"text":{"type":"integer"}},"properties":{"text":{"$ref":"#/$defs/text"}}});
    let mut input = request(first.clone());
    input["output_schemas"]["other"] = second.clone();
    let (returned, flags, _) = invoke(
        &input,
        &json!({"is_error":false,"structured_output":{},"usage":{"input_tokens":1,"output_tokens":1}}),
        false,
    );
    assert!(returned.message.is_ok());
    let args: Vec<_> = flags.lines().collect();
    let schema: Value =
        serde_json::from_str(args[args.iter().position(|a| *a == "--json-schema").unwrap() + 1])
            .unwrap();
    let validator = jsonschema::draft202012::options().build(&schema).unwrap();
    let good = json!({"schema":"af.worker-reply/1","outputs":{"document":[{"text":"x","literal":{"$ref":"#/$defs/text"}}],"other":[{"text":42}]}});
    assert!(validator.is_valid(&good));
    let mut bad = good.clone();
    bad["outputs"]["document"][0]["text"] = json!(42);
    assert!(!validator.is_valid(&bad));
    bad = good.clone();
    bad["outputs"]["other"][0]["text"] = json!("wrong scope");
    assert!(!validator.is_valid(&bad));
    bad = good;
    bad["outputs"]["document"][0]["literal"]["$ref"] = json!("rewritten");
    assert!(!validator.is_valid(&bad));
}
