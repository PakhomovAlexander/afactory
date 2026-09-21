//! Fresh CLI reads of an executed Integration phase followed by a complete derived-head Review.
use super::*;
use review_store::{Cas, EventStore};

#[path = "../../../review-pipeline/tests/support/captured_review_integration.rs"]
mod captured_integration;

#[test]
fn integration_inspection_preserves_phase_and_handoff_wire_without_mutating_history() {
    let directory = captured_integration::run_integration_handoff();
    let repo = tempfile::tempdir().unwrap();
    let state = directory.path();
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let task_run = review_store::store::task::task_run_id("integration-review").unwrap();
    let before_task = store.replay(&task_run).unwrap();
    let before_review = store.replay("review").unwrap();
    let shown = json_output(
        cli(repo.path(), state, &["task", "show", "integration-review"]),
        0,
    );
    let explained = json_output(
        cli(
            repo.path(),
            state,
            &["task", "explain", "integration-review"],
        ),
        0,
    );
    let schema = validator("task-inspection-v9.json");
    for value in [&shown, &explained] {
        valid(&schema, value);
        assert_eq!(value["schema"], "af/task-inspection@9");
        assert_eq!(value["phase"]["kind"], "finished");
        let phases = value["review_integrations"].as_array().unwrap();
        assert_eq!(phases.len(), 2);
        let phase = phases
            .iter()
            .find(|phase| phase["record"]["selection"]["kind"] == "prepared")
            .unwrap();
        let empty = phases
            .iter()
            .find(|phase| phase["record"]["selection"]["kind"] == "empty")
            .unwrap();
        assert_eq!(empty["requires_checks"], false);
        assert_eq!(empty["finished"], true);
        assert!(empty["report_id"].is_null());
        assert!(empty["integration_committed_event_id"].is_null());
        assert_eq!(phase["record"]["selection"]["kind"], "prepared");
        assert_eq!(phase["requires_checks"], true);
        assert_eq!(phase["finished"], true);
        assert!(phase["integration_committed_event_id"].is_string());
        let reports = value["run_reports"].as_array().unwrap();
        let report = reports
            .iter()
            .find(|r| r["artifact_id"] == phase["report_id"])
            .unwrap();
        assert_eq!(report["report"]["phase_id"], phase["artifact_id"]);
        assert_eq!(report["report"]["nodes"].as_array().unwrap().len(), 1);
        assert!(
            reports
                .iter()
                .any(|r| r["report"].get("phase_id").is_none())
        );
        let handoffs = value["review_handoffs"].as_array().unwrap();
        assert_eq!(handoffs.len(), 1);
        let handoff = &handoffs[0];
        assert_eq!(handoff["artifact_type"], "af/TaskReviewHandoff@2");
        assert_eq!(handoff["record"]["evidence"]["kind"], "integrated_round");
        assert_eq!(
            handoff["record"]["evidence"]["phase_id"],
            phase["artifact_id"]
        );
        for row in phases.iter().chain(handoffs.iter()) {
            let original = cas
                .get_artifact(row["artifact_id"].as_str().unwrap())
                .unwrap();
            assert_eq!(row["artifact_type"], original.artifact_type);
            assert_eq!(row["record"], original.payload);
        }
        assert!(
            value["history"].as_array().unwrap().iter().any(|row| {
                row["transition"]["change"]["kind"] == "review_integration_finished"
            })
        );
    }
    for field in [
        "history",
        "review_integrations",
        "review_handoffs",
        "run_reports",
    ] {
        assert_eq!(shown[field], explained[field]);
    }
    let historical = json_output(
        cli(
            repo.path(),
            state,
            &[
                "task",
                "explain",
                "integration-review",
                "--plan",
                shown["review_handoffs"][0]["record"]["predecessor_plan_id"]
                    .as_str()
                    .unwrap(),
            ],
        ),
        0,
    );
    valid(&validator("task-plan-inspection-v1.json"), &historical);
    assert_eq!(historical["current_plan"], false);
    let listed = json_output(cli(repo.path(), state, &["task", "list"]), 0);
    assert_eq!(listed["tasks"].as_array().unwrap().len(), 1);
    valid(&validator("task-list-entry-v2.json"), &listed["tasks"][0]);
    assert_eq!(
        listed["tasks"][0]["chargeable_tokens"],
        shown["chargeable_tokens"]
    );
    assert_eq!(store.replay(&task_run).unwrap(), before_task);
    assert_eq!(store.replay("review").unwrap(), before_review);
}
