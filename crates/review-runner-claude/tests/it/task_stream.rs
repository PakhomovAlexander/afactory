//! Claude `stream-json` (design §7.3, §12, §15 P6): the result line is the envelope `json`
//! prints, the adapter reads it and nothing else, the Store keeps it with stderr, and so an
//! Attempt's accounting, classification and raw bytes are the same under both shapes and at
//! every Telemetry level, while at `spans` and above its messages become turns and tool calls.

use crate::load_safe_wall::LOAD_SAFE_WALL;
use crate::telemetry_levels::{LEVELS, at_level};
use review_core::{Arg, Command};
use review_runner::task::{ModelWorkerReturn, WorkerAccess, WorkerModelAdapter};
use review_runner_claude::task::{ClaudeTaskAdapter, native_output, result_output};
use review_store::Cas;
use review_telemetry::level::Level;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const MODEL: &str = "claude-fixture-1";

/// The `stream-json` lines that precede `envelope`: init, a tool-calling message, the tool's
/// result, the final message; then the envelope itself as the `result` line.
fn stream_of(envelope: &Value, text: &str, tool_output: &str) -> String {
    let mut lines = vec![
        json!({"type":"system","subtype":"init","session_id":"0c9a3f1e-2b4d-4c6e-8f0a-1b2c3d4e5f60",
               "model":MODEL,"tools":["Read","Glob","Grep"],"mcp_servers":[]}),
        json!({"type":"assistant","message":{"id":"msg_01StreamOne","model":MODEL,"stop_reason":"tool_use",
               "usage":{"input_tokens":3,"output_tokens":4},
               "content":[{"type":"tool_use","id":"toolu_01StreamRead","name":"Read",
                           "input":{"file_path":"lib.rs"}}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result",
               "tool_use_id":"toolu_01StreamRead","content":tool_output}]}}),
        json!({"type":"assistant","message":{"id":"msg_01StreamTwo","model":MODEL,"stop_reason":"end_turn",
               "usage":{"input_tokens":5,"output_tokens":6},
               "content":[{"type":"text","text":text}]}}),
    ];
    lines.push(envelope.clone());
    lines.iter().map(|line| line.to_string() + "\n").collect()
}

/// The pairs: what `--output-format json` prints, and the same Attempt under `stream-json`.
fn pairs() -> Vec<(&'static str, String, String)> {
    let usage = json!({"input_tokens":8,"output_tokens":10,"cache_creation_input_tokens":2,
                       "cache_read_input_tokens":40});
    let model_usage = json!({MODEL:{"inputTokens":8,"outputTokens":10}});
    let cases = [
        (
            "success",
            json!({"type":"result","subtype":"success","is_error":false,"result":"OK",
                   "usage":usage,"modelUsage":model_usage}),
        ),
        (
            "authentication",
            json!({"type":"result","subtype":"success","is_error":true,
                   "result":"Invalid API key · Please run /login","usage":usage}),
        ),
        (
            "refusal",
            json!({"type":"result","subtype":"success","is_error":false,"result":"I can't help",
                   "stop_reason":"refusal","usage":usage,"modelUsage":model_usage}),
        ),
        (
            "malformed usage",
            json!({"type":"result","subtype":"success","is_error":false,"result":"OK",
                   "usage":{"input_tokens":"many","output_tokens":10}}),
        ),
        (
            "another model",
            json!({"type":"result","subtype":"success","is_error":false,"result":"OK","usage":usage,
                   "modelUsage":{MODEL:{"inputTokens":8},"claude-other-9":{"inputTokens":1}}}),
        ),
    ];
    cases
        .into_iter()
        .map(|(name, envelope)| {
            let json = envelope.to_string() + "\n";
            (
                name,
                json,
                stream_of(&envelope, "the final words", "pub fn example() {}"),
            )
        })
        .collect()
}

/// A fake `claude` printing `stdout` (`stream` when af asks for `stream-json`, when given) and
/// `stderr`, recording the arguments it was given.
fn fake(dir: &Path, json: &str, stream: Option<&str>, stderr: &str) -> std::path::PathBuf {
    std::fs::write(dir.join("json-out"), json).unwrap();
    std::fs::write(dir.join("stream-out"), stream.unwrap_or(json)).unwrap();
    std::fs::write(dir.join("err-out"), stderr).unwrap();
    let program = dir.join("claude");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' \"$*\" >>'{dir}/args'\ncase \"$*\" in *stream-json*) cat '{dir}/stream-out';; *) cat '{dir}/json-out';; esac\ncat '{dir}/err-out' >&2\n",
            dir = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    program
}

fn invoke(program: &Path, cas: &Cas, dir: &Path) -> ModelWorkerReturn {
    let adapter = ClaudeTaskAdapter::new(&Command::new(
        program.to_str().unwrap(),
        ["--model", MODEL, "--effort", "high"]
            .into_iter()
            .map(Arg::literal)
            .collect(),
    ))
    .unwrap();
    adapter.invoke(
        cas,
        dir,
        b"review".to_vec(),
        LOAD_SAFE_WALL,
        WorkerAccess::ReadOnly,
        None,
        &[],
    )
}

/// What an Attempt came to that the kernel records: its message, usage, classification and the
/// bytes of its raw evidence.
fn outcome(returned: &ModelWorkerReturn, cas: &Cas) -> String {
    let raw: Vec<String> = returned
        .raw_artifact_ids
        .iter()
        .map(|id| String::from_utf8_lossy(&cas.get(id).unwrap()).into_owned())
        .collect();
    format!(
        "message {:?}\nusage {:?}\nobservation {:?}\nfailure {:?}\nraw {raw:?}",
        returned.message, returned.usage, returned.usage_observation, returned.native_failure
    )
}

#[test]
fn claude_stream_result_envelope_is_the_json_envelope() {
    for (name, json, stream) in pairs() {
        // The rule itself: the stream's result line is the json envelope, byte for byte.
        assert_eq!(
            result_output(stream.as_bytes()),
            result_output(json.as_bytes()),
            "{name}"
        );
        assert_eq!(result_output(json.as_bytes()), json.as_bytes(), "{name}");
        assert_eq!(
            native_output(stream.as_bytes()).as_ref(),
            json.as_bytes(),
            "{name}"
        );
        // The adapter reads that line and nothing else under either shape.
        let mut seen = Vec::new();
        for printed in [&json, &stream] {
            let dir = tempfile::tempdir().unwrap();
            let cas = Cas::open(dir.path().join("cas")).unwrap();
            let program = fake(dir.path(), printed, None, "");
            seen.push(outcome(&invoke(&program, &cas, dir.path()), &cas));
        }
        assert_eq!(seen[0], seen[1], "{name}");
        assert!(!seen[1].contains("the final words"), "{name}: {}", seen[1]);
    }
    // The adapter reads the native text of a stream with no result line, and a stdout that
    // frames no message whole; the Store keeps none of that native text, only a `json` document.
    let lines = format!(
        "{}\nError: something native went wrong\n",
        json!({"type":"system","subtype":"init","session_id":"s"})
    );
    assert_eq!(
        native_output(lines.as_bytes()).as_ref(),
        b"Error: something native went wrong\n"
    );
    assert_eq!(result_output(lines.as_bytes()), b"");
    assert_eq!(native_output(b"plain text").as_ref(), b"plain text");
    assert_eq!(result_output(b"plain text"), b"");
    let typeless = b"{\"is_error\":false,\"result\":\"OK\"}\n";
    assert_eq!(result_output(typeless), typeless);
}

/// The raw-output rule (design §7.3) at every level: native text outside the message framing —
/// beside valid messages and a result line, or in a stream with no result line — is classified
/// from the complete capture and never reaches the Store, however large; with a result line the
/// Store keeps it and stderr, without one stderr alone (or the adapter's replacement).
#[test]
fn claude_store_keeps_no_unframed_output() {
    let envelope = json!({"type":"result","subtype":"success","is_error":false,"result":"OK",
        "usage":{"input_tokens":8,"output_tokens":10,"cache_creation_input_tokens":0},
        "modelUsage":{MODEL:{"inputTokens":8,"outputTokens":10}}});
    let json = envelope.to_string() + "\n";
    let stream = stream_of(&envelope, "the final words", "pub fn example() {}");
    let noise = format!("native noise {}\n", "n".repeat(256 * 1024));
    let beside: String = stream
        .lines()
        .flat_map(|line| [line.to_string() + "\n", noise.clone()])
        .collect();
    let mut no_result: Vec<&str> = stream.lines().collect();
    no_result.pop();
    let no_result =
        no_result.join("\n") + "\nError: something native went wrong\n" + noise.as_str();
    let auth = format!(
        "{}\nInvalid API key · Please run /login\n",
        json!({"type":"system","subtype":"init","session_id":"s"})
    );
    for (name, printed) in [
        ("beside a result line", &beside),
        ("no result line", &no_result),
        ("an authentication failure", &auth),
    ] {
        let mut seen = Vec::new();
        for level in LEVELS {
            let dir = tempfile::tempdir().unwrap();
            let cas = Cas::open(dir.path().join("cas")).unwrap();
            let program = fake(dir.path(), printed, None, "diagnostic\n");
            let (returned, _) = at_level(level, &dir.path().join("telemetry"), || {
                invoke(&program, &cas, dir.path())
            });
            let raw: Vec<Vec<u8>> = returned
                .raw_artifact_ids
                .iter()
                .map(|id| cas.get(id).unwrap())
                .collect();
            match name {
                "beside a result line" => {
                    assert_eq!(returned.message.as_deref(), Ok(&b"OK"[..]), "{level:?}");
                    assert_eq!(raw, [json.as_bytes(), b"diagnostic\n"], "{level:?}");
                }
                "no result line" => {
                    let message = returned.message.as_ref().unwrap_err();
                    assert!(message.starts_with("Claude Worker"), "{level:?}: {message}");
                    assert_eq!(raw, [b"diagnostic\n"], "{level:?}");
                }
                _ => {
                    // The adapter's replacement is what the Store keeps.
                    let kind = returned.native_failure.unwrap();
                    assert!(kind.is_auth(), "{level:?}: {kind:?}");
                    assert_eq!(raw, [kind.diagnostic().as_bytes()], "{level:?}");
                }
            }
            seen.push(outcome(&returned, &cas));
        }
        for (level, outcome) in LEVELS.iter().zip(&seen) {
            assert_eq!(outcome, &seen[0], "{name} at {level:?}");
            assert!(!outcome.contains("native noise"), "{name} at {level:?}");
            assert!(!outcome.contains("\"type\":\"assistant\""), "{name}");
        }
    }
}

#[test]
fn claude_accounting_identical_at_every_level() {
    for (name, json, stream) in pairs() {
        let mut seen = Vec::new();
        for level in LEVELS {
            let dir = tempfile::tempdir().unwrap();
            let cas = Cas::open(dir.path().join("cas")).unwrap();
            let program = fake(dir.path(), &json, Some(&stream), "a diagnostic line\n");
            let (returned, recorded) = at_level(level, &dir.path().join("telemetry"), || {
                invoke(&program, &cas, dir.path())
            });
            seen.push(outcome(&returned, &cas));
            let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
            let streamed = matches!(level, Level::Spans | Level::Content);
            // The invocation-cost gate failed (`docs/telemetry.md`): `stream-json` only where
            // Telemetry taps it.
            assert_eq!(
                args.contains("--output-format stream-json --verbose"),
                streamed,
                "{name} {level:?}: {args}"
            );
            assert_eq!(args.contains("--output-format json"), !streamed, "{args}");
            let recorded = recorded.map(|recorded| recorded.ends("af.turn").len());
            match level {
                Level::Off => assert_eq!(recorded, None),
                Level::Metrics => assert_eq!(recorded, Some(0), "{name}"),
                _ => assert_eq!(recorded, Some(2), "{name} {level:?}"),
            }
        }
        for (level, outcome) in LEVELS.iter().zip(&seen) {
            assert_eq!(outcome, &seen[0], "{name} at {level:?}");
        }
    }
}

/// Design §7.3: a successful envelope does not take the adapter's whole-capture privacy filter,
/// so Telemetry's own decision covers it: a device code and a challenge URL separated across two
/// records of the stream, or across stdout and stderr, withhold the Attempt's pending content;
/// nothing of either is recorded; the Attempt succeeds and is charged exactly as at `off`.
#[test]
fn telemetry_release_decision_covers_success() {
    let envelope = json!({"type":"result","subtype":"success","is_error":false,"result":"OK",
        "usage":{"input_tokens":8,"output_tokens":10,"cache_creation_input_tokens":0},
        "modelUsage":{MODEL:{"inputTokens":8,"outputTokens":10}}});
    let json = envelope.to_string() + "\n";
    for (stream, stderr) in [
        (
            stream_of(
                &envelope,
                "enter the device code WXYZ-9876",
                "see https://claude.ai/oauth/authorize?code=true",
            ),
            String::new(),
        ),
        (
            stream_of(&envelope, "enter the device code WXYZ-9876", "nothing here"),
            "visit https://claude.ai/oauth/authorize to continue\n".to_string(),
        ),
    ] {
        let mut charged = Vec::new();
        for level in [Level::Off, Level::Content] {
            let dir = tempfile::tempdir().unwrap();
            let cas = Cas::open(dir.path().join("cas")).unwrap();
            let program = fake(dir.path(), &json, Some(&stream), &stderr);
            let (returned, recorded) = at_level(level, &dir.path().join("telemetry"), || {
                invoke(&program, &cas, dir.path())
            });
            assert_eq!(
                returned.message.as_deref(),
                Ok(&b"OK"[..]),
                "classification unchanged"
            );
            assert_eq!(returned.native_failure, None);
            charged.push(format!("{:?}", returned.usage));
            let Some(recorded) = recorded else {
                continue;
            };
            let harness = &recorded.ends("af.harness")[0];
            assert_eq!(harness["af.stream.withheld"], true);
            assert_eq!(harness["af.native.withheld"], true);
            let ids = &recorded.events("af.harness.ids")[0];
            assert_eq!(ids["af.stream.withheld"], true);
            assert!(ids.get("af.harness.ids").is_none());
            for words in [
                "WXYZ",
                "claude.ai",
                "oauth",
                "toolu_01Stream",
                "msg_01Stream",
            ] {
                assert!(!recorded.text.contains(words), "{words}");
            }
            assert_eq!(
                recorded.ends("af.turn").len(),
                2,
                "the structure is still recorded"
            );
        }
        assert_eq!(charged[0], charged[1]);
    }
}

/// Design §7.3: at `spans` a Claude Attempt's messages become turns and tool calls, with the
/// harness's usage and stop reasons, and the Store keeps the result line and stderr only.
#[test]
fn telemetry_claude_messages_become_spans_through_the_adapter() {
    let (_, json, stream) = pairs().remove(0);
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let program = fake(dir.path(), &json, Some(&stream), "diagnostic\n");
    let (returned, recorded) = at_level(Level::Spans, &dir.path().join("telemetry"), || {
        invoke(&program, &cas, dir.path())
    });
    let recorded = recorded.unwrap();
    assert_eq!(
        cas.get(&returned.raw_artifact_ids[0]).unwrap(),
        json.as_bytes(),
        "the Store keeps the result line"
    );
    assert_eq!(
        cas.get(&returned.raw_artifact_ids[1]).unwrap(),
        b"diagnostic\n"
    );
    assert_eq!(returned.raw_artifact_ids.len(), 2);
    let turns = recorded.ends("af.turn");
    assert_eq!(
        turns[0]["gen_ai.response.finish_reasons"],
        json!(["tool_use"])
    );
    assert_eq!(turns[0]["af.usage.input_tokens"], "3");
    assert_eq!(
        turns[1]["gen_ai.response.finish_reasons"],
        json!(["end_turn"])
    );
    let tools = recorded.ends("af.tool");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["af.tool.output_bytes"], 19);
    assert!(!recorded.text.contains("the final words"));
    let harness = &recorded.ends("af.harness")[0];
    assert_eq!(harness["dropped_chunks"], 0);
    assert_eq!(harness["gaps"], 0);
    assert_eq!(harness["af.stream.withheld"], false);
}
