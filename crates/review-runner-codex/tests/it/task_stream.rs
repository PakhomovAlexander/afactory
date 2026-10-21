//! Codex items (design §7.3, §15 P6): `codex exec --json` is tapped as it arrives, its turns and
//! command and MCP items become turns and tool calls at `spans` and above, no `-c` key is
//! added, and the Store keeps the turn and error lines and the `-o` final message — so an
//! Attempt's accounting, classification and raw bytes are the same at every Telemetry level.

use crate::load_safe_wall::LOAD_SAFE_WALL;
use crate::telemetry_levels::{LEVELS, at_level};
use review_core::{Arg, Command};
use review_runner::task::{ModelWorkerReturn, WorkerAccess, WorkerModelAdapter};
use review_runner_codex::task::CodexTaskAdapter;
use review_store::Cas;
use review_telemetry::level::Level;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// A Codex session: a thread, one turn with a command, a reasoning summary, an MCP call and the
/// agent's message, and its usage.
fn session(message: &str, command_output: &str) -> String {
    [
        json!({"type":"thread.started","thread_id":"0199a213-81c0-7800-8aa1-bbab2a035a53"}),
        json!({"type":"turn.started"}),
        json!({"type":"item.started","item":{"id":"item_0","type":"command_execution",
               "command":"bash -lc 'cat lib.rs'","aggregated_output":"","status":"in_progress"}}),
        json!({"type":"item.completed","item":{"id":"item_0","type":"command_execution",
               "command":"bash -lc 'cat lib.rs'","aggregated_output":command_output,
               "exit_code":0,"status":"completed"}}),
        json!({"type":"item.completed","item":{"id":"item_1","type":"reasoning",
               "text":"**Reading** the source"}}),
        json!({"type":"item.started","item":{"id":"item_2","type":"mcp_tool_call","server":"docs",
               "tool":"lookup","arguments":{"q":"x"},"status":"in_progress"}}),
        json!({"type":"item.completed","item":{"id":"item_2","type":"mcp_tool_call","server":"docs",
               "tool":"lookup","arguments":{"q":"x"},"result":{"content":[]},"status":"completed"}}),
        json!({"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":message}}),
        json!({"type":"turn.completed","usage":{"input_tokens":1200,"cached_input_tokens":200,
               "output_tokens":30,"reasoning_output_tokens":12}}),
    ]
    .iter()
    .map(|line| line.to_string() + "\n")
    .collect()
}

/// A fake `codex` printing `stdout` and `stderr` and writing `reply` to its `-o` file, recording
/// its arguments.
fn fake(dir: &Path, stdout: &str, stderr: &str, reply: &str) -> std::path::PathBuf {
    std::fs::write(dir.join("out"), stdout).unwrap();
    std::fs::write(dir.join("err"), stderr).unwrap();
    std::fs::write(dir.join("reply"), reply).unwrap();
    let program = dir.join("codex");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' \"$*\" >>'{dir}/args'\nout=; prev=; for arg in \"$@\"; do [ \"$prev\" = -o ] && out=$arg; prev=$arg; done\ncat '{dir}/reply' >\"$out\"\ncat '{dir}/out'\ncat '{dir}/err' >&2\n",
            dir = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    program
}

fn invoke(program: &Path, cas: &Cas, dir: &Path) -> ModelWorkerReturn {
    let adapter = CodexTaskAdapter::new(&Command::new(
        program.to_str().unwrap(),
        [
            "--model",
            "gpt-fixture-1",
            "-c",
            "model_reasoning_effort=\"high\"",
        ]
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
fn codex_accounting_identical_at_every_level() {
    let failed = [
        json!({"type":"thread.started","thread_id":"t"}).to_string(),
        json!({"type":"turn.started"}).to_string(),
        json!({"type":"item.completed","item":{"id":"item_0","type":"agent_message",
               "text":"partial"}})
        .to_string(),
        json!({"type":"turn.failed","error":{"message":"stream disconnected: connection refused"}})
            .to_string(),
    ]
    .join("\n")
        + "\n";
    for (name, stdout, reply) in [
        ("success", session("OK", "pub fn example() {}\n"), "OK"),
        ("failure", failed, ""),
    ] {
        let mut seen = Vec::new();
        for level in LEVELS {
            let dir = tempfile::tempdir().unwrap();
            let cas = Cas::open(dir.path().join("cas")).unwrap();
            let program = fake(dir.path(), &stdout, "a diagnostic line\n", reply);
            let (returned, recorded) = at_level(level, &dir.path().join("telemetry"), || {
                invoke(&program, &cas, dir.path())
            });
            seen.push(outcome(&returned, &cas));
            // Telemetry adds no `-c` key: the invocation is the same at every level.
            let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
            assert_eq!(args.matches("-c ").count(), 1, "{args}");
            let turns = recorded.map(|recorded| recorded.ends("af.turn").len());
            match level {
                Level::Off => assert_eq!(turns, None),
                Level::Metrics => assert_eq!(turns, Some(0), "{name}"),
                _ => assert_eq!(turns, Some(1), "{name} {level:?}"),
            }
        }
        for (level, outcome) in LEVELS.iter().zip(&seen) {
            assert_eq!(outcome, &seen[0], "{name} at {level:?}");
        }
        assert!(!seen[0].contains("item.completed"), "{name}: {}", seen[0]);
    }
}

#[test]
fn telemetry_codex_items_become_spans_through_the_adapter() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let stdout = session("the answer", "pub fn example() {}\n");
    let program = fake(dir.path(), &stdout, "", "the answer");
    let (returned, recorded) = at_level(Level::Spans, &dir.path().join("telemetry"), || {
        invoke(&program, &cas, dir.path())
    });
    assert_eq!(returned.message.as_deref(), Ok(&b"the answer"[..]));
    let recorded = recorded.unwrap();
    // The Store: the turn line and the final message; never an item's line.
    let raw: Vec<Vec<u8>> = returned
        .raw_artifact_ids
        .iter()
        .map(|id| cas.get(id).unwrap())
        .collect();
    assert_eq!(raw.len(), 2);
    assert_eq!(
        raw[0],
        stdout
            .lines()
            .last()
            .unwrap()
            .to_string()
            .into_bytes()
            .into_iter()
            .chain(*b"\n")
            .collect::<Vec<u8>>()
    );
    assert_eq!(raw[1], b"the answer");
    let turn = &recorded.ends("af.turn")[0];
    assert_eq!(turn["af.usage.input_tokens"], "1200");
    assert_eq!(turn["af.usage.cache_read_tokens"], "200");
    let tools = recorded.ends("af.tool");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["af.tool.exit_code"], 0);
    assert_eq!(tools[0]["af.tool.output_bytes"], 20);
    assert_eq!(recorded.events("af.item.reasoning").len(), 1);
    assert_eq!(
        recorded.events("af.item.agent_message")[0]["af.item.text_bytes"],
        10
    );
    for words in ["the answer", "Reading", "pub fn example", "lookup"] {
        assert!(!recorded.text.contains(words), "{words}");
    }
    let harness = &recorded.ends("af.harness")[0];
    assert_eq!(harness["af.native"], "unavailable");
    assert_eq!(harness["gen_ai.provider.name"], "openai");
}

/// Design §7.3: a successful final message does not take the adapter's privacy filter, so
/// Telemetry's own decision covers it: a device code and a challenge URL separated across two
/// items, or across stdout and stderr, withhold the Attempt's pending content and nothing of
/// either is recorded, while the Attempt succeeds and is charged as at `off`.
#[test]
fn telemetry_release_decision_covers_success() {
    for (stdout, stderr) in [
        (
            session(
                "use the device code WXYZ-9876",
                "see https://auth.openai.com/codex/device\n",
            ),
            String::new(),
        ),
        (
            session("use the device code WXYZ-9876", "nothing\n"),
            "open https://auth.openai.com/codex/device\n".to_string(),
        ),
    ] {
        let mut charged = Vec::new();
        for level in [Level::Off, Level::Content] {
            let dir = tempfile::tempdir().unwrap();
            let cas = Cas::open(dir.path().join("cas")).unwrap();
            let program = fake(dir.path(), &stdout, &stderr, "OK");
            let (returned, recorded) = at_level(level, &dir.path().join("telemetry"), || {
                invoke(&program, &cas, dir.path())
            });
            assert_eq!(
                returned.message.as_deref(),
                Ok(&b"OK"[..]),
                "classification unchanged"
            );
            charged.push(format!("{:?}", returned.usage));
            let Some(recorded) = recorded else {
                continue;
            };
            assert_eq!(recorded.ends("af.harness")[0]["af.stream.withheld"], true);
            let ids = &recorded.events("af.harness.ids")[0];
            assert_eq!(ids["af.stream.withheld"], true);
            assert!(ids.get("af.harness.ids").is_none());
            for words in ["WXYZ", "auth.openai.com", "0199a213"] {
                assert!(!recorded.text.contains(words), "{words}");
            }
        }
        assert_eq!(charged[0], charged[1]);
    }
}
