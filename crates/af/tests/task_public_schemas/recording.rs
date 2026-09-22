//! Fresh CLI inspection of an actual selected-output recovery after its original deadline.
use super::captured_continuation as captured_review;
use super::*;
use review_store::{Cas, EventStore};
#[path = "../../../review-pipeline/tests/support/captured_review_recovery.rs"]
mod recovery;

#[test]
fn expired_recording_inspection_keeps_one_task_and_exact_old_and_new_history() {
    let (directory, task_id) = recovery::run_expired_waiting(|cas, store, limits| {
        let definition = captured_review::PIPELINE.replace(
            "runner = { program = \"/bin/true\" }",
            r#"runner = { program = "/bin/sh", args = [{value="-c"},{value="cat >/dev/null; printf '%s' '{\"verdict\":\"approve\",\"summary\":null,\"findings\":[],\"benchmark_demands\":[],\"dispositions\":[]}'"}] }"#,
        );
        captured_review::admit_heavy_definition_with_limits(cas, store, &definition, limits)
    });
    let repo = tempfile::tempdir().unwrap();
    let state = directory.path();
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let run = review_store::store::task::task_run_id(&task_id).unwrap();
    let before_task = store.replay(&run).unwrap();
    let before_review = store.replay("review").unwrap();
    let database = std::fs::read(state.join("events.sqlite")).unwrap();
    let original = store.task_projection(&cas, &task_id).unwrap().unwrap();
    let shown = json_output(cli(repo.path(), state, &["task", "show", &task_id]), 0);
    let explained = json_output(cli(repo.path(), state, &["task", "explain", &task_id]), 4);
    for value in [&shown, &explained] {
        valid(&validator("task-inspection-v8.json"), value);
        assert_eq!(value["schema"], "af/task-inspection@8");
        assert!(
            value.get("review_integrations").is_none(),
            "ordinary Review recovery must not invent Integration"
        );
        assert_eq!(value["phase"]["kind"], "finished");
        assert_eq!(value["result"]["acceptance"], "inconclusive");
        assert_eq!(value["attempts"], 1);
        assert_eq!(value["chargeable_tokens"], "0");
        let history = value["history"].as_array().unwrap();
        let recovered = history
            .iter()
            .filter(|row| row["transition"]["change"]["kind"] == "recording_resumed")
            .collect::<Vec<_>>();
        assert_eq!(recovered.len(), 1);
        let sequence = recovered[0]["sequence"].as_u64().unwrap();
        assert_eq!(
            recovered[0]["transition"],
            before_task
                .iter()
                .find(|e| e.sequence == sequence)
                .unwrap()
                .payload
        );
        for version in 3..=7 {
            let mut old = value.clone();
            old["schema"] = json!(format!("af/task-inspection@{version}"));
            assert!(!validator(&format!("task-inspection-v{version}.json")).is_valid(&old));
        }
    }
    assert_eq!(shown["history"], explained["history"]);
    assert_eq!(shown["execution_records"], explained["execution_records"]);
    assert_eq!(
        explained["plan"]["limits"],
        serde_json::to_value(&original.revision.limits).unwrap()
    );
    assert_eq!(store.replay(&run).unwrap(), before_task);
    assert_eq!(store.replay("review").unwrap(), before_review);
    assert_eq!(
        std::fs::read(state.join("events.sqlite")).unwrap(),
        database
    );
}
