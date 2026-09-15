//! Public Task schemas checked against CLI admission and the actual serialized graph/history.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use review_core::task::delivery::{
    TASK_DELIVERY_RECORD_V1, TaskDeliveryRecordV1, TaskDeliveryStatusV1,
};
use review_core::task::execution::{
    TaskAttemptResultV1, TaskExecutionRecordV1, TaskExecutionRecordV2, TaskExecutionRecordV3,
};
use review_graph::task::CompiledTask;
use serde_json::{Value, json};

#[path = "task_public_schemas/broker.rs"]
mod broker;
#[path = "../../review-pipeline/tests/support/captured_review_continuation.rs"]
mod captured_continuation;
#[path = "../../review-pipeline/tests/support/captured_review.rs"]
mod captured_fixture;
#[path = "task_public_schemas/continuation.rs"]
mod continuation;
#[path = "task_public_schemas/integration.rs"]
mod integration;
#[path = "task_public_schemas/owned.rs"]
mod owned;
#[path = "task_public_schemas/recording.rs"]
mod recording;
#[path = "support/task_cli.rs"]
mod task_cli;

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn validator(name: &str) -> jsonschema::Validator {
    let directory = workspace().join("schemas");
    let mut options = jsonschema::options();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let Some(id) = value["$id"].as_str() else {
            continue;
        };
        options.with_resource(
            id.to_owned(),
            jsonschema::Resource::from_contents(value).unwrap(),
        );
    }
    let value: Value =
        serde_json::from_slice(&std::fs::read(directory.join(name)).unwrap()).unwrap();
    options.build(&value).unwrap()
}

fn valid(schema: &jsonschema::Validator, value: &Value) {
    let errors: Vec<_> = schema
        .iter_errors(value)
        .map(|e| format!("{} at {}", e, e.instance_path))
        .collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

fn cli(repo: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .args(["--json", "--state"])
        .arg(state)
        .output()
        .unwrap()
}

fn json_output(output: Output, status: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(status),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn refused(output: Output, label: &str) {
    let error = json_output(output, 1);
    assert_eq!(error["schema"], "af/error@1", "{label}");
    assert!(!error["error"].as_str().unwrap().is_empty(), "{label}");
}

fn file(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn catalog(path: &Path) -> Value {
    let parsed: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    serde_json::to_value(parsed).unwrap()
}

fn append_delivery(
    cas: &review_store::Cas,
    store: &mut review_store::EventStore,
    lease: &review_store::store::task::TaskLease,
    template: &TaskDeliveryRecordV1,
    receipt: &Value,
) {
    let mut record = template.clone();
    record.receipt_id = cas.put_json(receipt).unwrap();
    record.target_id = cas.put_json(&receipt["target"]).unwrap();
    let id = cas
        .put_artifact(
            TASK_DELIVERY_RECORD_V1,
            review_core::Producer::KernelOperation {
                run_id: review_store::store::task::task_run_id(&record.task_id).unwrap(),
                node_id: None,
                operation_id: "local-delivery@1".into(),
            },
            record.references().into_iter().map(str::to_owned).collect(),
            None,
            serde_json::to_value(record).unwrap(),
        )
        .unwrap()
        .0;
    store.record_task_delivery(cas, lease, &id).unwrap();
}

#[test]
fn task_file_and_catalog_schemas_match_real_fixtures_and_cli_refusals() {
    let file_schema = validator("task-file-v1.json");
    let catalog_schema = validator("task-catalog-v1.json");
    for name in ["pagination", "review", "embedded-review", "bounded-repair"] {
        let fixture = workspace().join("fixtures/task-runtime").join(name);
        valid(
            &catalog_schema,
            &catalog(&fixture.join(".af/task-catalog.toml")),
        );
        for filename in ["ticket.json", "review.json"] {
            let path = fixture.join(filename);
            if path.is_file() {
                valid(&file_schema, &file(&path));
            }
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let (repo, _) = task_cli::fixture_named(directory.path(), "pagination");
    let original = file(&repo.join("ticket.json"));
    let mut selected = original.clone();
    selected.as_object_mut().unwrap().remove("pipeline");
    selected.as_object_mut().unwrap().remove("facts");
    valid(&file_schema, &selected);
    std::fs::write(
        repo.join("selected.json"),
        serde_json::to_vec(&selected).unwrap(),
    )
    .unwrap();
    let planned = json_output(
        cli(
            &repo,
            &directory.path().join("selected"),
            &["task", "plan", "--file", "selected.json"],
        ),
        0,
    );
    assert_eq!(
        planned["selection"]["assessment"]["decision"]["kind"],
        "selected"
    );
    valid(&validator("task-inspection-v3.json"), &planned);
    let cases = [
        ("/schema", json!("af.task-file/2")),
        ("/task_id", json!("bad/task")),
        ("/goal", json!(" \n")),
        ("/pipeline", Value::Null),
        ("/facts", Value::Null),
        ("/requirements", json!({})),
        ("/limits/max_attempts", json!(4294967296_u64)),
        ("/limits/tokens", json!(9007199254740992_u64)),
    ];
    for (index, (pointer, value)) in cases.into_iter().enumerate() {
        let mut bad = original.clone();
        if pointer == "/requirements" {
            bad["requirements"] = value;
        } else {
            *bad.pointer_mut(pointer).unwrap() = value;
        }
        assert!(!file_schema.is_valid(&bad), "schema accepted {pointer}");
        std::fs::write(repo.join("invalid.json"), serde_json::to_vec(&bad).unwrap()).unwrap();
        let output = cli(
            &repo,
            &directory.path().join(format!("file-{index}")),
            &["task", "plan", "--file", "invalid.json"],
        );
        refused(output, pointer);
    }
    let mut unknown = original.clone();
    unknown["authority"] = json!({});
    assert!(!file_schema.is_valid(&unknown));
    std::fs::write(
        repo.join("invalid.json"),
        serde_json::to_vec(&unknown).unwrap(),
    )
    .unwrap();
    refused(
        cli(
            &repo,
            &directory.path().join("unknown"),
            &["task", "plan", "--file", "invalid.json"],
        ),
        "unknown Task-file property",
    );

    let path = repo.join(".af/task-catalog.toml");
    let original = catalog(&path);
    let cases = [
        ("/schema", json!("af.shared-task-catalog/1")),
        ("/code_policy", json!("../outside.toml")),
        ("/packages/fixture~1evaluator/version", json!("1.0")),
        ("/packages/fixture~1evaluator/digest", json!("sha256:no")),
        ("/packages/fixture~1evaluator/path", json!("../worker")),
        ("/independence/distinct_models", json!("true")),
    ];
    for (index, (pointer, value)) in cases.into_iter().enumerate() {
        let mut bad = original.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert!(!catalog_schema.is_valid(&bad), "schema accepted {pointer}");
        std::fs::write(&path, toml::to_string(&bad).unwrap()).unwrap();
        // Only this disposable fixture's authority commit changes; production worktrees are
        // never consulted or mutated by this admission test.
        for args in [
            vec!["add", ".af/task-catalog.toml"],
            vec!["commit", "-qm", "invalid catalog"],
        ] {
            assert!(
                Command::new("git")
                    .current_dir(&repo)
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        let output = cli(
            &repo,
            &directory.path().join(format!("catalog-{index}")),
            &["task", "plan", "--file", "ticket.json"],
        );
        refused(output, pointer);
    }
    for name in ["code_policy", "developers", "planner", "review"] {
        let mut bad = original.clone();
        bad[name] = Value::Null;
        assert!(!catalog_schema.is_valid(&bad), "null option {name}");
    }
    let mut imported = original.clone();
    imported["packages"] = json!({});
    imported["imports"] = json!([".af/imports/shared/lock.json"]);
    valid(&catalog_schema, &imported);
    imported["imports"] = json!([
        ".af/imports/shared/lock.json",
        ".af/imports/shared/lock.json"
    ]);
    valid(&catalog_schema, &imported);
    imported["imports"] = json!([]);
    assert!(!catalog_schema.is_valid(&imported));
}

#[test]
fn compiled_task_schema_matches_real_graph_and_closed_rust_variants() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "pagination");
    let inspection = json_output(
        cli(&repo, &state, &["task", "plan", "--file", "ticket.json"]),
        0,
    );
    valid(&validator("task-inspection-v3.json"), &inspection);
    let schema = validator("compiled-task-v1.json");
    let graph = &inspection["graph"];
    valid(&schema, graph);
    let compiled: CompiledTask = serde_json::from_value(graph.clone()).unwrap();
    assert_eq!(serde_json::to_value(&compiled).unwrap(), *graph);
    compiled.scheduler_plan().unwrap();
    compiled
        .budget(serde_json::from_value(inspection["plan"]["limits"].clone()).unwrap())
        .unwrap();
    let node = compiled.nodes.keys().next().unwrap();
    for operator in [
        json!({"kind":"root_inputs"}),
        json!({"kind":"select"}),
        json!({"kind":"provider_admission","bindings":["slot"]}),
        json!({"kind":"primitive","operator":{"op":"select"},"signature":"select"}),
    ]
    .into_iter()
    .chain(
        [
            "generation",
            "gate",
            "gather",
            "ledger",
            "slicer",
            "reviewer",
            "scatter",
        ]
        .into_iter()
        .map(|kind| {
            let mut operation = json!({"kind":kind});
            if matches!(kind, "reviewer" | "scatter") {
                operation["slot"] = json!("slot");
            }
            json!({"kind":"review_domain","review_node":"gate","operation":operation})
        }),
    ) {
        let mut value = graph.clone();
        value["nodes"][node]["operator"] = operator;
        valid(&schema, &value);
        serde_json::from_value::<CompiledTask>(value).unwrap();
    }
    for operator in [
        json!({"kind":"unknown"}),
        json!({"kind":"select","extra":true}),
        json!({"kind":"provider_admission","bindings":["slot","slot"]}),
        json!({"kind":"review_domain","review_node":"gate","operation":{"kind":"reviewer"}}),
    ] {
        let mut value = graph.clone();
        value["nodes"][node]["operator"] = operator;
        assert!(!schema.is_valid(&value));
        if let Ok(parsed) = serde_json::from_value::<CompiledTask>(value.clone()) {
            // Admission compares recorded.payload with the complete compiler serialization
            // (catalog.rs compile_inner), so ignored unit-variant extras or normalized sets
            // cannot survive admission even where serde itself accepts them.
            assert_ne!(serde_json::to_value(parsed).unwrap(), value);
        }
    }
    for (key, bad_value) in [
        ("order", Value::Null),
        ("max_parallel", json!(4294967296_u64)),
        ("allowances", json!([])),
        ("unexpected", json!(true)),
    ] {
        let mut value = graph.clone();
        value[key] = bad_value;
        assert!(!schema.is_valid(&value), "{key}");
        assert!(
            serde_json::from_value::<CompiledTask>(value).is_err(),
            "{key}"
        );
    }
}

#[test]
fn inspection_and_list_schemas_preserve_actual_output_and_frozen_accounting_versions() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "pagination");
    let inspection_schema = validator("task-inspection-v3.json");
    let list_schema = validator("task-list-entry-v2.json");
    let planned = json_output(
        cli(&repo, &state, &["task", "plan", "--file", "ticket.json"]),
        0,
    );
    valid(&inspection_schema, &planned);
    let exact = json_output(
        cli(
            &repo,
            &state,
            &[
                "task",
                "explain",
                "pagination-cli",
                "--plan",
                planned["plan_id"].as_str().unwrap(),
            ],
        ),
        0,
    );
    let plan_schema = validator("task-plan-inspection-v1.json");
    valid(&plan_schema, &exact);
    assert_eq!(exact["current_plan"], true);
    assert_eq!(exact["plan"], planned["plan"]);
    assert_eq!(exact["graph"], planned["graph"]);
    assert_eq!(exact["task_revision_id"], planned["revision_id"]);
    let mut invalid = exact.clone();
    invalid["recorded_event_ids"] = json!([]);
    assert!(!plan_schema.is_valid(&invalid));
    let mut invalid = exact;
    invalid["approved"] = json!(true);
    assert!(!plan_schema.is_valid(&invalid));
    let listed = json_output(cli(&repo, &state, &["task", "list"]), 0);
    valid(&list_schema, &listed["tasks"][0]);
    assert!(listed["tasks"][0]["outcome"].is_null());
    let finished = json_output(
        cli(
            &repo,
            &state,
            &["task", "run", "--execute", "pagination-cli"],
        ),
        0,
    );
    valid(&inspection_schema, &finished);
    let listed = json_output(cli(&repo, &state, &["task", "list"]), 0);
    let entry = &listed["tasks"][0];
    valid(&list_schema, entry);
    assert!(finished["execution_records"].as_array().unwrap().len() > 2);
    assert!(!finished["run_reports"].as_array().unwrap().is_empty());
    assert_eq!(
        json_output(cli(&repo, &state, &["task", "show", "pagination-cli"]), 0),
        finished
    );

    let digest = format!("sha256:{}", "a".repeat(64));
    let record = TaskExecutionRecordV1::Settled {
        attempt_id: "A".repeat(26),
        charged_tokens: 7,
        result: TaskAttemptResultV1::Failed {
            diagnostic_id: digest.clone(),
            feedback_id: None,
        },
        raw_artifact_ids: vec![],
        usage_id: None,
    };
    record.validate().unwrap();
    let mut wide = record.clone();
    if let TaskExecutionRecordV1::Settled { charged_tokens, .. } = &mut wide {
        *charged_tokens = u128::MAX;
    }
    let v2 = TaskExecutionRecordV2::from_accounting(&record).unwrap();
    let v3 = TaskExecutionRecordV3::from_accounting(&wide).unwrap();
    v2.validate().unwrap();
    v3.validate().unwrap();
    for (version, record) in [
        (1, serde_json::to_value(record).unwrap()),
        (2, serde_json::to_value(v2).unwrap()),
        (3, serde_json::to_value(v3).unwrap()),
    ] {
        let mut value = finished.clone();
        value["execution_records"] = json!([{"artifact_id":digest,"artifact_type":format!("af/TaskExecutionRecord@{version}"),"record":record,"diagnostic":["historical",{"opaque":true}]}]);
        value["chargeable_tokens"] = json!(u128::MAX.to_string());
        valid(&inspection_schema, &value);
        value["execution_records"][0]["artifact_type"] = json!(if version == 1 {
            "af/TaskExecutionRecord@2"
        } else {
            "af/TaskExecutionRecord@1"
        });
        assert!(
            !inspection_schema.is_valid(&value),
            "mismatched version {version}"
        );
    }
    for charge in [
        json!(7),
        json!("01"),
        json!("-1"),
        json!("340282366920938463463374607431768211456"),
    ] {
        let mut value = finished.clone();
        value["chargeable_tokens"] = charge.clone();
        assert!(!inspection_schema.is_valid(&value));
        let mut value = entry.clone();
        value["chargeable_tokens"] = charge;
        assert!(!list_schema.is_valid(&value));
    }
    for (key, bad) in [
        ("unexpected", json!(true)),
        ("history", Value::Null),
        ("execution_records", json!([{}])),
        ("result", json!({})),
    ] {
        let mut value = finished.clone();
        value[key] = bad;
        assert!(!inspection_schema.is_valid(&value), "{key}");
    }
    let mut value = planned.clone();
    value.as_object_mut().unwrap().remove("plan");
    assert!(!inspection_schema.is_valid(&value));
    let mut value = finished.clone();
    value.as_object_mut().unwrap().remove("result");
    assert!(!inspection_schema.is_valid(&value));
    let mut value = planned.clone();
    value["result"] = finished["result"].clone();
    assert!(!inspection_schema.is_valid(&value));
    let mut value = entry.clone();
    value.as_object_mut().unwrap().remove("delivery");
    assert!(!list_schema.is_valid(&value));
    let mut value = entry.clone();
    value["phase"] = json!({"kind":"submitted"});
    assert!(!list_schema.is_valid(&value));

    let before_delivery = directory.path().join("before-delivery");
    task_cli::copy_tree(&state, &before_delivery);
    let worktree = directory.path().join("delivered");
    let receipt = json_output(
        cli(
            &repo,
            &state,
            &[
                "task",
                "deliver",
                "pagination-cli",
                "--branch",
                "task/schema-delivery",
                "--worktree",
                worktree.to_str().unwrap(),
                "--confirm",
                "pagination-cli",
            ],
        ),
        0,
    );
    let delivered = json_output(cli(&repo, &state, &["task", "show", "pagination-cli"]), 0);
    assert_eq!(delivered["delivery"], receipt);
    valid(&inspection_schema, &delivered);
    let listed = json_output(cli(&repo, &state, &["task", "list"]), 0);
    valid(&list_schema, &listed["tasks"][0]);

    // Pending delivery is also public inspection output. Use the actual preparation receipt
    // retained by this same CLI delivery, rather than inventing a second copy of its shape.
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let store = review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let projection = store
        .task_projection(&cas, "pagination-cli")
        .unwrap()
        .unwrap();
    let prepared = projection
        .deliveries
        .iter()
        .find(|(_, record)| record.status == TaskDeliveryStatusV1::Prepared)
        .unwrap();
    let mut value = delivered.clone();
    value["delivery"] = cas.get_json(&prepared.1.receipt_id).unwrap();
    valid(&inspection_schema, &value);
    value["delivery"]["target"]
        .as_object_mut()
        .unwrap()
        .remove("branch");
    assert!(!inspection_schema.is_valid(&value));
    let mut value = delivered.clone();
    value["delivery"]["unexpected"] = json!(true);
    assert!(!inspection_schema.is_valid(&value));

    let preparation = cas.get_json(&prepared.1.receipt_id).unwrap();
    let terminal = projection
        .deliveries
        .iter()
        .find(|(_, record)| record.status == TaskDeliveryStatusV1::Delivered)
        .unwrap();
    for (index, case) in [
        "missing_revision",
        "extra_target",
        "null_branch",
        "extra_preparation",
        "extra_receipt",
        "extra_outcome",
        "invalid_ignored_paths",
        "historical_ignored_paths",
    ]
    .into_iter()
    .enumerate()
    {
        let is_prepared = index < 4;
        let historical = case == "historical_ignored_paths";
        let mut raw = if is_prepared {
            preparation.clone()
        } else {
            receipt.clone()
        };
        match case {
            "missing_revision" => {
                raw.as_object_mut().unwrap().remove("source_revision");
            }
            "extra_target" => raw["target"]["unexpected"] = json!(true),
            "null_branch" => raw["target"]["branch"] = Value::Null,
            "extra_preparation" | "extra_receipt" => raw["unexpected"] = json!(true),
            "extra_outcome" => raw["outcome"]["unexpected"] = json!(true),
            "invalid_ignored_paths" => raw["ignored_paths"] = json!([7]),
            "historical_ignored_paths" => {
                raw.as_object_mut().unwrap().remove("ignored_paths");
            }
            _ => unreachable!(),
        }
        let mut view = delivered.clone();
        view["delivery"] = raw.clone();
        assert_eq!(inspection_schema.is_valid(&view), historical, "{case}");
        let isolated = directory.path().join(format!("delivery-{index}"));
        task_cli::copy_tree(&before_delivery, &isolated);
        let cas = review_store::Cas::open_existing(isolated.join("cas")).unwrap();
        let mut store = review_store::EventStore::open(isolated.join("events.sqlite")).unwrap();
        let lease = store
            .take_task_lease(&cas, "pagination-cli", "schema-test", 15000)
            .unwrap();
        if !is_prepared {
            append_delivery(&cas, &mut store, &lease, &prepared.1, &preparation);
        }
        append_delivery(
            &cas,
            &mut store,
            &lease,
            if is_prepared {
                &prepared.1
            } else {
                &terminal.1
            },
            &raw,
        );
        store.release_task_lease(&cas, &lease).unwrap();
        drop(store);
        for args in [vec!["task", "show", "pagination-cli"], vec!["task", "list"]] {
            let output = cli(&repo, &isolated, &args);
            if historical {
                let shown = json_output(output, 0);
                let retained = if args[1] == "show" {
                    &shown["delivery"]
                } else {
                    &shown["tasks"][0]["delivery"]
                };
                assert_eq!(retained, &raw, "Historical receipt was rewritten");
                assert!(retained.get("ignored_paths").is_none());
            } else {
                refused(output, case);
            }
        }
    }
}
