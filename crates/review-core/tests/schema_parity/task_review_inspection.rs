use super::{assert_invalid, assert_valid};
use serde_json::json;

fn id(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

#[test]
fn review_handoff_inspection_preserves_transition_generations_and_typed_evidence() {
    let value = json!({
        "schema":"af/task-inspection@6", "task_id":"review-task", "revision_id":id('3'),
        "phase":{"kind":"running"}, "plan_id":id('4'), "chargeable_tokens":"7", "attempts":1,
        "history":[
            {"sequence":1,"transition":{"writer":"writer","epoch":1,"now_unix_ms":1,
                "change":{"kind":"resumed"}}},
            {"sequence":2,"transition":{"writer":"writer","epoch":1,"now_unix_ms":2,
                "change":{"kind":"review_continued","handoff_id":id('7')}}}
        ],
        "run_reports":[], "execution_records":[],
        "review_handoffs":[{"artifact_id":id('7'),"artifact_type":"af/TaskReviewHandoff@1","record":{
            "task_id":"review-task", "predecessor_revision_id":id('1'),
            "predecessor_plan_id":id('2'), "successor_revision_id":id('3'),
            "successor_plan_id":id('4'), "predecessor_round_id":id('5'),
            "successor_round_id":id('6'),
            "evidence":{"kind":"closed_round","report_event_id":"a".repeat(26)}
        }}]
    });
    assert_valid("task-inspection-v6.json", &value);
    for (pointer, replacement) in [
        ("/history/1/transition/change/handoff_id", json!("unbound")),
        (
            "/review_handoffs/0/artifact_type",
            json!("af/TaskReviewContinuation@1"),
        ),
        (
            "/review_handoffs/0/record/evidence/report_event_id",
            json!("invalid"),
        ),
        ("/chargeable_tokens", json!(7)),
        ("/review_handoffs", json!([])),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        assert_invalid("task-inspection-v6.json", &invalid, pointer);
    }
    let mut invalid = value.clone();
    invalid["review_handoffs"][0]["record"]["approved"] = json!(true);
    assert_invalid(
        "task-inspection-v6.json",
        &invalid,
        "data cannot invent approval",
    );
    let mut epoch = value.clone();
    epoch["review_handoffs"][0]["record"]["evidence"] = json!({
        "kind":"superseded_input", "superseded_event_id":"b".repeat(26)
    });
    assert_valid("task-inspection-v6.json", &epoch);
    for version in [3, 5] {
        let mut old = value.clone();
        old["schema"] = json!(format!("af/task-inspection@{version}"));
        assert_invalid(
            &format!("task-inspection-v{version}.json"),
            &old,
            "earlier inspection generations remain unchanged",
        );
    }
}

#[test]
fn integration_inspection_preserves_phase_reports_and_integrated_handoff_generation() {
    let mut value = json!({
        "schema":"af/task-inspection@7", "task_id":"review-task", "revision_id":id('3'),
        "phase":{"kind":"running"}, "plan_id":id('4'), "chargeable_tokens":"7", "attempts":2,
        "history":[
            {"sequence":1,"transition":{"writer":"writer","epoch":1,"now_unix_ms":1,
                "change":{"kind":"resumed"}}},
            {"sequence":2,"transition":{"writer":"writer","epoch":1,"now_unix_ms":2,
                "change":{"kind":"review_integration_selected","phase_id":id('7')}}}
        ],
        "run_reports":[], "execution_records":[],
        "review_integrations":[{
            "artifact_id":id('7'),"artifact_type":"af/TaskReviewIntegrationPhase@1",
            "record":{
                "task_id":"review-task","task_revision_id":id('1'),"plan_id":id('2'),
                "round_id":id('5'),"closing_report_event_id":"a".repeat(26),
                "selection":{"kind":"prepared","integration_plan_id":id('8'),
                    "derived_snapshot_id":id('9')}
            },
            "node":"root.integration_checks","requires_checks":true,"finished":false,
            "report_id":null,"integration_committed_event_id":null
        }]
    });
    assert_valid("task-inspection-v7.json", &value);
    for (pointer, replacement) in [
        ("/review_integrations", json!([])),
        ("/review_integrations/0/requires_checks", json!(false)),
        ("/review_integrations/0/finished", json!(true)),
        ("/review_integrations/0/report_id", json!(id('a'))),
        (
            "/review_integrations/0/integration_committed_event_id",
            json!("b".repeat(26)),
        ),
        ("/history/1/transition/change/phase_id", json!("unbound")),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        assert_invalid("task-inspection-v7.json", &invalid, pointer);
    }
    value["review_integrations"][0]["finished"] = json!(true);
    value["review_integrations"][0]["report_id"] = json!(id('a'));
    value["run_reports"] = json!([{
        "artifact_id":id('a'),"diagnostics":{},"report":{
            "task_revision_id":id('1'),"plan_id":id('2'),"through_sequence":2,
            "phase_id":id('7'),"nodes":[{"node":"root.integration_checks",
                "outcome":{"kind":"completed","output_id":id('b')}}]
        }
    }]);
    assert_valid("task-inspection-v7.json", &value);
    value["review_integrations"][0]["integration_committed_event_id"] = json!("b".repeat(26));
    value["review_handoffs"] = json!([{
        "artifact_id":id('c'),"artifact_type":"af/TaskReviewHandoff@2","record":{
            "task_id":"review-task", "predecessor_revision_id":id('1'),
            "predecessor_plan_id":id('2'), "successor_revision_id":id('3'),
            "successor_plan_id":id('4'), "predecessor_round_id":id('5'),
            "successor_round_id":id('6'), "evidence":{"kind":"integrated_round",
                "report_event_id":"a".repeat(26),"phase_id":id('7'),
                "integration_committed_event_id":"b".repeat(26)}
        }
    }]);
    assert_valid("task-inspection-v7.json", &value);
    let mut incorrect_generation = value.clone();
    incorrect_generation["review_handoffs"][0]["artifact_type"] = json!("af/TaskReviewHandoff@1");
    assert_invalid(
        "task-inspection-v7.json",
        &incorrect_generation,
        "integrated evidence retains generation two",
    );
    for version in [3, 5, 6] {
        let mut old = value.clone();
        old["schema"] = json!(format!("af/task-inspection@{version}"));
        assert_invalid(
            &format!("task-inspection-v{version}.json"),
            &old,
            "earlier inspection generations do not acquire phase semantics",
        );
    }
    let mut empty = value;
    empty.as_object_mut().unwrap().remove("review_handoffs");
    empty["run_reports"] = json!([]);
    empty["review_integrations"][0]["record"]["selection"] = json!({"kind":"empty"});
    empty["review_integrations"][0]["requires_checks"] = json!(false);
    empty["review_integrations"][0]["report_id"] = json!(null);
    empty["review_integrations"][0]["integration_committed_event_id"] = json!(null);
    assert_valid("task-inspection-v7.json", &empty);
    empty["review_integrations"][0]["finished"] = json!(false);
    assert_invalid(
        "task-inspection-v7.json",
        &empty,
        "empty selection is durably terminal without checks",
    );
}
