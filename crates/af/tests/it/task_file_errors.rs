//! A malformed Task file is refused by the field a person wrote: its path in the file, the
//! shape it takes and the value it holds, never the parser's raw text alone. A wrong duration
//! flag is refused before the file is read.

use std::path::Path;
use std::process::Output;

use serde_json::{Value, json};

fn af(dir: &Path, args: &[&str]) -> Output {
    crate::common::af()
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn ticket() -> Value {
    json!({
        "schema": "af.task-file/1",
        "task_id": "ticket-1",
        "kind": "software",
        "goal": "Add a flag",
        "strategy": "light",
        "limits": {
            "tokens": 6000000,
            "max_attempts": 3,
            "wall_ms": 600000,
            "verification": {"tokens": 1000, "attempts": 1, "wall_ms": 60000}
        }
    })
}

/// `af task plan --file NAME` over `bytes`, refused with exit 1: what it said.
fn refused(dir: &Path, name: &str, bytes: &[u8]) -> String {
    std::fs::write(dir.join(name), bytes).unwrap();
    let state = dir.join("state");
    let output = af(
        dir,
        &[
            "task",
            "plan",
            "--file",
            name,
            "--state",
            state.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{}", err(&output));
    assert!(!state.exists(), "a refused file created state");
    err(&output)
}

#[test]
fn malformed_task_files_name_the_field_and_its_shape() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();

    // A number written as text.
    let mut tokens = ticket();
    tokens["limits"]["tokens"] = json!("6000000");
    let said = refused(dir, "tokens.json", tokens.to_string().as_bytes());
    assert!(
        said.contains(
            "task file tokens.json: limits.tokens must be an integer from 0 to \
             9007199254740991, got \"6000000\" (string)"
        ),
        "{said}"
    );

    // A misspelt field, in a TOML file.
    let said = refused(
        dir,
        "goals.toml",
        b"schema = \"af.task-file/1\"\ntask_id = \"ticket-1\"\nkind = \"software\"\n\
          goals = \"Add a flag\"\nstrategy = \"light\"\n",
    );
    assert!(
        said.contains(
            "task file goals.toml: goals is not a Task file field; its fields are schema, \
             task_id, kind, goal"
        ),
        "{said}"
    );

    // A nested field missing.
    let mut reserve = ticket();
    reserve["limits"]["verification"]
        .as_object_mut()
        .unwrap()
        .remove("attempts");
    let said = refused(dir, "reserve.json", reserve.to_string().as_bytes());
    assert!(
        said.contains(
            "task file reserve.json: limits.verification.attempts is missing; the reserve \
             holds tokens, attempts, wall_ms"
        ),
        "{said}"
    );

    // Not JSON at all: the parser's words, under the file's name.
    let said = refused(dir, "broken.json", b"{\"schema\": ");
    assert!(
        said.contains("task file broken.json is not valid JSON: "),
        "{said}"
    );

    // A wrong identity is named by its field too.
    let mut identity = ticket();
    identity["task_id"] = json!("-leading-dash");
    let said = refused(dir, "identity.json", identity.to_string().as_bytes());
    assert!(
        said.contains("task file identity.json: task_id \"-leading-dash\" is not a Task ID"),
        "{said}"
    );
}

#[test]
fn a_zero_duration_flag_is_refused_before_the_task_file_is_read() {
    let dir = tempfile::tempdir().unwrap();
    // The file does not exist: a refusal that read it first would say so instead.
    let output = af(
        dir.path(),
        &[
            "review",
            "run",
            "--file",
            "missing.json",
            "--timeout-secs",
            "0",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{}", err(&output));
    assert!(
        err(&output).contains("--timeout-secs must be positive"),
        "{}",
        err(&output)
    );
    assert!(!err(&output).contains("missing.json"), "{}", err(&output));
    // `af task start` refuses it as a usage error, before anything at all.
    let output = af(
        dir.path(),
        &[
            "task",
            "start",
            "--file",
            "missing.json",
            "--timeout-secs",
            "0",
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", err(&output));
}
