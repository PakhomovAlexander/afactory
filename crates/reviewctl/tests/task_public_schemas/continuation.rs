//! Fresh CLI reads of a real Task that has executed two canonical Review Rounds.
use super::*;
use review_store::{Cas, EventStore};

#[test]
fn review_continuation_inspection_preserves_exact_history_and_historical_plans() {
    let directory = captured_continuation::run_numeric_rounds(true, false);
    let repo = tempfile::tempdir().unwrap();
    let state = directory.path();
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let task_run = review_store::store::task::task_run_id("heavy-review").unwrap();
    let before_task = store.replay(&task_run).unwrap();
    let before_review = store.replay("review").unwrap();
    let shown = json_output(
        cli(repo.path(), state, &["task", "show", "heavy-review"]),
        0,
    );
    let explained = json_output(
        cli(repo.path(), state, &["task", "explain", "heavy-review"]),
        0,
    );
    let schema = validator("task-inspection-v6.json");
    for value in [&shown, &explained] {
        valid(&schema, value);
        assert_eq!(value["schema"], "af/task-inspection@6");
        assert_eq!(value["chargeable_tokens"], "5");
        assert_eq!(value["review_handoffs"].as_array().unwrap().len(), 1);
        assert!(
            value["history"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| { row["transition"]["change"]["kind"] == "review_continued" })
        );
    }
    assert_eq!(shown["history"], explained["history"]);
    assert_eq!(shown["review_handoffs"], explained["review_handoffs"]);
    let handoff = &shown["review_handoffs"][0]["record"];
    let old_plan = handoff["predecessor_plan_id"].as_str().unwrap();
    let original = json_output(
        cli(
            repo.path(),
            state,
            &["task", "explain", "heavy-review", "--plan", old_plan],
        ),
        0,
    );
    valid(&validator("task-plan-inspection-v1.json"), &original);
    assert_eq!(original["current_plan"], false);
    assert_eq!(
        original["task_revision_id"],
        handoff["predecessor_revision_id"]
    );
    assert_eq!(original["plan_id"], old_plan);
    assert_ne!(original["plan_id"], shown["plan_id"]);
    let listed = json_output(cli(repo.path(), state, &["task", "list"]), 0);
    assert_eq!(listed["tasks"].as_array().unwrap().len(), 1);
    valid(&validator("task-list-entry-v2.json"), &listed["tasks"][0]);
    assert_eq!(listed["tasks"][0]["chargeable_tokens"], "5");
    assert_eq!(store.replay(&task_run).unwrap(), before_task);
    assert_eq!(store.replay("review").unwrap(), before_review);
    let projected = store
        .task_projection(&cas, "heavy-review")
        .unwrap()
        .unwrap();
    assert_eq!(projected.review_handoffs.len(), 1);
    assert_eq!(projected.plan_id.as_deref(), shown["plan_id"].as_str());
}
