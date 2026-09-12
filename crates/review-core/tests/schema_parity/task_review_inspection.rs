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
    for version in [3, 4, 5] {
        let mut old = value.clone();
        old["schema"] = json!(format!("af/task-inspection@{version}"));
        assert_invalid(
            &format!("task-inspection-v{version}.json"),
            &old,
            "earlier inspection generations remain unchanged",
        );
    }
}
