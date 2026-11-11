//! `af/error@2`, the document every af command prints under `--json` when it fails: the four
//! fields every error has, and the typed `cause`, `next_action` and `details` a classified
//! refusal adds, each cause with exactly its own facts. Version 2 adds `interrupted`
//! (ADR-0256); a version 1 document fails closed.

use serde_json::json;

use super::{assert_invalid, assert_valid};

const ERROR_V2: &str = "error-v2.json";

#[test]
fn an_error_document_carries_its_cause_next_command_and_facts() {
    let plain = json!({
        "schema": "af/error@2",
        "command": "af task",
        "error": "Task nope not found in this checkout's Task Store; af task list lists its Tasks",
        "exit_code": 1,
    });
    assert_valid(ERROR_V2, &plain);
    let exhausted = json!({
        "schema": "af/error@2",
        "command": "af task",
        "error": "cannot continue Task pagination-cli: node root.nodes.implement was interrupted 2 times (recovery allowance 1); fix: ...",
        "exit_code": 1,
        "cause": "recovery_exhausted",
        "next_action": "af task start --file TASK_FILE --state ../state",
        "details": {"node": "root.nodes.implement", "count": 2, "writer_lost": 0,
            "host_cancelled": 2, "allowance": 1},
    });
    assert_valid(ERROR_V2, &exhausted);
    let usage = json!({
        "schema": "af/error@2",
        "command": "af review plan",
        "error": "unexpected argument '--unknown' found",
        "exit_code": 2,
        "cause": "usage",
        "next_action": "af review plan --help",
    });
    assert_valid(ERROR_V2, &usage);
    let refreshed = json!({
        "schema": "af/error@2",
        "command": "af task",
        "error": "refreshed source: No Pipeline selected for Task t: unavailable: a Worker or check cannot run here",
        "exit_code": 4,
        "cause": "refresh_refused",
        "next_action": "af task show t",
        "details": {"decision": "unavailable"},
    });
    assert_valid(ERROR_V2, &refreshed);
    let file = json!({
        "schema": "af/error@2",
        "command": "af task",
        "error": "Task file zero.json: limits.wall_ms=0 must be positive",
        "exit_code": 1,
        "cause": "task_file_invalid",
        "next_action": "af task plan --file zero.json",
        "details": {"file": "zero.json", "field": "limits.wall_ms", "value": 0},
    });
    assert_valid(ERROR_V2, &file);
    let goal = json!({
        "schema": "af/error@2",
        "command": "af task",
        "error": "cannot open goal file goal.md: No such file or directory (os error 2)",
        "exit_code": 1,
        "cause": "input_unreadable",
        "next_action": "af task run fix-2 --from fix-1 --goal goal.md",
        "details": {"file": "goal.md", "what": "goal file"},
    });
    assert_valid(ERROR_V2, &goal);
    let interrupted = json!({
        "schema": "af/error@2",
        "command": "af task",
        "error": "interrupted by SIGINT; its Worker processes were stopped and the Task was not finished. To continue, resume it with `af task run t --state ../state`, ...",
        "exit_code": 130,
        "cause": "interrupted",
        "next_action": "af task run t --state ../state",
        "details": {"signal": "SIGINT", "task": "t"},
    });
    assert_valid(ERROR_V2, &interrupted);
    let login = json!({
        "schema": "af/error@2",
        "command": "af provider auth",
        "error": "interrupted by SIGTERM; native login stopped. ...",
        "exit_code": 143,
        "cause": "interrupted",
        "next_action": null,
        "details": {"signal": "SIGTERM", "task": null},
    });
    assert_valid(ERROR_V2, &login);

    for (cause, details) in [
        (
            "task_not_found",
            json!({"task_id":"missing","store":"this checkout's Task Store"}),
        ),
        (
            "no_task_catalog",
            json!({"path":".af/task-catalog.toml","revision":"HEAD"}),
        ),
        ("no_authority", json!({"revision":"HEAD"})),
        ("not_repository", json!({"repo":"."})),
        (
            "provider_not_registered",
            json!({"provider":"nope","kind":"codex"}),
        ),
        (
            "unknown_review_node",
            json!({"node":"nope","pipeline":".af/pipelines/review.toml"}),
        ),
        (
            "plan_mismatch",
            json!({"task_id":"t","confirmed":"sha256:old","current":"sha256:new"}),
        ),
        ("dirty_source", json!({"repo":"."})),
        (
            "review_selectors_missing",
            json!({"repo":".","selector":"--policy-rev"}),
        ),
        (
            "review_binding_missing",
            json!({"missing":["correctness"],"candidates":{"correctness":["codex-main"]}}),
        ),
    ] {
        let document = json!({"schema":"af/error@2","command":"af review plan",
            "error":"refused; choose the next action","exit_code":1,"cause":cause,
            "next_action":null,"details":details});
        assert_valid(ERROR_V2, &document);
        let mut invalid = document.clone();
        invalid["details"]["unexpected"] = json!(true);
        assert_invalid(ERROR_V2, &invalid, "unexpected detail");
        let mut wrong_exit = document;
        wrong_exit["exit_code"] = json!(2);
        assert_invalid(ERROR_V2, &wrong_exit, "a first-run refusal is exit 1");
    }

    // ADR-0259: `af review plan`'s binding refusal carries the plan it computed, and names
    // `af provider detect` when a Worker's kind has no registered Provider.
    let unbound = json!({
        "schema": "af/error@2",
        "command": "af review",
        "error": "model Workers architecture, correctness need explicit Provider bindings: no codex Provider is registered for architecture and correctness. ...",
        "exit_code": 1,
        "cause": "review_binding_missing",
        "next_action": "af provider detect",
        "details": {
            "missing": ["architecture", "correctness"],
            "candidates": {"architecture": [], "correctness": []},
            "plan": {
                "schema": "af/review-plan@1",
                "token_free": true,
                "pipeline": {"gates": [{"name": "check", "required": true}]},
                "providers": [{"node": "architecture", "runner": "codex", "required": true,
                    "binding": null, "ready": false}],
                "next_action": "af provider detect",
            },
        },
    });
    assert_valid(ERROR_V2, &unbound);
    for (pointer, replacement, why) in [
        (
            "/details/plan/schema",
            json!("af/review-plan@2"),
            "a plan of another version",
        ),
        (
            "/details/plan",
            json!("plan"),
            "a plan that is not a document",
        ),
    ] {
        let mut changed = unbound.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert_invalid(ERROR_V2, &changed, why);
    }
    let mut unversioned = unbound.clone();
    unversioned["details"]["plan"]
        .as_object_mut()
        .unwrap()
        .remove("schema");
    assert_invalid(ERROR_V2, &unversioned, "a plan without its schema");
    let mut elsewhere = unbound.clone();
    elsewhere["cause"] = json!("review_selectors_missing");
    elsewhere["details"] = json!({"repo": ".", "selector": "--policy-rev",
        "plan": unbound["details"]["plan"].clone()});
    assert_invalid(ERROR_V2, &elsewhere, "a plan on another cause");

    for (document, pointer, replacement, why) in [
        (
            &plain,
            "/schema",
            json!("af/error@1"),
            "the version 2 replaced",
        ),
        (
            &interrupted,
            "/schema",
            json!("af/error@1"),
            "an interruption in version 1",
        ),
        (
            &interrupted,
            "/exit_code",
            json!(1),
            "an interruption that exits as a failure",
        ),
        (
            &interrupted,
            "/details/signal",
            json!("SIGHUP"),
            "a signal af does not take",
        ),
        (
            &interrupted,
            "/next_action",
            json!("af task run t\n--state s"),
            "a next command that is not one line",
        ),
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
        assert_invalid(ERROR_V2, &changed, why);
    }
    for (document, field) in [
        (&interrupted, "details"),
        (&interrupted, "next_action"),
        (&exhausted, "details"),
        (&exhausted, "next_action"),
        (&usage, "next_action"),
    ] {
        let mut missing = document.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert_invalid(ERROR_V2, &missing, field);
    }
    let mut stray = exhausted.clone();
    stray["details"]["file"] = json!("ticket.json");
    assert_invalid(ERROR_V2, &stray, "a fact of another cause");
    let mut stray = interrupted.clone();
    stray["details"]["node"] = json!("root.nodes.implement");
    assert_invalid(
        ERROR_V2,
        &stray,
        "an interruption with another cause's fact",
    );
    let mut orphan = plain.clone();
    orphan["next_action"] = json!("af task list");
    assert_invalid(ERROR_V2, &orphan, "a next command without its cause");
    let mut extra = plain;
    extra["hint"] = json!("try again");
    assert_invalid(ERROR_V2, &extra, "a field the document does not have");
}
