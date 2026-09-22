use super::*;
use review_core::task::event::{
    TaskChangeV1, TaskTransitionV1, TaskTransitionV2, TaskTransitionV3, TaskTransitionV4,
    TaskTransitionV5,
};

#[test]
fn adoption_observation_has_its_own_strict_wire() {
    let normalized = TaskTransitionV1 {
        writer: "observer".into(),
        epoch: 3,
        now_unix_ms: 5678,
        change: TaskChangeV1::AdoptionObservationRecorded {
            observation_id: format!("sha256:{}", "d".repeat(64)),
        },
    };
    assert!(serde_json::to_value(&normalized).is_err());
    let wire = TaskTransitionV5::from_adoption(&normalized).unwrap();
    wire.validate().unwrap();
    let value = serde_json::to_value(&wire).unwrap();
    assert_valid("task-transition-v5.json", &value);
    assert_eq!(wire.into_transition(), normalized);
    for schema in [
        "task-transition-v1.json",
        "task-transition-v2.json",
        "task-transition-v3.json",
        "task-transition-v4.json",
    ] {
        assert_invalid(schema, &value, "adoption observation is additive authority");
    }
    review_core::event::validate_event_payload(review_core::EventType::TaskTransitionV5, &value)
        .unwrap();
    let id = |c: char| format!("sha256:{}", c.to_string().repeat(64));
    let mut inspection = json!({
        "schema":"af/task-inspection@11", "task_id":"observed-task", "revision_id":id('a'),
        "phase":{"kind":"running"}, "plan_id":id('b'), "chargeable_tokens":"0", "attempts":0,
        "history":[{"sequence":4,"transition":value}], "execution_records":[], "run_reports":[]
    });
    assert_valid("task-inspection-v11.json", &inspection);
    inspection["adoption_observations"] = json!([]);
    assert_invalid(
        "task-inspection-v11.json",
        &inspection,
        "an absent section is omitted, never empty",
    );

    let mut missing = value.clone();
    missing["change"]
        .as_object_mut()
        .unwrap()
        .remove("observation_id");
    assert_invalid(
        "task-transition-v5.json",
        &missing,
        "observation identity is mandatory",
    );
    assert!(serde_json::from_value::<TaskTransitionV5>(missing).is_err());
}

#[test]
fn recording_resume_has_its_own_strict_wire_and_leaves_old_resume_frozen() {
    let normalized = TaskTransitionV1 {
        writer: "recovery".into(),
        epoch: 2,
        now_unix_ms: 1234,
        change: TaskChangeV1::RecordingResumed {
            task_revision_id: format!("sha256:{}", "a".repeat(64)),
            plan_id: format!("sha256:{}", "b".repeat(64)),
            report_id: format!("sha256:{}", "c".repeat(64)),
        },
    };
    assert!(normalized.validate().is_err());
    assert!(serde_json::to_value(&normalized).is_err());
    assert!(TaskTransitionV2::from_continuation(&normalized).is_none());
    assert!(TaskTransitionV3::from_integration(&normalized).is_none());
    let wire = TaskTransitionV4::from_recording(&normalized).unwrap();
    wire.validate().unwrap();
    let value = serde_json::to_value(&wire).unwrap();
    assert_valid("task-transition-v4.json", &value);
    assert_eq!(wire.into_transition(), normalized);
    for schema in [
        "task-transition-v1.json",
        "task-transition-v2.json",
        "task-transition-v3.json",
    ] {
        assert_invalid(schema, &value, "recovery is additive authority");
    }
    assert!(serde_json::from_value::<TaskTransitionV1>(value.clone()).is_err());
    review_core::event::validate_event_payload(review_core::EventType::TaskTransitionV4, &value)
        .unwrap();
    let mut invalid = Vec::new();
    for field in ["task_revision_id", "plan_id", "report_id"] {
        let mut bad = value.clone();
        bad["change"][field] = json!("unbound");
        invalid.push(bad);
        let mut bad = value.clone();
        bad["change"].as_object_mut().unwrap().remove(field);
        invalid.push(bad);
    }
    let mut bad = value.clone();
    bad["epoch"] = json!(9007199254740992u64);
    invalid.push(bad);
    let mut bad = value.clone();
    bad["now_unix_ms"] = json!(0);
    invalid.push(bad);
    let mut bad = value.clone();
    bad["change"]["output_ids"] = json!([]);
    invalid.push(bad);
    let mut bad = value.clone();
    bad["deadline_unix_ms"] = json!(99999);
    invalid.push(bad);
    for bad in invalid {
        assert_invalid(
            "task-transition-v4.json",
            &bad,
            "invalid or invented recovery authority",
        );
        assert!(
            serde_json::from_value::<TaskTransitionV4>(bad)
                .and_then(|v| v.validate().map_err(serde::de::Error::custom))
                .is_err()
        );
    }
    let old = TaskTransitionV1 {
        change: TaskChangeV1::Resumed {},
        ..normalized
    };
    let old_json = serde_json::to_value(&old).unwrap();
    assert_eq!(
        old_json,
        json!({"writer":"recovery","epoch":2,"now_unix_ms":1234,"change":{"kind":"resumed"}})
    );
    assert_valid("task-transition-v1.json", &old_json);
    assert_invalid(
        "task-transition-v4.json",
        &old_json,
        "ordinary resume stays distinct",
    );
}

#[test]
fn recording_inspection_retains_optional_histories_and_exact_resource_bounds() {
    let id = |c: char| format!("sha256:{}", c.to_string().repeat(64));
    let mut value = json!({
        "schema":"af/task-inspection@11", "task_id":"review-task", "revision_id":id('a'),
        "phase":{"kind":"running"}, "plan_id":id('b'),
        "chargeable_tokens":u128::MAX.to_string(), "attempts":1,
        "history":[
            {"sequence":0,"transition":{"writer":"writer","epoch":1,"now_unix_ms":1,
                "change":{"kind":"resumed"}}},
            {"sequence":1,"transition":{"writer":"recovery","epoch":2,"now_unix_ms":2,
                "change":{"kind":"recording_resumed","task_revision_id":id('a'),
                    "plan_id":id('b'),"report_id":id('c')}}}
        ],
        "execution_records":[], "run_reports":[]
    });
    assert_valid("task-inspection-v11.json", &value);
    assert!(value.get("review_integrations").is_none());
    for (pointer, replacement) in [
        (
            "/chargeable_tokens",
            json!("340282366920938463463374607431768211456"),
        ),
        ("/chargeable_tokens", json!(1)),
        ("/attempts", json!(1e30)),
        ("/history/1/sequence", json!(9007199254740992u64)),
        ("/history/1/transition/epoch", json!(9007199254740992u64)),
        ("/history/1/transition/change/report_id", json!("unbound")),
        ("/phase", json!({"kind":"finished","result_id":id('d')})),
    ] {
        let mut bad = value.clone();
        *bad.pointer_mut(pointer).unwrap() = replacement;
        assert_invalid("task-inspection-v11.json", &bad, pointer);
    }
    for (field, extra) in [
        ("authority", json!({"approved":true})),
        ("result", json!({})),
        ("graph", json!({})),
        ("review_integrations", json!([])),
    ] {
        let mut bad = value.clone();
        bad[field] = extra;
        assert_invalid("task-inspection-v11.json", &bad, field);
    }
    let mut bad = value.clone();
    bad["history"][1]["transition"]["change"]["output_ids"] = json!([]);
    assert_invalid(
        "task-inspection-v11.json",
        &bad,
        "recovery cannot invent selected outputs",
    );
    // Recording recovery is ordinary history: no section or version depends on it.
    let mut ordinary = value.clone();
    ordinary["history"].as_array_mut().unwrap().pop();
    assert_valid("task-inspection-v11.json", &ordinary);

    value["history"].as_array_mut().unwrap().push(json!({
        "sequence":2,"transition":{"writer":"writer","epoch":2,"now_unix_ms":3,
            "change":{"kind":"review_integration_selected","phase_id":id('e')}}
    }));
    assert_invalid(
        "task-inspection-v11.json",
        &value,
        "recorded Integration retains typed phase evidence",
    );
    value["review_integrations"] = json!([{
        "artifact_id":id('e'),"artifact_type":"af/TaskReviewIntegrationPhase@1",
        "record":{"task_id":"review-task","task_revision_id":id('a'),"plan_id":id('b'),
            "round_id":id('f'),"closing_report_event_id":"a".repeat(26),
            "selection":{"kind":"empty"}},
        "node":"root.integration_checks","requires_checks":false,"finished":true,
        "report_id":null,"integration_committed_event_id":null
    }]);
    assert_valid("task-inspection-v11.json", &value);
    value["review_integrations"][0]["finished"] = json!(false);
    assert_invalid(
        "task-inspection-v11.json",
        &value,
        "the Empty Integration condition remains enforced",
    );
}
