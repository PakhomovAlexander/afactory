//! The one Task state every rendering prints (`schemas/task-state-v1.json`) and the documents
//! that carry it: `af/task-list@3` with its `af/task-list-entry@3`, `af/task-checks@2`,
//! `af/task-collected-inspection@2` and the `state` of `af/task-inspection@12`. The Rust
//! validator and the schema accept exactly the same states, and the previous versions fail
//! closed.
use super::{assert_invalid, assert_valid, validator};
use review_core::task::state::*;
use serde_json::{Value, json};

fn id(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

/// A `$defs` member of the state schema, as a standalone validator.
fn state_valid(state: &Value) -> bool {
    static VALIDATOR: std::sync::OnceLock<jsonschema::Validator> = std::sync::OnceLock::new();
    VALIDATOR.get_or_init(state_validator).is_valid(state)
}

fn state_validator() -> jsonschema::Validator {
    let wrapper = json!({"$ref": "urn:af:schema:task-state:1#/$defs/state"});
    let mut registry = jsonschema::Registry::new();
    for name in ["task-state-v1.json", "task-contracts-v1.json"] {
        let value = super::schema(name);
        let id = String::from(value["$id"].as_str().unwrap());
        registry = registry
            .add(id, jsonschema::Resource::from_contents(value))
            .unwrap();
    }
    let registry = registry.prepare().unwrap();
    jsonschema::options()
        .with_registry(&registry)
        .build(&wrapper)
        .unwrap()
}

#[test]
fn the_schema_and_the_rust_validator_accept_the_same_states() {
    let outcomes = std::iter::once(None).chain(TaskOutcomeV1::ALL.map(Some));
    let mut accepted = 0;
    for phase in TaskStatePhaseV1::ALL {
        for outcome in outcomes.clone() {
            for cause in std::iter::once(None).chain(TaskCauseV1::ALL.map(Some)) {
                for next in [None, Some("af task run t")] {
                    let state = TaskStateV1 {
                        phase,
                        outcome,
                        cause,
                        next_action: next.map(str::to_owned),
                    };
                    let value = serde_json::to_value(&state).unwrap();
                    let rust = state.validate().is_ok();
                    // Collection keeps no next command; the Rust rule allows one only to an af
                    // command, which the schema's pattern states too.
                    assert_eq!(
                        state_valid(&value),
                        rust,
                        "the schema and the Rust rule disagree on {value}"
                    );
                    accepted += usize::from(rust);
                }
            }
        }
    }
    assert!(accepted > 30, "{accepted}");
    for (value, why) in [
        (
            json!({"phase":"finished","outcome":"gate_failed","cause":"check_failed","next_action":"rm -rf /"}),
            "a next command that is not af's",
        ),
        (
            json!({"phase":"finished","outcome":"incomplete","cause":null,"next_action":null}),
            "the outcome every failure used to share",
        ),
        (
            json!({"phase":"running","outcome":null,"cause":null}),
            "a state without its next command field",
        ),
        (
            json!({"phase":"planned","outcome":null,"cause":null,"next_action":null,"extra":1}),
            "an undeclared field",
        ),
    ] {
        assert!(!state_valid(&value), "{why}");
        assert!(
            serde_json::from_value::<TaskStateV1>(value.clone())
                .map_err(|e| e.to_string())
                .and_then(|state| state.validate())
                .is_err(),
            "{why}"
        );
    }
}

fn entry() -> Value {
    json!({
        "schema": "af/task-list-entry@3",
        "task_id": "pagination-cli",
        "kind": "implement",
        "phase": {"kind": "finished", "result_id": id('1')},
        "state": {"phase":"finished","outcome":"gate_failed","cause":"check_failed",
            "next_action":"af task run pagination-cli --retry-gate"},
        "gate": {"status":"failed","failed_tests":["alpha::one","beta::two"],"flaky":[]},
        "round": 0,
        "chargeable_tokens": "0",
        "derived_snapshot_id": id('2'),
        "delivery": null,
        "updated_unix_ms": 1_000,
    })
}

#[test]
fn a_list_document_carries_each_task_s_state_and_gate() {
    let document = json!({"schema": "af/task-list@3", "tasks": [entry()]});
    assert_valid("task-list-v3.json", &document);
    let mut planned = entry();
    planned["phase"] = json!({"kind": "ready"});
    planned["state"] = json!({"phase":"planned","outcome":null,"cause":null,
        "next_action": format!("af task run pagination-cli --confirm-plan {}", id('3'))});
    planned["derived_snapshot_id"] = Value::Null;
    planned.as_object_mut().unwrap().remove("gate");
    assert_valid("task-list-entry-v3.json", &planned);
    for (edit, why) in [
        (
            Box::new(|v: &mut Value| v["schema"] = json!("af/task-list-entry@2"))
                as Box<dyn Fn(&mut Value)>,
            "the previous entry version",
        ),
        (
            Box::new(|v: &mut Value| v["outcome"] = json!("changes_requested")),
            "the version 2 outcome string",
        ),
        (
            Box::new(|v: &mut Value| {
                v.as_object_mut().unwrap().remove("state");
            }),
            "an entry without its state",
        ),
        (
            Box::new(|v: &mut Value| v["gate"]["status"] = json!("flaky")),
            "a gate status outside the three",
        ),
        (
            Box::new(|v: &mut Value| v["gate"]["failed_tests"] = json!(["a", "a"])),
            "a failing test named twice",
        ),
        (
            Box::new(|v: &mut Value| v["state"]["phase"] = json!("running")),
            "a finished phase whose state is not finished",
        ),
    ] {
        let mut changed = entry();
        edit(&mut changed);
        assert_invalid("task-list-entry-v3.json", &changed, why);
    }
    let mut old = document.clone();
    old["schema"] = json!("af/task-list@2");
    assert_invalid("task-list-v3.json", &old, "the previous list version");
}

#[test]
fn the_checks_document_names_each_check_s_failing_tests_and_stderr_tail() {
    let document = json!({
        "schema": "af/task-checks@2",
        "task_id": "pagination-cli",
        "state": {"phase":"finished","outcome":"gate_failed","cause":"check_failed",
            "next_action":"af task run pagination-cli --retry-gate"},
        "gate": {"status":"failed","failed_tests":["alpha::one"],"flaky":["beta::two"]},
        "receipt": {"node":"root.nodes.check","receipt_id":id('4'),"outcome":"failed","checks":[{
            "name":"pagination","status":"failed","exit_code":1,"elapsed_ms":47,
            "cache":"cold (no warm cache)","remote":false,
            "stopped_step":{"step":"test","exit_code":100},
            "failed_tests":["alpha::one"],"flaky":["beta::two"],
            "stdout_id":id('5'),"stderr_id":id('6'),
            "stderr_tail":["        FAIL [   0.004s] af::it alpha::one"]}]}
    });
    assert_valid("task-checks-v2.json", &document);
    // Under a warm layer each started check carries its `af/TaskCheckLayer@1` entry.
    let mut layered = document.clone();
    layered["receipt"]["checks"][0]["layer"] = json!({
        "key": format!("{}/{}", "a".repeat(64), "b".repeat(64)),
        "lock": "waited", "waited_ms": 61000,
        "holder": {"pid": 4242, "host": "claw-3", "since_unix_ms": 1},
        "start": "warm", "sources": {"stale": 2, "fresh": 15},
        "phases": {"build_wall_ms": 2400000, "check_wall_ms": 3600000, "build_ms": 812000,
            "tests_ms": 1530000, "elapsed_ms": 2342000},
        "evicted": {"phase": "build", "by": "storage_sweep", "at_unix_ms": 5}
    });
    assert_valid("task-checks-v2.json", &layered);
    layered["receipt"]["checks"][0]["layer"]["lock"] = json!("held");
    assert_invalid(
        "task-checks-v2.json",
        &layered,
        "a holder of a layer held at once",
    );
    let mut old = document.clone();
    old["schema"] = json!("af/task-checks@1");
    assert_invalid("task-checks-v2.json", &old, "the previous checks version");
    let mut empty = document.clone();
    empty["receipt"] = Value::Null;
    empty["gate"] = Value::Null;
    assert_valid("task-checks-v2.json", &empty);
    let mut long = document.clone();
    long["receipt"]["checks"][0]["stderr_tail"] = json!(vec!["x"; 41]);
    assert_invalid("task-checks-v2.json", &long, "more than 40 stderr lines");
    let mut step = document;
    step["receipt"]["checks"][0]["stopped_step"]["step"] = json!("a b");
    assert_invalid("task-checks-v2.json", &step, "a step label with a space");
}

#[test]
fn a_collected_inspection_states_the_collection_as_its_cause() {
    let document = json!({
        "schema": "af/task-collected-inspection@2",
        "task_id": "pagination-cli",
        "phase": {"kind": "finished", "result_id": id('1')},
        "state": {"phase":"finished","outcome":"changes_requested","cause":"collected","next_action":null},
        "collected": {
            "schema": "af/TaskCollected@1", "task_id": "pagination-cli", "kind": "implement",
            "revision_id": id('2'), "outcome": "changes_requested", "chargeable_tokens": "0",
            "last_event_unix_ms": 1, "collected_unix_ms": 2, "collected_bytes": 3
        }
    });
    let validator = validator("task-collected-inspection-v2.json");
    let errors: Vec<String> = validator
        .iter_errors(&document)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let mut uncaused = document.clone();
    uncaused["state"]["cause"] = json!("budget");
    uncaused["state"]["outcome"] = json!("exhausted");
    uncaused["state"]["next_action"] = json!("af task show pagination-cli");
    assert!(
        !validator.is_valid(&uncaused),
        "a collected Task names its collection"
    );
    let mut old = document;
    old["schema"] = json!("af/task-collected-inspection@1");
    assert!(
        !validator.is_valid(&old),
        "the previous version fails closed"
    );
}

#[test]
fn an_inspection_carries_the_state_its_failures_and_its_gate() {
    let value = json!({
        "schema":"af/task-inspection@12", "task_id":"pagination-cli", "revision_id":id('1'),
        "phase":{"kind":"running"},
        "state":{"phase":"abandoned","outcome":"cancelled","cause":"host_cancelled",
            "next_action":"af task run pagination-cli"},
        "clock":{"wall_ms":60000,"started_unix_ms":5,"deadline_unix_ms":60005},
        "plan_id":id('2'), "chargeable_tokens":"0", "attempts":1,
        "gate":{"status":"passed","failed_tests":[],"flaky":[]},
        "failures":[{"node":"root.nodes.implement","cause":"malformed_reply",
            "first_line":"Invalid typed Worker reply","diagnostic_id":id('3')},
            {"node":"root.nodes.evaluate","cause":null,
            "first_line":"Required branch receipt was not produced: root.nodes.check did not publish one",
            "diagnostic_id":id('4')}],
        "history":[], "execution_records":[], "run_reports":[]
    });
    assert_valid("task-inspection-v12.json", &value);
    for (pointer, replacement, why) in [
        (
            "/schema",
            json!("af/task-inspection@11"),
            "the previous version",
        ),
        (
            "/state/outcome",
            json!("verified"),
            "an abandoned Task that verified",
        ),
        ("/failures", json!([]), "an empty failure list"),
        (
            "/failures/0/cause",
            json!("crashed"),
            "a cause outside the vocabulary",
        ),
        (
            "/gate/status",
            json!("ok"),
            "a gate status outside the three",
        ),
        (
            "/clock/started_unix_ms",
            Value::Null,
            "a deadline for a clock that never started",
        ),
    ] {
        let mut changed = value.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert_invalid("task-inspection-v12.json", &changed, why);
    }
    let mut stateless = value;
    stateless.as_object_mut().unwrap().remove("state");
    assert_invalid(
        "task-inspection-v12.json",
        &stateless,
        "an inspection without its state",
    );
}

/// `af/task-selection@2`: a refused plan's document carries the planner's fit, the Rust
/// `TaskLimitsFitV1` serialized as the diagnostic's terms, and nothing else.
#[test]
fn a_refused_selection_names_the_smallest_limits_that_fit() {
    use review_core::task::VerificationReserveV1;
    use review_core::task::limits_fit::*;
    let limits = |tokens, attempts, wall_ms, reserve: (u64, u32, u64)| TaskFitLimitsV1 {
        tokens,
        max_attempts: attempts,
        wall_ms,
        verification: VerificationReserveV1 {
            tokens: reserve.0,
            attempts: reserve.1,
            wall_ms: reserve.2,
        },
    };
    let fit = TaskLimitsFitV1 {
        supplied: limits(100, 1, 1_000, (0, 0, 0)),
        required: limits(400, 3, 15_000, (200, 2, 10_000)),
        reserves: vec![TaskVerificationSlotReserveV1 {
            node: "root.nodes.evaluate".into(),
            attempts: 2,
            tokens_per_attempt: 100,
            wall_ms_per_attempt: 5_000,
        }],
        mandatory: TaskMandatoryWorkV1 {
            tokens: 200,
            attempts: 1,
            wall_ms: 5_000,
        },
        unfixable: vec![],
    };
    fit.validate().unwrap();
    assert_eq!(fit.raised().len(), 6);
    let document = json!({
        "schema": "af/task-selection@2",
        "task_id": "tight",
        "selection_id": id('1'),
        "request_revision_id": id('2'),
        "attempts": 0,
        "chargeable_tokens": 0,
        "selection": {
            "schema": "af.pipeline-selection/1",
            "requested": {"name": "fixture/implementation", "fallback": "refuse"},
            "no_match": "refuse",
            "candidates": [{"pipeline": "fixture/implementation", "preferred": true, "priority": 0,
                "state": {"kind": "infeasible", "reason": "Task cannot retain verification reserves"}}],
            "decision": {"kind": "infeasible"}
        },
        "planning_diagnostic": {
            "kind": "infeasible",
            "pipeline": "fixture/implementation",
            "reason": "Task cannot retain verification reserves",
            "supplied_limits": fit.supplied,
            "required_limits": fit.required,
            "raised": fit.raised().into_iter().map(|(limit, supplied, required)| {
                json!({"limit": limit, "supplied": supplied, "required": required})
            }).collect::<Vec<_>>(),
            "reserves": fit.reserves,
            "mandatory": fit.mandatory,
            "unfixable": fit.unfixable,
        }
    });
    assert_valid("task-selection-v2.json", &document);
    for (pointer, replacement, why) in [
        (
            "/schema",
            json!("af/task-selection@1"),
            "the previous version",
        ),
        ("/attempts", json!(1), "a refusal that ran an Attempt"),
        (
            "/planning_diagnostic/required_limits/tokens",
            json!(-1),
            "a negative limit",
        ),
        (
            "/planning_diagnostic/raised/0/limit",
            json!("budget"),
            "a limit no Task file names",
        ),
        (
            "/planning_diagnostic/reserves/0/attempts",
            json!(0),
            "a reserve of no Attempt",
        ),
    ] {
        let mut changed = document.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert_invalid("task-selection-v2.json", &changed, why);
    }
    let mut shrunk = fit.clone();
    shrunk.required.tokens = 50;
    assert!(
        shrunk.validate().is_err(),
        "a fit never lowers a supplied limit"
    );
    let mut extra = document;
    extra["planning_diagnostic"]["estimate_ms"] = json!(1);
    assert_invalid(
        "task-selection-v2.json",
        &extra,
        "an estimate the planner never makes",
    );
}
