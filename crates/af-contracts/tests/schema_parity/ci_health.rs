use super::*;
use af_contracts::ci_health::{Diagnosis, Snapshot};

fn snapshot() -> Value {
    serde_json::from_slice(
        &std::fs::read(
            workspace_root().join("fixtures/scenarios/maintenance-ci-health/snapshot.json"),
        )
        .unwrap(),
    )
    .unwrap()
}
fn diagnosis(s: &Value) -> Value {
    json!({"schema":"af.ci-diagnosis/1", "snapshot_id":format!("sha256:{}","d".repeat(64)),
        "source_artifact_ids":[format!("sha256:{}","d".repeat(64))],
        "occurrence_id":s["failures"][0]["occurrence_id"],
        "signature_id":s["failures"][0]["signature_id"], "source_sha":s["source_sha"],
        "category":"product_bug", "rationale":"Exact assertion evidence", "evidence":[{
            "snapshot_id":format!("sha256:{}","d".repeat(64)), "source_sha":s["source_sha"], "reference":"snapshot"}]})
}
#[test]
fn ci_health_current_contracts_roundtrip_and_refuse_missing_malformed_and_old_shapes() {
    let snapshot = snapshot();
    let diagnosis = diagnosis(&snapshot);
    for (name, good) in [
        ("ci-health-snapshot-v1.json", snapshot),
        ("ci-diagnosis-v1.json", diagnosis),
    ] {
        assert_valid(name, &good);
        let check = |v: Value| -> bool {
            if name.contains("snapshot") {
                serde_json::from_value::<Snapshot>(v).is_ok_and(|s| s.validate().is_ok())
            } else {
                serde_json::from_value::<Diagnosis>(v).is_ok_and(|s| s.validate().is_ok())
            }
        };
        assert!(check(good.clone()));
        for key in good.as_object().unwrap().keys() {
            let mut bad = good.clone();
            bad.as_object_mut().unwrap().remove(key);
            assert!(!validator(name).is_valid(&bad), "missing {key}");
            assert!(!check(bad), "missing {key}");
        }
        let mut bad = good.clone();
        bad["schema"] = json!("af.ci-unsupported/0");
        assert!(!validator(name).is_valid(&bad));
        assert!(!check(bad));
        let mut bad = good.clone();
        bad["extra"] = json!(true);
        assert!(!validator(name).is_valid(&bad));
        assert!(!check(bad));
    }
}
#[test]
fn ci_health_diagnosis_binds_the_selected_occurrence_revision_and_nonempty_evidence() {
    let s = snapshot();
    let d = diagnosis(&s);
    let snapshot: Snapshot = serde_json::from_value(s).unwrap();
    let diagnosis: Diagnosis = serde_json::from_value(d.clone()).unwrap();
    diagnosis
        .validate_against(&diagnosis.snapshot_id, &snapshot)
        .unwrap();
    for (key, value) in [
        ("occurrence_id", json!(format!("sha256:{}", "f".repeat(64)))),
        ("signature_id", json!(format!("sha256:{}", "f".repeat(64)))),
        ("source_sha", json!("f".repeat(40))),
        ("evidence", json!([])),
    ] {
        let mut bad = d.clone();
        bad[key] = value;
        assert!(
            serde_json::from_value::<Diagnosis>(bad)
                .unwrap()
                .validate_against(&diagnosis.snapshot_id, &snapshot)
                .is_err(),
            "{key}"
        );
    }
    let mut bad = d.clone();
    bad["evidence"][0]["reference"] = json!(format!("sha256:{}", "e".repeat(64)));
    assert!(
        serde_json::from_value::<Diagnosis>(bad)
            .unwrap()
            .validate_against(&diagnosis.snapshot_id, &snapshot)
            .is_err()
    );
    let mut unknown = snapshot;
    unknown.failures[0].normalized_fingerprint = None;
    assert!(
        diagnosis
            .validate_against(&diagnosis.snapshot_id, &unknown)
            .is_err()
    );
}

#[test]
fn ci_health_release_selection_diagnosis_and_evidence_bind_failed_head_and_refuse_main() {
    let s = snapshot();
    let mut snapshot: Snapshot = serde_json::from_value(s.clone()).unwrap();
    let release_head = snapshot.failures[0].head_sha.clone();
    snapshot.source_sha = "b".repeat(40);
    snapshot.release_pr = Some(af_contracts::ci_health::ReleasePr {
        number: 7,
        head_sha: release_head.clone(),
    });
    snapshot.failures[0].release_blocking = true;
    let diagnosis: Diagnosis = serde_json::from_value(diagnosis(&s)).unwrap();
    let selection = af_contracts::ci_health::Selection {
        schema: "af.ci-diagnosis-selection/1".into(),
        occurrence_id: diagnosis.occurrence_id.clone(),
        signature_id: diagnosis.signature_id.clone(),
        source_sha: release_head,
    };
    assert_valid(
        "ci-diagnosis-v1.json",
        &serde_json::to_value(&diagnosis).unwrap(),
    );
    assert_valid(
        "ci-diagnosis-selection-v1.json",
        &serde_json::to_value(&selection).unwrap(),
    );
    selection.validate(&snapshot).unwrap();
    diagnosis
        .validate_against(&diagnosis.snapshot_id, &snapshot)
        .unwrap();
    let mut wrong_selection = selection;
    wrong_selection.source_sha = snapshot.source_sha.clone();
    assert!(wrong_selection.validate(&snapshot).is_err());
    for change in ["diagnosis", "evidence", "both"] {
        let mut wrong = diagnosis.clone();
        if change != "evidence" {
            wrong.source_sha = snapshot.source_sha.clone();
        }
        if change != "diagnosis" {
            wrong.evidence[0].source_sha = snapshot.source_sha.clone();
        }
        assert!(
            wrong
                .validate_against(&wrong.snapshot_id, &snapshot)
                .is_err(),
            "{change}"
        );
    }
}

#[test]
fn ci_health_notification_v2_requires_actionable_failures_and_explicit_release_alerts() {
    let bundle: Value = serde_json::from_slice(
        &std::fs::read(
            workspace_root().join("fixtures/scenarios/maintenance-ci-health/scenario.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let step = bundle["definitions"]["maintenance-ci-health"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "notify-findings")
        .unwrap();
    let body: Value =
        serde_json::from_str(step["inputs"]["body"]["value"]["value"].as_str().unwrap()).unwrap();
    assert_valid("ci-health-notification-v2.json", &body);
    assert_eq!(body["failures"].as_array().unwrap().len(), 1);
    for field in ["failures", "release_alerts"] {
        let mut missing = body.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(!validator("ci-health-notification-v2.json").is_valid(&missing));
    }
    let mut old = body;
    old["schema"] = json!("af.ci-health-notification/1");
    assert!(!validator("ci-health-notification-v2.json").is_valid(&old));
}

#[test]
fn ci_health_embedded_contracts_preserve_published_constraints_without_remote_resolution_metadata()
{
    let fixture = workspace_root().join("fixtures/scenarios/maintenance-ci-health/task");
    let contract: Value =
        serde_json::from_slice(&std::fs::read(fixture.join("kind/structured.json")).unwrap())
            .unwrap();
    for (direction, port, file) in [
        ("inputs", "snapshot", "ci-health-snapshot-v1.json"),
        ("outputs", "diagnosis", "ci-diagnosis-v1.json"),
    ] {
        let mut published: Value = serde_json::from_slice(
            &std::fs::read(workspace_root().join("schemas").join(file)).unwrap(),
        )
        .unwrap();
        published.as_object_mut().unwrap().remove("$id");
        published.as_object_mut().unwrap().remove("$schema");
        assert_eq!(contract[direction][port]["schema"], published);
        if direction == "outputs" {
            let worker: Value = serde_json::from_slice(
                &std::fs::read(fixture.join("worker/outputs/diagnosis.schema.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(worker, published);
        } else {
            let worker: Value = serde_json::from_slice(
                &std::fs::read(fixture.join("worker/input.schema.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                worker["properties"][port]["items"]["properties"]["payload"], published,
                "Worker input must accept the same snapshot contract as the kind"
            );
        }
    }
}

#[test]
fn ci_health_gate_occurrence_fixtures_use_current_native_task_list_documents() {
    let value: Value = serde_json::from_slice(
        &std::fs::read(
            workspace_root().join("fixtures/scenarios/maintenance-ci-health/gate-evidence.json"),
        )
        .unwrap(),
    )
    .unwrap();
    for document in value["documents"].as_array().unwrap() {
        assert_valid("task-list-v4.json", document);
    }
}

#[test]
fn ci_health_selection_is_versioned_and_matches_the_exact_occurrence() {
    let root = workspace_root().join("fixtures/scenarios/maintenance-ci-health");
    let task: Value =
        serde_json::from_slice(&std::fs::read(root.join("task/task.json")).unwrap()).unwrap();
    let wire = task["requirements"].clone();
    assert_valid("ci-diagnosis-selection-v1.json", &wire);
    let snapshot: Snapshot = serde_json::from_value(snapshot()).unwrap();
    let selection: af_contracts::ci_health::Selection =
        serde_json::from_value(wire.clone()).unwrap();
    selection.validate(&snapshot).unwrap();
    for key in ["schema", "occurrence_id", "signature_id", "source_sha"] {
        let mut wrong = wire.clone();
        wrong[key] = if key == "schema" {
            json!("af.ci-diagnosis-selection/0")
        } else if key == "source_sha" {
            json!("f".repeat(40))
        } else {
            json!(format!("sha256:{}", "f".repeat(64)))
        };
        assert!(
            serde_json::from_value::<af_contracts::ci_health::Selection>(wrong)
                .unwrap()
                .validate(&snapshot)
                .is_err()
        );
        let mut missing = wire.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(!validator("ci-diagnosis-selection-v1.json").is_valid(&missing));
        assert!(serde_json::from_value::<af_contracts::ci_health::Selection>(missing).is_err());
    }
}
