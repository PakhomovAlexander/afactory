//! `af/error@1`, the document every af command prints under `--json` when it fails: the four
//! fields every error has, and the typed `cause`, `next_action` and `details` a classified
//! refusal adds, each cause with exactly its own facts.

use serde_json::json;

use super::{assert_invalid, assert_valid};

const ERROR_V1: &str = "error-v1.json";

#[test]
fn an_error_document_carries_its_cause_next_command_and_facts() {
    let plain = json!({
        "schema": "af/error@1",
        "command": "af task",
        "error": "Task nope not found in this checkout's Task Store; af task list lists its Tasks",
        "exit_code": 1,
    });
    assert_valid(ERROR_V1, &plain);
    let exhausted = json!({
        "schema": "af/error@1",
        "command": "af task",
        "error": "cannot continue Task pagination-cli: node root.nodes.implement was interrupted 2 times (recovery allowance 1); fix: ...",
        "exit_code": 1,
        "cause": "recovery_exhausted",
        "next_action": "af task start --file TASK_FILE --state ../state",
        "details": {"node": "root.nodes.implement", "count": 2, "writer_lost": 0,
            "host_cancelled": 2, "allowance": 1},
    });
    assert_valid(ERROR_V1, &exhausted);
    let usage = json!({
        "schema": "af/error@1",
        "command": "af review plan",
        "error": "unexpected argument '--unknown' found",
        "exit_code": 2,
        "cause": "usage",
        "next_action": "af review plan --help",
    });
    assert_valid(ERROR_V1, &usage);
    let refreshed = json!({
        "schema": "af/error@1",
        "command": "af task",
        "error": "refreshed source: No Pipeline selected for Task t: unavailable: a Worker or check cannot run here",
        "exit_code": 4,
        "cause": "refresh_refused",
        "next_action": "af task show t",
        "details": {"decision": "unavailable"},
    });
    assert_valid(ERROR_V1, &refreshed);
    let file = json!({
        "schema": "af/error@1",
        "command": "af task",
        "error": "Task file zero.json: limits.wall_ms=0 must be positive",
        "exit_code": 1,
        "cause": "task_file_invalid",
        "next_action": "af task plan --file zero.json",
        "details": {"file": "zero.json", "field": "limits.wall_ms", "value": 0},
    });
    assert_valid(ERROR_V1, &file);
    let goal = json!({
        "schema": "af/error@1",
        "command": "af task",
        "error": "cannot open goal file goal.md: No such file or directory (os error 2)",
        "exit_code": 1,
        "cause": "input_unreadable",
        "next_action": "af task run fix-2 --from fix-1 --goal goal.md",
        "details": {"file": "goal.md", "what": "goal file"},
    });
    assert_valid(ERROR_V1, &goal);

    for (document, pointer, replacement, why) in [
        (&plain, "/schema", json!("af/error@2"), "another version"),
        (&plain, "/exit_code", json!(0), "an error that exits 0"),
        (&plain, "/error", json!(""), "an error without its sentence"),
        (
            &plain,
            "/command",
            json!("task"),
            "a command that is not af's",
        ),
        (&usage, "/exit_code", json!(1), "a usage error that exits 1"),
        (
            &exhausted,
            "/cause",
            json!("tired"),
            "a cause af does not name",
        ),
        (
            &exhausted,
            "/next_action",
            json!("rm -rf ."),
            "a next command that is not af's",
        ),
        (
            &exhausted,
            "/next_action",
            json!(null),
            "exhaustion without its fix",
        ),
        (&exhausted, "/details/count", json!(-1), "a negative count"),
        (
            &refreshed,
            "/exit_code",
            json!(1),
            "a refused refresh that is not waiting",
        ),
        (
            &refreshed,
            "/details/decision",
            json!("later"),
            "a decision no selection makes",
        ),
    ] {
        let mut changed = document.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert_invalid(ERROR_V1, &changed, why);
    }
    for (document, field) in [
        (&exhausted, "details"),
        (&exhausted, "next_action"),
        (&usage, "next_action"),
    ] {
        let mut missing = document.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert_invalid(ERROR_V1, &missing, field);
    }
    let mut stray = exhausted.clone();
    stray["details"]["file"] = json!("ticket.json");
    assert_invalid(ERROR_V1, &stray, "a fact of another cause");
    let mut orphan = plain.clone();
    orphan["next_action"] = json!("af task list");
    assert_invalid(ERROR_V1, &orphan, "a next command without its cause");
    let mut extra = plain;
    extra["hint"] = json!("try again");
    assert_invalid(ERROR_V1, &extra, "a field the document does not have");
}
