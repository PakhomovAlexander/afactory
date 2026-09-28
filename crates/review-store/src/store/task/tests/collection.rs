//! ADR-0128: collection is one versioned transition — a tombstone, then a sweep of every object
//! no uncollected record reaches — and a collected Task's projection stops at the tombstone.

use super::*;
use crate::store::task::collection::CollectionDisposition;
use review_core::task::input_bindings::{
    ReferencedTaskV1, TaskInputBindingV1, TaskInputBindingsV1,
};

/// A revision of the fixture's Task under another ID, with `extra` provenance.
fn revision_for(f: &Fixture, task_id: &str, extra: Vec<String>) -> String {
    let mut revision = f.revision.clone();
    revision.task_id = task_id.into();
    revision.provenance.input_artifact_ids.extend(extra);
    f.cas
        .put_artifact(
            task::TASK_REVISION_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&revision).unwrap(),
        )
        .unwrap()
        .0
}

/// Open, finish and release one Task: its revision, result and a result-only blob are its own;
/// the fixture's requirements and policy are shared with every other Task.
fn finished(f: &mut Fixture, task_id: &str, extra: Vec<String>) -> (String, String) {
    let revision_id = revision_for(f, task_id, extra);
    let lease = f
        .store
        .open_task(&f.cas, &revision_id, "writer-1", 1_000_000)
        .unwrap();
    let own = f
        .cas
        .put(format!("{task_id} evidence only it reaches").as_bytes())
        .unwrap();
    let result = TaskResultV1 {
        task_revision_id: revision_id.clone(),
        execution: task::TaskExecutionV1::Exhausted,
        acceptance: TaskAcceptanceV1::Unsatisfied,
        domain_conclusion: "changes_requested".into(),
        outputs: BTreeMap::new(),
        evidence: BTreeSet::new(),
        missing_obligations: BTreeSet::from(["checked".into()]),
    };
    let result_id = f
        .cas
        .put_artifact(
            task::TASK_RESULT_V1,
            producer(),
            vec![revision_id.clone(), own],
            None,
            serde_json::to_value(result).unwrap(),
        )
        .unwrap()
        .0;
    f.store
        .task_change(
            &f.cas,
            &lease,
            TaskChangeV1::Finished {
                result_id: result_id.clone(),
            },
            now().unwrap(),
        )
        .unwrap();
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    // Distinct last-event times order the Tasks.
    std::thread::sleep(std::time::Duration::from_millis(5));
    (revision_id, result_id)
}

fn disposition(
    plan: &crate::store::task::collection::CollectionPlan,
    task_id: &str,
) -> CollectionDisposition {
    plan.tasks
        .iter()
        .find(|task| task.task_id == task_id)
        .unwrap()
        .disposition
        .clone()
}

fn filed_bytes(cas: &Cas) -> u64 {
    cas.filed_objects().unwrap().iter().map(|o| o.len).sum()
}

#[test]
fn keeping_one_collects_the_older_task_and_sweeps_exactly_what_the_preview_said() {
    let mut f = Fixture::new(false);
    let (older_revision, older_result) = finished(&mut f, "older", vec![]);
    let (newer_revision, newer_result) = finished(&mut f, "newer", vec![]);
    let shared = f.revision.inputs["requirements"].artifact_ids[0].clone();

    let sizes = f.store.store_inventory(&f.cas).unwrap();
    let older = sizes.tasks["older"];
    let newer = sizes.tasks["newer"];
    assert!(
        older.exclusive_bytes > 0 && older.shared_bytes > 0,
        "{older:?}"
    );
    assert_eq!(
        older.shared_objects, newer.shared_objects,
        "both share the same objects"
    );
    assert_eq!(sizes.totals.bytes, filed_bytes(&f.cas));

    let events_before = f.store.len(&task_run_id("older").unwrap()).unwrap();
    let plan = f.store.plan_task_collection(&f.cas, 0, 1).unwrap();
    assert_eq!(disposition(&plan, "older"), CollectionDisposition::Collect);
    assert_eq!(
        disposition(&plan, "newer"),
        CollectionDisposition::KeptNewest
    );
    let collected = plan.tasks.iter().find(|t| t.task_id == "older").unwrap();
    assert_eq!(collected.collected_bytes, older.exclusive_bytes);
    assert_eq!(
        plan.reclaimable_bytes,
        older.exclusive_bytes + sizes.totals.unreachable_bytes,
        "the older Task's own objects and what nothing reached already"
    );
    assert_eq!(
        f.store.len(&task_run_id("older").unwrap()).unwrap(),
        events_before,
        "planning writes nothing"
    );
    let newer_before = f.store.task_projection(&f.cas, "newer").unwrap().unwrap();

    let before = filed_bytes(&f.cas);
    let outcome = f.store.apply_task_collection(&f.cas, 0, 1, false).unwrap();
    assert_eq!(outcome.tombstoned, ["older"]);
    assert!(outcome.swept);
    assert_eq!(outcome.removed_bytes, plan.reclaimable_bytes);
    assert_eq!(before - filed_bytes(&f.cas), plan.reclaimable_bytes);

    for gone in [&older_revision, &older_result] {
        assert!(!f.cas.is_filed(gone), "{gone} was only the older Task's");
    }
    for kept in [&shared, &newer_revision, &newer_result] {
        assert!(f.cas.is_filed(kept), "{kept} is still reached");
    }
    let tombstone = f.store.task_tombstone("older").unwrap().unwrap();
    assert_eq!(tombstone.revision_id, older_revision);
    assert_eq!(tombstone.outcome, "changes_requested");
    assert_eq!(tombstone.collected_bytes, older.exclusive_bytes);
    let tail = f
        .store
        .replay(&task_run_id("older").unwrap())
        .unwrap()
        .pop()
        .unwrap();
    assert!(
        tail.artifact_refs.is_empty(),
        "the tombstone references nothing"
    );
    assert!(matches!(
        f.store.task_projection(&f.cas, "older"),
        Err(StoreError::Collected { ref task_id, .. }) if task_id == "older"
    ));
    let listed = f.store.collected_tasks().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].result_id, older_result);
    assert_eq!(f.store.task_ids(&f.cas).unwrap(), ["newer"]);
    let newer_after = f.store.task_projection(&f.cas, "newer").unwrap().unwrap();
    assert_eq!(newer_after.phase, newer_before.phase);
    assert_eq!(newer_after.next_sequence, newer_before.next_sequence);

    // A second run collects nothing and removes nothing: the sweep is a function of the log.
    let again = f.store.apply_task_collection(&f.cas, 0, 1, false).unwrap();
    assert!(again.tombstoned.is_empty());
    assert_eq!(again.removed_bytes, 0);
}

#[test]
fn a_sweep_stopped_after_the_tombstone_is_consistent_and_finished_by_the_next_run() {
    let mut f = Fixture::new(false);
    let (older_revision, _) = finished(&mut f, "older", vec![]);
    finished(&mut f, "newer", vec![]);
    let plan = f.store.plan_task_collection(&f.cas, 0, 1).unwrap();
    let before = filed_bytes(&f.cas);

    let stopped = f.store.apply_task_collection(&f.cas, 0, 1, true).unwrap();
    assert_eq!(stopped.tombstoned, ["older"]);
    assert!(!stopped.swept);
    assert_eq!(filed_bytes(&f.cas), before, "nothing swept yet");
    assert!(f.cas.is_filed(&older_revision));
    // Tombstoned with every object still present: consistent, never corrupt.
    f.store = EventStore::open(&f.path).unwrap();
    assert!(matches!(
        f.store.task_projection(&f.cas, "older"),
        Err(StoreError::Collected { .. })
    ));
    assert_eq!(f.store.task_ids(&f.cas).unwrap(), ["newer"]);
    let resumed = f.store.plan_task_collection(&f.cas, 0, 1).unwrap();
    assert_eq!(resumed.reclaimable_bytes, plan.reclaimable_bytes);

    let finished_sweep = f.store.apply_task_collection(&f.cas, 0, 1, false).unwrap();
    assert!(finished_sweep.tombstoned.is_empty(), "no second tombstone");
    assert_eq!(finished_sweep.removed_bytes, plan.reclaimable_bytes);
    assert_eq!(before - filed_bytes(&f.cas), plan.reclaimable_bytes);
    assert!(!f.cas.is_filed(&older_revision));
}

#[test]
fn a_running_task_and_an_unfinished_one_are_never_collected() {
    let mut f = Fixture::new(false);
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let waiting_revision = revision_for(&f, "waiting", vec![]);
    let waiting = f
        .store
        .open_task(&f.cas, &waiting_revision, "writer-2", 1_000_000)
        .unwrap();
    f.store.release_task_lease(&f.cas, &waiting).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    finished(&mut f, "done", vec![]);

    let plan = f.store.plan_task_collection(&f.cas, 0, 0).unwrap();
    assert_eq!(disposition(&plan, "task-1"), CollectionDisposition::Running);
    assert_eq!(
        disposition(&plan, "waiting"),
        CollectionDisposition::Unfinished
    );
    assert_eq!(disposition(&plan, "done"), CollectionDisposition::Collect);
    let outcome = f.store.apply_task_collection(&f.cas, 0, 0, false).unwrap();
    assert_eq!(outcome.tombstoned, ["done"]);
    assert_eq!(
        f.store
            .task_projection(&f.cas, "task-1")
            .unwrap()
            .unwrap()
            .phase,
        TaskPhaseV1::Running {}
    );
}

#[test]
fn a_live_writer_lease_refuses_the_store_lease_and_the_preview_says_so() {
    let mut f = Fixture::new(false);
    finished(&mut f, "done", vec![]);
    let _live = f.open();
    let plan = f.store.plan_task_collection(&f.cas, 0, 0).unwrap();
    assert_eq!(plan.live_writers.len(), 1);
    assert_eq!(plan.live_writers[0].0, "task-1");
    assert_eq!(disposition(&plan, "done"), CollectionDisposition::Collect);
    let before = filed_bytes(&f.cas);
    let error = f
        .store
        .apply_task_collection(&f.cas, 0, 0, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("live writer lease"), "{error}");
    assert!(
        f.store.task_tombstone("done").unwrap().is_none(),
        "no tombstone"
    );
    assert_eq!(filed_bytes(&f.cas), before, "nothing swept");
}

#[test]
fn a_task_another_task_binds_is_refused_by_name() {
    let mut f = Fixture::new(false);
    let (bound_revision, bound_result) = finished(&mut f, "bound", vec![]);
    let bindings = TaskInputBindingsV1 {
        schema: "af.task-input-bindings/1".into(),
        bindings: BTreeMap::from([(
            "requirements".into(),
            TaskInputBindingV1 {
                artifact_id: bound_result.clone(),
                resolved_artifact_id: None,
                snapshot_id: None,
                rerooted_snapshot_id: None,
                task: Some(ReferencedTaskV1 {
                    task_id: "bound".into(),
                    task_revision_id: bound_revision,
                    result_id: bound_result,
                    port: "document".into(),
                    acceptance: TaskAcceptanceV1::Unsatisfied,
                    domain_conclusion: "changes_requested".into(),
                }),
                also: vec![],
            },
        )]),
    };
    let record = f
        .cas
        .put_artifact(
            review_core::task::input_bindings::TASK_INPUT_BINDINGS_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&bindings).unwrap(),
        )
        .unwrap()
        .0;
    finished(&mut f, "binder", vec![record]);
    finished(&mut f, "free", vec![]);

    let plan = f.store.plan_task_collection(&f.cas, 0, 0).unwrap();
    assert_eq!(
        disposition(&plan, "bound"),
        CollectionDisposition::BoundBy {
            tasks: vec!["binder".into()]
        }
    );
    let outcome = f.store.apply_task_collection(&f.cas, 0, 0, false).unwrap();
    assert_eq!(outcome.tombstoned, ["binder", "free"]);
    assert!(f.store.task_projection(&f.cas, "bound").unwrap().is_some());
}

#[test]
fn a_campaign_record_keeps_an_object_a_collected_task_also_reached() {
    let mut f = Fixture::new(false);
    let (older_revision, older_result) = finished(&mut f, "older", vec![]);
    finished(&mut f, "newer", vec![]);
    f.store
        .append(
            "campaign-keeps",
            &f.cas,
            NewEvent::new(EventType::SourceCapturedV1, json!({}))
                .referencing(vec![older_result.clone()]),
        )
        .unwrap();
    let sizes = f.store.store_inventory(&f.cas).unwrap();
    assert!(
        sizes.tasks["older"].shared_bytes > sizes.tasks["newer"].shared_bytes,
        "the result it shares with the Campaign is shared"
    );
    let outcome = f.store.apply_task_collection(&f.cas, 0, 1, false).unwrap();
    assert_eq!(outcome.tombstoned, ["older"]);
    assert!(
        f.cas.is_filed(&older_result),
        "the Campaign record still reaches it"
    );
    assert!(
        f.cas.is_filed(&older_revision),
        "and, through the result's envelope, the revision it names"
    );
}

#[test]
fn an_append_never_commits_a_reference_to_a_removed_object() {
    let mut f = Fixture::new(false);
    let orphan = f.cas.put(b"verified, then removed by a sweep").unwrap();
    f.cas.flush().unwrap();
    f.cas.remove_unreachable(&orphan).unwrap();
    let error = f
        .store
        .append(
            "campaign-late",
            &f.cas,
            NewEvent::new(EventType::SourceCapturedV1, json!({})).referencing(vec![orphan]),
        )
        .unwrap_err();
    assert!(
        matches!(error, StoreError::DanglingArtifact { .. }),
        "{error}"
    );
}

/// A Task whose bindings were resolved against a finished Task, then collected before the new
/// Task opened, cannot open: the opening re-checks every Task its bindings name under the lock
/// collection writes with, so `gc --apply` never collects a Task another is about to bind.
#[test]
fn a_task_collected_after_its_binding_was_resolved_refuses_the_opening() {
    let mut f = Fixture::new(false);
    let (bound_revision, bound_result) = finished(&mut f, "bound", vec![]);
    let bindings = TaskInputBindingsV1 {
        schema: "af.task-input-bindings/1".into(),
        bindings: BTreeMap::from([(
            "requirements".into(),
            TaskInputBindingV1 {
                artifact_id: bound_result.clone(),
                resolved_artifact_id: None,
                snapshot_id: None,
                rerooted_snapshot_id: None,
                task: Some(ReferencedTaskV1 {
                    task_id: "bound".into(),
                    task_revision_id: bound_revision,
                    result_id: bound_result,
                    port: "document".into(),
                    acceptance: TaskAcceptanceV1::Unsatisfied,
                    domain_conclusion: "changes_requested".into(),
                }),
                also: vec![],
            },
        )]),
    };
    let record = f
        .cas
        .put_artifact(
            review_core::task::input_bindings::TASK_INPUT_BINDINGS_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&bindings).unwrap(),
        )
        .unwrap()
        .0;
    // The binder's revision exists — its bindings resolved — but it has not opened yet, so the
    // plan sees nothing binding `bound` and tombstones it. (No sweep here: in the real race the
    // binder's objects were filed after the collector's lease and survive it; in this fixture
    // they predate it and a sweep would remove them, which is a different refusal.)
    let binder = revision_for(&f, "binder", vec![record]);
    let outcome = f.store.apply_task_collection(&f.cas, 0, 0, true).unwrap();
    assert_eq!(outcome.tombstoned, ["bound"]);
    let refused = f
        .store
        .open_task(&f.cas, &binder, "writer-2", 1_000_000)
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("names Task `bound`, which was collected"),
        "{refused}"
    );
    assert!(f.store.task_projection(&f.cas, "binder").unwrap().is_none());
}
