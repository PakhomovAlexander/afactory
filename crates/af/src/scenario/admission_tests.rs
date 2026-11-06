//! Deterministic coordinator tests: no Provider CLI, clock sleeps or remote adapters.
use super::roadmap::{Admission, GoalState, Proposal, Revision, Roadmap};
use super::tests::{DAY, START, simple, valid};
use super::*;
use std::sync::{Arc, Barrier};

fn fixture() -> (Roadmap, Pin) {
    let mut p: Proposal = serde_json::from_str(include_str!(
        "../../../../fixtures/scenarios/performance-roadmap.json"
    ))
    .unwrap();
    p.horizon.start_unix_ms = START;
    p.horizon.end_unix_ms = START + 30 * DAY;
    p.max_tokens = 500;
    p.daily_tokens = 100;
    p.rolling_tokens = 150;
    p.rolling_window_ms = 2 * DAY;
    p.goals[0].loop_tokens = 60;
    let mut second = p.goals[0].clone();
    second.id = "other".into();
    second.component = "Other product component".into();
    second.priority = 2;
    p.goals.push(second);
    let roadmap = Roadmap::new(Revision::new(None, p).unwrap()).unwrap();
    let (mut bundle, mut policy) = simple();
    let template = template::Template {
        schema: "af/scenario-task-template@1".into(),
        repo: "/repo".into(),
        state: "/task-state".into(),
        authority_commit: "a".repeat(40),
        repository_id: digest(b"repo"),
        source_content_digest: digest(b"source"),
        authority_id: digest(b"authority"),
        max_tokens: 20,
        bindings: None,
        document: serde_json::json!({"schema":"af.task-file/1","task_id":"placeholder","kind":"implement","goal":"Complete the supplied optimization goal","pipeline":{"name":"fixture/implementation","fallback":"refuse"},"strategy":"small","verification":"evaluation","limits":{"tokens":20,"max_attempts":1,"wall_ms":60000,"verification":{"tokens":5,"attempts":1,"wall_ms":10000}}}),
    };
    let id = template.id().unwrap();
    policy.task_templates.insert(id.clone());
    let root = bundle.definitions.get_mut("main").unwrap();
    root.authority.task_templates.insert(id);
    root.admission = Some(Admission {
        roadmap: roadmap.id.clone(),
        identity: roadmap.identity.clone(),
        revision: roadmap.head().revision.clone(),
    });
    root.steps[0].operation = Kind::FreshTask {
        template: Box::new(template),
        bindings: BTreeMap::new(),
    };
    root.steps[0].inputs.clear();
    root.steps[0].outputs = BTreeMap::from([("result".into(), Port::Task)]);
    (roadmap, Pin::new(bundle, policy).unwrap())
}
fn run(pin: &Pin, id: &str, at: u64) -> Run {
    Run::new(
        id.into(),
        pin.clone(),
        BTreeMap::new(),
        super::tests::limits(),
        at,
        None,
    )
    .unwrap()
}
fn store(roadmap: Roadmap) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::isolated(&dir.path().join("state")).unwrap();
    store
        .update(|w| {
            w.roadmaps.insert(roadmap.id.clone(), roadmap);
            Ok(())
        })
        .unwrap();
    (dir, store)
}
fn admit(store: &Store, pin: &Pin, id: &str, at: u64) -> Run {
    store.update(|w| w.admit_run(run(pin, id, at))).unwrap()
}
fn settle_failed(store: &Store, id: &str, at: u64, charge: Option<u64>) {
    store
        .update(|w| {
            let r = w.runs.get_mut(id).unwrap();
            r.prepare(at)?;
            r.task_charges.values_mut().next().unwrap().charged = charge;
            r.updated_unix_ms = at;
            r.fail("Recorded native Task failure".into());
            Ok(())
        })
        .unwrap();
}
#[test]
fn concurrent_manual_admission_reserves_complete_loops_once_and_restart_keeps_claim() {
    let (roadmap, pin) = fixture();
    let (_dir, store) = store(roadmap);
    let barrier = Arc::new(Barrier::new(3));
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|n| {
                let store = store.clone();
                let barrier = barrier.clone();
                let pin = pin.clone();
                scope.spawn(move || {
                    barrier.wait();
                    admit(&store, &pin, &format!("run-{n}"), START)
                })
            })
            .collect();
        barrier.wait();
        assert_eq!(
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|r| r.goal_claim.is_some())
                .count(),
            1
        );
    });
    let w = store.read().unwrap();
    let r = w.runs.values().find(|r| r.goal_claim.is_some()).unwrap();
    let c = r.goal_claim.as_ref().unwrap();
    assert_eq!(
        w.roadmaps["performance"].progress[&c.goal_id].reserved_tokens,
        60
    );
    let replay = Store::isolated(&_dir.path().join("state")).unwrap();
    assert_eq!(w, replay.read().unwrap());
    assert!(store.update(|w| w.admit_run(r.clone())).is_err());
    let again = store
        .update(|w| w.roadmaps.get_mut("performance").unwrap().admit(r))
        .unwrap()
        .unwrap();
    assert_eq!(again, *c);
    valid("scenario-store-v6.json", &store.read().unwrap());
}
#[test]
fn unknown_charge_and_midnight_never_release_a_complete_loop_or_enable_a_revision_reset() {
    let (roadmap, pin) = fixture();
    let (_dir, store) = store(roadmap);
    admit(&store, &pin, "lost", START);
    settle_failed(&store, "lost", START + DAY, None);
    let r = admit(&store, &pin, "tomorrow", START + 2 * DAY);
    assert!(r.goal_claim.is_none());
    let w = store.read().unwrap();
    let roadmap = &w.roadmaps["performance"];
    assert_eq!(roadmap.progress["store-replay"].reserved_tokens, 60);
    assert_eq!(roadmap.progress["store-replay"].state, GoalState::Deferred);
    assert_eq!(w.runs["lost"].accounted_tokens().unwrap(), 60);
    let mut proposal = roadmap.head().proposal.clone();
    proposal.max_tokens = 59;
    assert!(roadmap.plan(proposal).is_err());
    let mut proposal = roadmap.head().proposal.clone();
    proposal.horizon.end_unix_ms += DAY;
    let revision = roadmap.plan(proposal.clone()).unwrap();
    store
        .update(|w| {
            w.roadmaps
                .get_mut("performance")
                .unwrap()
                .revise(proposal, &revision.revision)
        })
        .unwrap();
    assert_eq!(
        store.read().unwrap().roadmaps["performance"].progress["store-replay"].reserved_tokens,
        60
    );
}
#[test]
fn recorded_failed_usage_counts_against_daily_rolling_and_total_across_dates() {
    for bound in ["daily", "rolling", "total"] {
        let (mut roadmap, mut pin) = fixture();
        let mut p = roadmap.head().proposal.clone();
        p.daily_tokens = 500;
        p.rolling_tokens = 500;
        match bound {
            "daily" => p.daily_tokens = 70,
            "rolling" => p.rolling_tokens = 70,
            _ => p.max_tokens = 70,
        };
        roadmap = Roadmap::new(Revision::new(None, p).unwrap()).unwrap();
        let a = pin
            .bundle
            .definitions
            .get_mut("main")
            .unwrap()
            .admission
            .as_mut()
            .unwrap();
        a.identity = roadmap.identity.clone();
        a.revision = roadmap.head().revision.clone();
        pin = Pin::new(pin.bundle, pin.policy).unwrap();
        let (_dir, store) = store(roadmap);
        admit(&store, &pin, "failed", START);
        settle_failed(&store, "failed", START + 1, Some(17));
        let blocked = admit(&store, &pin, "same-day", START + 2);
        assert!(blocked.goal_claim.is_none(), "{bound}");
        let later = admit(&store, &pin, "later", START + 3 * DAY);
        assert_eq!(later.goal_claim.is_some(), bound != "total", "{bound}");
        let p = &store.read().unwrap().roadmaps["performance"].progress["store-replay"];
        assert_eq!(p.actual_tokens, 17);
        assert_eq!(p.reserved_tokens, 0);
    }
}
#[test]
fn impossible_goal_dependencies_and_conflicts_defer_without_dispatch_or_scope_change() {
    let (mut roadmap, mut pin) = fixture();
    let mut p = roadmap.head().proposal.clone();
    p.goals[0].loop_tokens = 101;
    p.goals[1].dependencies = vec!["store-replay".into()];
    roadmap = Roadmap::new(Revision::new(None, p).unwrap()).unwrap();
    let a = pin
        .bundle
        .definitions
        .get_mut("main")
        .unwrap()
        .admission
        .as_mut()
        .unwrap();
    a.identity = roadmap.identity.clone();
    a.revision = roadmap.head().revision.clone();
    pin = Pin::new(pin.bundle, pin.policy).unwrap();
    let (_dir, store) = store(roadmap);
    let r = admit(&store, &pin, "blocked", START);
    assert_eq!(r.dispatches, 0);
    assert!(r.task_charges.is_empty());
    assert!(r.goal_claim.is_none());
    let w = store.read().unwrap();
    assert!(
        w.roadmaps["performance"].progress["store-replay"]
            .reason
            .as_ref()
            .unwrap()
            .contains("per-run")
    );
    assert!(
        w.roadmaps["performance"].progress["other"]
            .reason
            .as_ref()
            .unwrap()
            .contains("Dependency")
    );
}
#[test]
fn manual_and_ant_admission_share_ledger_and_duplicate_ticks_keep_skipped_reasons() {
    let (roadmap, pin) = fixture();
    let (_dir, store) = store(roadmap);
    admit(&store, &pin, "manual", START);
    let mut ant = super::tests::ant("UTC", 0, 0);
    ant.pin.definition.scenario = pin.bundle.clone();
    ant.pin.scenario = pin;
    ant.pin = AntPin::new(ant.pin.definition, ant.pin.scenario.policy).unwrap();
    store
        .update(|w| {
            w.ants.insert("daily".into(), ant);
            Ok(())
        })
        .unwrap();
    let result = store
        .update(|w| {
            Ok(schedule::tick_all_with_roadmaps(
                &mut w.ants,
                &mut w.runs,
                &mut w.roadmaps,
                START,
            ))
        })
        .unwrap();
    assert!(result.0.is_empty());
    assert!(result.1.is_empty());
    let w = store.read().unwrap();
    assert!(matches!(
        w.ants["daily"].occurrences[0].outcome,
        OccurrenceOutcome::SkippedRoadmap { .. }
    ));
    store
        .update(|w| {
            Ok(schedule::tick_all_with_roadmaps(
                &mut w.ants,
                &mut w.runs,
                &mut w.roadmaps,
                START,
            ))
        })
        .unwrap();
    assert_eq!(w, store.read().unwrap());
}
#[test]
fn same_labels_in_distinct_stores_cannot_reuse_claims_or_roadmap_pins() {
    let (a, pin) = fixture();
    let (b, _) = fixture();
    assert_ne!(a.identity, b.identity);
    let (_one, s1) = store(a);
    let (_two, s2) = store(b);
    let first = admit(&s1, &pin, "same-label", START);
    assert!(first.goal_claim.is_some());
    assert!(
        s2.update(|w| w.admit_run(run(&pin, "same-label", START)))
            .is_err()
    );
    assert!(s2.read().unwrap().runs.is_empty());
}

#[test]
fn lost_planning_response_reuses_intent_identity_and_refuses_changed_plan_or_template_authority() {
    let (_roadmap, pin) = fixture();
    let mut r = run(&pin, "lost-response", START);
    let first = r.prepare(START).unwrap().unwrap();
    let second = r.prepare(START).unwrap().unwrap();
    assert_eq!(first.intent, second.intent);
    assert_eq!(
        template::task_id(&first.intent.id),
        template::task_id(&second.intent.id)
    );
    assert_eq!(r.task_charges.len(), 1);
    let Kind::FreshTask { template, .. } = &first.definition.operation else {
        unreachable!()
    };
    let target = TaskTarget {
        repo: template.repo.clone(),
        state: template.state.clone(),
        task_id: template::task_id(&first.intent.id),
        plan_id: digest(b"plan"),
        revision_id: digest(b"revision"),
        max_tokens: 20,
    };
    r.fresh_tasks.insert(first.intent.id.clone(), target);
    r.validate().unwrap();
    let mut changed = r.clone();
    changed
        .fresh_tasks
        .get_mut(&first.intent.id)
        .unwrap()
        .max_tokens += 1;
    assert!(changed.validate().is_err());
    let mut changed = r.clone();
    let Kind::FreshTask { template, .. } = &mut changed
        .pin
        .bundle
        .definitions
        .get_mut("main")
        .unwrap()
        .steps[0]
        .operation
    else {
        unreachable!()
    };
    template.document["goal"] = serde_json::json!("wider");
    assert!(changed.validate().is_err());
    let mut child = pin.bundle.definitions["main"].authority.clone();
    child.task_templates.insert(digest(b"other-template"));
    assert!(!child.within(&pin.bundle.definitions["main"].authority));
    let mut other = run(&pin, "same-label", START);
    let other = other.prepare(START).unwrap().unwrap();
    assert_ne!(first.intent.id, other.intent.id);
}

#[test]
fn only_accepted_native_completion_advances_dependencies_and_symmetric_conflicts_remain_blocked() {
    let (mut roadmap, mut pin) = fixture();
    let mut p = roadmap.head().proposal.clone();
    p.daily_tokens = 500;
    p.goals[1].dependencies = vec!["store-replay".into()];
    let mut conflict = p.goals[0].clone();
    conflict.id = "conflict".into();
    conflict.conflicts = vec!["store-replay".into()];
    conflict.priority = 3;
    p.goals.push(conflict);
    roadmap = Roadmap::new(Revision::new(None, p).unwrap()).unwrap();
    let a = pin
        .bundle
        .definitions
        .get_mut("main")
        .unwrap()
        .admission
        .as_mut()
        .unwrap();
    a.identity = roadmap.identity.clone();
    a.revision = roadmap.head().revision.clone();
    pin = Pin::new(pin.bundle, pin.policy).unwrap();
    let (_dir, store) = store(roadmap);
    admit(&store, &pin, "first", START);
    let before = store.read().unwrap();
    assert_eq!(
        before.roadmaps["performance"].progress["other"].state,
        GoalState::Pending
    );
    // This is the coordinator's typed native settlement boundary, exercised without a model.
    store.update(|w| {
        let r=w.runs.get_mut("first").unwrap();let d=r.prepare(START)?.unwrap();
        let target=TaskTarget {repo:"/repo".into(),state:"/task-state".into(),task_id:template::task_id(&d.intent.id),plan_id:digest(b"plan"),revision_id:digest(b"revision"),max_tokens:20};
        r.fresh_tasks.insert(d.intent.id.clone(),target.clone());
        let key=template::charge_key(&d)?;r.task_charges.get_mut(&key).unwrap().charged=Some(17);
        let result=serde_json::from_value(serde_json::json!({"task_revision_id":target.revision_id,"execution":"completed","acceptance":"satisfied","domain_conclusion":"verified","outputs":{"report":{"artifact_type":"af/Report@1","cardinality":"one","artifact_ids":[digest(b"native report")]}},"evidence":[digest(b"native acceptance evidence")],"missing_obligations":[]})).unwrap();
        let receipt=TaskReceipt {task_id:target.task_id,plan_id:target.plan_id,revision_id:target.revision_id,result_id:digest(b"native result"),charged_tokens:17,result};
        r.complete("root","review",BTreeMap::from([("result".into(),Datum::Task(receipt))]))?;r.prepare(START)?;Ok(())
    }).unwrap();
    let completed = store.read().unwrap();
    let progress = &completed.roadmaps["performance"].progress["store-replay"];
    assert_eq!(progress.state, GoalState::Completed);
    assert_eq!(progress.actual_tokens, 17);
    assert_eq!(progress.reserved_tokens, 0);
    assert_eq!(progress.task_receipts.len(), 1);
    let next = admit(&store, &pin, "dependent", START);
    assert_eq!(next.goal_claim.as_ref().unwrap().goal_id, "other");
    let refused = admit(&store, &pin, "conflict", START);
    assert!(refused.goal_claim.is_none());
    assert!(
        store.read().unwrap().roadmaps["performance"].progress["conflict"]
            .reason
            .as_ref()
            .unwrap()
            .contains("Conflicting")
    );
}

#[test]
fn native_unknown_usage_retains_known_tokens_and_the_remaining_loop_after_terminal_failure() {
    let (roadmap, pin) = fixture();
    let (_dir, store) = store(roadmap);
    admit(&store, &pin, "unknown-native", START);
    store
        .update(|w| {
            let r = w.runs.get_mut("unknown-native").unwrap();
            r.prepare(START + 1)?;
            let c = r.task_charges.values_mut().next().unwrap();
            c.charged = Some(17);
            c.unknown_usage = true;
            r.fail("Native Task failed with unreported usage".into());
            Ok(())
        })
        .unwrap();
    let world = store.read().unwrap();
    let progress = &world.roadmaps["performance"].progress["store-replay"];
    assert_eq!(progress.actual_tokens, 17);
    assert_eq!(progress.reserved_tokens, 43);
    assert_eq!(world.runs["unknown-native"].accounted_tokens().unwrap(), 60);
    assert!(
        admit(&store, &pin, "next-day", START + 3 * DAY)
            .goal_claim
            .is_none()
    );
}

#[test]
fn shipped_fresh_task_example_has_narrow_digest_grants_and_versioned_schema_parity() {
    let bundle: Bundle = serde_json::from_str(include_str!(
        "../../../../fixtures/scenarios/fresh-task-loop.json"
    ))
    .unwrap();
    let (_, mut policy) = simple();
    policy.task_templates = bundle.definitions[&bundle.root]
        .authority
        .task_templates
        .clone();
    let pin = Pin::new(bundle.clone(), policy).unwrap();
    valid("scenario-bundle-v5.json", &bundle);
    valid("scenario-pin-v5.json", &pin);
    let Kind::FreshTask { template, .. } = &bundle.definitions[&bundle.root].steps[0].operation
    else {
        unreachable!()
    };
    valid("scenario-task-template-v1.json", template);
    let mut wrong = pin.clone();
    wrong.policy.task_templates.clear();
    assert!(wrong.validate().is_err());
}

#[test]
fn ant_occurrence_admits_one_complete_goal_and_keeps_its_original_per_run_authority() {
    let (roadmap, pin) = fixture();
    let (_dir, store) = store(roadmap);
    let mut ant = super::tests::ant("UTC", 0, 0);
    ant.pin.definition.scenario = pin.bundle;
    ant.pin = AntPin::new(ant.pin.definition, pin.policy).unwrap();
    store
        .update(|w| {
            w.ants.insert("daily".into(), ant);
            Ok(())
        })
        .unwrap();
    let (started, refused) = store
        .update(|w| {
            Ok(schedule::tick_all_with_roadmaps(
                &mut w.ants,
                &mut w.runs,
                &mut w.roadmaps,
                START,
            ))
        })
        .unwrap();
    assert!(refused.is_empty());
    assert_eq!(started.len(), 1);
    let world = store.read().unwrap();
    let run = &world.runs[&started[0]];
    assert_eq!(
        run.limits,
        world.ants["daily"].pin.definition.execution.per_run
    );
    assert_eq!(run.token_allowance(), 60);
    assert_eq!(run.limits.max_tokens, 100);
    assert_eq!(
        world.roadmaps["performance"].progress["store-replay"].reserved_tokens,
        60
    );
    store
        .update(|w| {
            Ok(schedule::tick_all_with_roadmaps(
                &mut w.ants,
                &mut w.runs,
                &mut w.roadmaps,
                START,
            ))
        })
        .unwrap();
    assert_eq!(world, store.read().unwrap());
}
#[test]
fn a_new_revision_cannot_admit_a_duplicate_active_partition_under_a_new_goal_id() {
    let (roadmap, pin) = fixture();
    let (_dir, store) = store(roadmap);
    admit(&store, &pin, "active", START);
    let mut proposal = store.read().unwrap().roadmaps["performance"]
        .head()
        .proposal
        .clone();
    proposal.daily_tokens = 500;
    proposal.rolling_tokens = 500;
    let mut duplicate = proposal.goals[0].clone();
    duplicate.id = "renamed-partition".into();
    duplicate.priority = 1;
    proposal.goals.push(duplicate);
    let revision = store.read().unwrap().roadmaps["performance"]
        .plan(proposal.clone())
        .unwrap();
    store
        .update(|w| {
            w.roadmaps
                .get_mut("performance")
                .unwrap()
                .revise(proposal, &revision.revision)
        })
        .unwrap();
    let mut new_pin = pin;
    new_pin
        .bundle
        .definitions
        .get_mut("main")
        .unwrap()
        .admission
        .as_mut()
        .unwrap()
        .revision = revision.revision;
    new_pin = Pin::new(new_pin.bundle, new_pin.policy).unwrap();
    let admitted = admit(&store, &new_pin, "future", START + DAY);
    assert_eq!(admitted.goal_claim.unwrap().goal_id, "other");
    assert!(
        store.read().unwrap().roadmaps["performance"].progress["renamed-partition"]
            .reason
            .as_ref()
            .unwrap()
            .contains("duplicate partition")
    );
    let before = store.read().unwrap().runs["active"].goal_claim.clone();
    store
        .update(|w| {
            let r = w.runs.get_mut("active").unwrap();
            let first = r.prepare(START + DAY)?.unwrap();
            let again = r.prepare(START + DAY)?.unwrap();
            require(first.intent.id == again.intent.id, "Changed intent")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(before, store.read().unwrap().runs["active"].goal_claim);
}

#[test]
fn complete_loop_reservation_includes_nested_fresh_tasks_and_refuses_hidden_exact_plan_reuse() {
    let (roadmap, pin) = fixture();
    let mut bundle = pin.bundle;
    let mut child = bundle.definitions["main"].clone();
    child.admission = None;
    child.limits.max_tokens = 60;
    let root = bundle.definitions.get_mut("main").unwrap();
    root.steps[0].operation = Kind::Scenario {
        scenario: "child".into(),
    };
    root.steps[0].outputs = child.outputs.clone();
    bundle.definitions.insert("child".into(), child);
    let pin = Pin::new(bundle, pin.policy).unwrap();
    let (_dir, store) = store(roadmap);
    let admitted = admit(&store, &pin, "nested", START);
    assert!(admitted.goal_claim.is_some());
    store
        .update(|w| {
            let run = w.runs.get_mut("nested").unwrap();
            let dispatch = run.prepare(START)?.unwrap();
            require(
                dispatch.frame == "root/review",
                "Fresh child did not retain its frame",
            )?;
            Ok(())
        })
        .unwrap();
    let world = store.read().unwrap();
    assert_eq!(
        world.roadmaps["performance"].progress["store-replay"].reserved_tokens,
        60
    );
    assert_eq!(world.runs["nested"].task_charges.len(), 1);
    let mut invalid = pin.bundle;
    let child = invalid.definitions.get_mut("child").unwrap();
    let target = TaskTarget {
        repo: "/repo".into(),
        state: "/task-state".into(),
        task_id: "existing".into(),
        plan_id: digest(b"plan"),
        revision_id: digest(b"revision"),
        max_tokens: 20,
    };
    child.authority.task_plans.insert(target.plan_id.clone());
    child.steps[0].operation = Kind::Task { target };
    assert!(invalid.validate().is_err());
}
