use super::*;
use af_runner::task::{WorkerContract, WorkerRequest};
use af_source_git::{
    Entry,
    task::{capture_snapshot, source_tree},
};
use serde_json::json;

struct Fixture {
    directory: tempfile::TempDir,
    cas: Cas,
    input: TaskInvocationV1,
    policy: KnowledgePolicyV1,
    nodes: BTreeSet<String>,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let input = source(&cas, "source fact", "dependency", "unrelated");
        Self {
            directory,
            cas,
            input,
            policy: policy(),
            nodes: BTreeSet::from(["root.discover".into(), "root.read".into()]),
        }
    }
    fn select(&self, input: &TaskInvocationV1) -> (TaskInvocationV1, KnowledgeContextV1) {
        let mut input = input.clone();
        select(
            &self.cas,
            &mut input,
            &self.policy,
            Scope {
                run: "task-one",
                slot: "author",
                nodes: &self.nodes,
            },
        )
        .unwrap();
        let view = read(&self.cas, &input);
        (input, view)
    }
    fn query(
        &self,
        view: &KnowledgeContextV1,
        entries: BTreeSet<String>,
        run: &str,
        node: &str,
    ) -> ArtifactInputV1 {
        let query = KnowledgeQueryV1 {
            schema: QUERY_TYPE.into(),
            index_id: view.index_id.clone(),
            entries,
        };
        let id = self
            .cas
            .put_artifact(
                QUERY_TYPE,
                Producer::Attempt {
                    run_id: run.into(),
                    node_id: node.into(),
                    attempt_id: "a".repeat(26),
                },
                vec![view.index_id.clone()],
                None,
                serde_json::to_value(query).unwrap(),
            )
            .unwrap()
            .0;
        ArtifactInputV1 {
            artifact_ids: vec![id],
            artifact_type: QUERY_TYPE.into(),
            cardinality: PortCardinality::One,
            snapshot_id: None,
        }
    }
    fn notes(&self, input: &mut TaskInvocationV1, run: &str, node: &str) {
        let head = self.input.inputs["source"].snapshot_id.clone().unwrap();
        let notes = WorkerNotesV1::bind(json!({"model_of_change":"PRIVATE_HYPOTHESIS","inspected":[{"path":"fact.txt","tree_entry_digest":format!("sha256:{}","0".repeat(64))}]}),node,&"b".repeat(26),&head).unwrap();
        let id = self
            .cas
            .put_artifact(
                af_contracts::task::WORKER_NOTES_V1,
                Producer::Attempt {
                    run_id: run.into(),
                    node_id: node.into(),
                    attempt_id: "b".repeat(26),
                },
                vec![head],
                None,
                serde_json::to_value(notes).unwrap(),
            )
            .unwrap()
            .0;
        input.inputs.insert(
            "notes".into(),
            ArtifactInputV1 {
                artifact_ids: vec![id],
                artifact_type: af_contracts::task::WORKER_NOTES_V1.into(),
                cardinality: PortCardinality::One,
                snapshot_id: None,
            },
        );
    }
}
fn policy() -> KnowledgePolicyV1 {
    KnowledgePolicyV1 {
        schema: POLICY_SCHEMA.into(),
        sources: BTreeMap::from([(
            "fact".into(),
            KnowledgeSourceV1 {
                path: "${source}/fact.txt".into(),
                offset: 0,
                bytes: 64,
                dependencies: BTreeSet::from(["dependency.txt".into()]),
            },
        )]),
        limits: KnowledgeLimitsV1 {
            manifest_bytes: 16 * 1024,
            context_bytes: 32 * 1024,
            retrieval_bytes: 4096,
            retrieval_items: 4,
            source_bytes: 4096,
            notes_bytes: 4096,
        },
        private_notes: PrivateNotesV1::SameSlot,
        cross_task: CrossTaskKnowledgeV1::Disabled,
        retention: KnowledgeRetentionV1::Task,
        freshness: KnowledgeFreshnessPolicyV1::Mark,
    }
}
fn source(cas: &Cas, fact: &str, dependency: &str, unrelated: &str) -> TaskInvocationV1 {
    let manifest = Manifest::new(
        [
            ("fact.txt", fact),
            ("dependency.txt", dependency),
            ("unrelated.txt", unrelated),
        ]
        .into_iter()
        .map(|(path, bytes)| Entry {
            path: path.into(),
            kind: EntryKind::File,
            content: cas.put(bytes.as_bytes()).unwrap(),
            size: bytes.len() as u64,
        })
        .collect(),
    )
    .unwrap();
    let origin = cas
        .put_json(&json!({"fixture":"knowledge source"}))
        .unwrap();
    let snapshot = capture_snapshot(cas, &manifest, &origin, None).unwrap();
    let input = source_tree(
        cas,
        Producer::KernelOperation {
            run_id: "fixture".into(),
            node_id: None,
            operation_id: "source@1".into(),
        },
        &snapshot,
        vec![],
    )
    .unwrap();
    TaskInvocationV1 {
        plan_id: origin,
        node: "root.discover".into(),
        inputs: BTreeMap::from([("source".into(), input)]),
    }
}
fn read(cas: &Cas, input: &TaskInvocationV1) -> KnowledgeContextV1 {
    serde_json::from_value(
        cas.get_artifact(&input.inputs[KNOWLEDGE_PORT].artifact_ids[0])
            .unwrap()
            .payload,
    )
    .unwrap()
}
fn contract(cas: &Cas) -> WorkerContract {
    WorkerContract::capture(
        cas,
        json!({"type":"object"}),
        BTreeMap::from([("answer".into(), json!({"type":"object"}))]),
    )
    .unwrap()
}

#[test]
fn manifest_then_on_demand_exact_context_and_rebuild_after_restart() {
    let f = Fixture::new();
    let (_, manifest) = f.select(&f.input);
    assert!(manifest.excerpts.is_empty());
    let entry = manifest.manifest.entries["fact"].clone();
    assert_eq!(entry.kind, KnowledgeKindV1::Fact);
    assert_eq!(entry.visibility, KnowledgeVisibilityV1::ProjectSource);
    let mut next = f.input.clone();
    next.node = "root.read".into();
    next.inputs.insert(
        QUERY_PORT.into(),
        f.query(
            &manifest,
            BTreeSet::from([entry.entry_id]),
            "task-one",
            "root.discover",
        ),
    );
    let (selected, context) = f.select(&next);
    assert_eq!(context.excerpts[0].text, "source fact");
    assert_eq!(
        f.cas.get(&context.excerpts[0].entry.content_id).unwrap(),
        b"source fact"
    );
    let contract = contract(&f.cas);
    let id = contract
        .prepare(&f.cas, &selected, &[], "Read declared data.")
        .unwrap();
    let (record, bytes) = contract.read_context(&f.cas, &id).unwrap();
    let request: WorkerRequest = serde_json::from_slice(&bytes).unwrap();
    assert!(!request.inputs.contains_key(QUERY_PORT));
    assert!(
        record
            .manifest
            .entries
            .iter()
            .any(|e| e.name == "knowledge-excerpt:fact"
                && e.rendered_bytes == 11
                && e.artifact_id.as_deref() == Some(&context.excerpts[0].entry.content_id))
    );
    // A rebuilt disposable index has exactly the same content-addressed selection.
    let reopened = Cas::open(f.directory.path().join("cas")).unwrap();
    let mut rebuilt = next;
    select(
        &reopened,
        &mut rebuilt,
        &f.policy,
        Scope {
            run: "task-one",
            slot: "author",
            nodes: &f.nodes,
        },
    )
    .unwrap();
    assert_eq!(rebuilt, selected);
    assert_eq!(
        contract
            .prepare(&reopened, &rebuilt, &[], "Read declared data.")
            .unwrap(),
        id
    );
    assert_eq!(contract.read_context(&reopened, &id).unwrap().1, bytes);
    // A recorded selection is not searched again to hide an evicted excerpt. Exact context
    // composition refuses; the already rendered request remains immutable evidence.
    let content = &context.excerpts[0].entry.content_id;
    let object = f
        .directory
        .path()
        .join("cas/objects")
        .join(&content[7..9])
        .join(&content[9..]);
    std::fs::remove_file(object).unwrap();
    assert!(
        contract
            .prepare(&reopened, &rebuilt, &[], "Read declared data.")
            .is_err()
    );
    assert_eq!(contract.read_context(&reopened, &id).unwrap().1, bytes);
}

#[test]
fn filenames_are_not_freshness_proof_but_unrelated_changes_keep_facts() {
    let f = Fixture::new();
    let (_, view) = f.select(&f.input);
    let entry_id = view.manifest.entries["fact"].entry_id.clone();
    let query = f.query(
        &view,
        BTreeSet::from([entry_id.clone()]),
        "task-one",
        "root.discover",
    );
    for (fact, dep, other, expected) in [
        ("new fact", "dependency", "unrelated", false),
        ("source fact", "changed dependency", "unrelated", false),
        ("source fact", "dependency", "changed unrelated", true),
    ] {
        let mut input = source(&f.cas, fact, dep, other);
        input.inputs.insert(QUERY_PORT.into(), query.clone());
        let (_, selected) = f.select(&input);
        assert_eq!(!selected.excerpts.is_empty(), expected);
        if !expected {
            assert_eq!(selected.omissions[&entry_id], Miss::Stale);
        }
    }
}

#[test]
fn notes_are_same_slot_hypotheses_with_host_resolved_dependencies_and_head_delta() {
    let mut f = Fixture::new();
    let mut input = f.input.clone();
    f.notes(&mut input, "task-one", "root.discover");
    let (_, manifest) = f.select(&input);
    let note = &manifest.manifest.entries["private_notes"];
    assert_eq!(note.kind, KnowledgeKindV1::Hypothesis);
    assert_eq!(
        note.freshness,
        KnowledgeFreshnessV1::Current,
        "Worker-claimed inspected digest is not authority"
    );
    assert!(manifest.warm_set_id.is_some());
    let query = f.query(
        &manifest,
        BTreeSet::from([note.entry_id.clone()]),
        "task-one",
        "root.discover",
    );
    input.inputs.insert(QUERY_PORT.into(), query.clone());
    let (selected, view) = f.select(&input);
    assert_eq!(view.excerpts.len(), 1);
    assert!(view.excerpts[0].text.contains("PRIVATE_HYPOTHESIS"));
    assert!(view.excerpts[0].text.contains("head_delta"));
    let contract = contract(&f.cas);
    let context_id = contract
        .prepare(&f.cas, &selected, &[], "Inspect.")
        .unwrap();
    let (_, bytes) = contract.read_context(&f.cas, &context_id).unwrap();
    let request: WorkerRequest = serde_json::from_slice(&bytes).unwrap();
    assert!(!request.inputs.contains_key("notes"));
    // A changed dependency marks the notes for revalidation, never as a source fact.
    let mut changed = source(&f.cas, "changed fact", "dependency", "unrelated");
    changed
        .inputs
        .insert("notes".into(), input.inputs["notes"].clone());
    changed.inputs.insert(QUERY_PORT.into(), query);
    let (_, view) = f.select(&changed);
    assert_eq!(
        view.manifest.entries["private_notes"].freshness,
        KnowledgeFreshnessV1::Revalidate
    );
    assert_eq!(view.excerpts.len(), 1);
    assert_eq!(
        view.excerpts[0].entry.freshness,
        KnowledgeFreshnessV1::Revalidate
    );
    let private: PrivateKnowledgeV1 = serde_json::from_str(&view.excerpts[0].text).unwrap();
    assert_eq!(
        private.head_delta.to_snapshot_id,
        changed.inputs["source"].snapshot_id.clone().unwrap()
    );
    f.policy.freshness = KnowledgeFreshnessPolicyV1::OmitStale;
    let (_, view) = f.select(&changed);
    assert!(!view.manifest.entries.contains_key("private_notes"));
    assert!(view.excerpts.is_empty());
    assert_eq!(view.omissions[&note.entry_id], Miss::Stale);
    assert_eq!(view.manifest.omissions["private_notes"], Miss::Stale);
    // No query means no private prose, even though the inspection map is an invocation input.
    let (selected, _) = f.select(&input_without_query(&input));
    let id = contract
        .prepare(&f.cas, &selected, &[], "Inspect.")
        .unwrap();
    assert!(
        !String::from_utf8(contract.read_context(&f.cas, &id).unwrap().1)
            .unwrap()
            .contains("PRIVATE_HYPOTHESIS")
    );
}
fn input_without_query(input: &TaskInvocationV1) -> TaskInvocationV1 {
    let mut i = input.clone();
    i.inputs.remove(QUERY_PORT);
    i
}

#[test]
fn private_notes_query_survives_unrelated_source_change_and_delivers_current_delta() {
    use af_contracts::{Arg, Command};
    use af_runner::task::{WorkerAccess, invoke_command};

    let f = Fixture::new();
    let mut initial = f.input.clone();
    f.notes(&mut initial, "task-one", "root.discover");
    let (_, manifest) = f.select(&initial);
    let note = &manifest.manifest.entries["private_notes"];
    let query = f.query(
        &manifest,
        BTreeSet::from([note.entry_id.clone()]),
        "task-one",
        "root.discover",
    );
    let mut next = source(&f.cas, "source fact", "dependency", "changed unrelated");
    next.node = "root.read".into();
    let head = next.inputs["source"].snapshot_id.clone().unwrap();
    next.inputs
        .insert("notes".into(), initial.inputs["notes"].clone());
    next.inputs.insert(QUERY_PORT.into(), query);
    let (selected, view) = f.select(&next);
    assert_eq!(view.excerpts.len(), 1, "{:?}", view.omissions);
    let excerpt = &view.excerpts[0];
    assert_eq!(excerpt.entry.entry_id, note.entry_id);
    assert_ne!(excerpt.entry.content_id, note.content_id);
    assert_eq!(excerpt.entry.freshness, KnowledgeFreshnessV1::Current);
    assert!(view.omissions.is_empty());
    let private: PrivateKnowledgeV1 = serde_json::from_str(&excerpt.text).unwrap();
    assert_eq!(private.head_delta.to_snapshot_id, head);
    assert_eq!(private.head_delta.from_snapshot_id, note.source_snapshot_id);
    assert_eq!(private.head_delta.node, "root.discover");
    assert_eq!(private.head_delta.changed_paths, vec!["unrelated.txt"]);
    let warm: af_contracts::WarmSetV3 = serde_json::from_value(
        f.cas
            .get_artifact(view.warm_set_id.as_ref().unwrap())
            .unwrap()
            .payload,
    )
    .unwrap();
    assert_eq!(
        f.cas
            .get_artifact(warm.head_delta_artifact_id.as_ref().unwrap())
            .unwrap()
            .payload,
        serde_json::to_value(&private.head_delta).unwrap()
    );

    let contract = contract(&f.cas);
    let context_id = contract
        .prepare(&f.cas, &selected, &[], "Inspect.")
        .unwrap();
    let (record, _) = contract.read_context(&f.cas, &context_id).unwrap();
    assert!(record.manifest.entries.iter().any(|entry| {
        entry.name == "knowledge-excerpt:private_notes"
            && entry.artifact_id.as_ref() == Some(&excerpt.entry.content_id)
            && entry.rendered_bytes == excerpt.text.len() as u64
    }));
    let harness = f.directory.path().join("read-notes.py");
    std::fs::write(
        &harness,
        r#"import json,sys
request=json.load(sys.stdin)
assert 'notes' not in request['inputs'] and 'knowledge_query' not in request['inputs']
view=request['inputs']['knowledge'][0]['payload']
assert len(view['excerpts']) == 1
excerpt=view['excerpts'][0]
assert excerpt['entry']['kind'] == 'hypothesis'
assert excerpt['entry']['visibility'] == 'same_slot'
assert excerpt['entry']['freshness'] == 'current'
private=json.loads(excerpt['text'])
assert private['notes']['model_of_change'] == 'PRIVATE_HYPOTHESIS'
assert private['head_delta']['to_snapshot_id'] == request['inputs']['source'][0]['snapshot_id']
assert private['head_delta']['changed_paths'] == ['unrelated.txt']
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'answer':[private]}}))
"#,
    )
    .unwrap();
    let returned = invoke_command(
        &f.cas,
        f.directory.path(),
        &f.directory.path().join("runtime"),
        &Command::new(
            "/usr/bin/python3",
            vec![Arg::literal(harness.to_str().unwrap())],
        ),
        &contract,
        &context_id,
        std::time::Duration::from_secs(30),
        None,
        &[],
        WorkerAccess::ReadOnly,
    );
    assert_eq!(
        returned.reply.unwrap().outputs["answer"][0],
        serde_json::to_value(private).unwrap()
    );

    // Rebuilding after restart resolves the same query to the same current bytes.
    let reopened = Cas::open(f.directory.path().join("cas")).unwrap();
    let mut rebuilt = next;
    select(
        &reopened,
        &mut rebuilt,
        &f.policy,
        Scope {
            run: "task-one",
            slot: "author",
            nodes: &f.nodes,
        },
    )
    .unwrap();
    assert_eq!(rebuilt, selected);
    assert_eq!(
        contract
            .prepare(&reopened, &rebuilt, &[], "Inspect.")
            .unwrap(),
        context_id
    );
}

#[test]
fn private_notes_query_cannot_select_another_producer_or_an_over_bound_new_delta() {
    let mut f = Fixture::new();
    let mut initial = f.input.clone();
    f.notes(&mut initial, "task-one", "root.discover");
    let (_, manifest) = f.select(&initial);
    let note = &manifest.manifest.entries["private_notes"];
    let query = f.query(
        &manifest,
        BTreeSet::from([note.entry_id.clone()]),
        "task-one",
        "root.discover",
    );
    let mut next = source(&f.cas, "source fact", "dependency", "changed unrelated");
    next.inputs.insert(QUERY_PORT.into(), query);
    // Identical prose and source from another admitted same-slot producer is a new entry.
    f.notes(&mut next, "task-one", "root.read");
    let (_, view) = f.select(&next);
    assert_ne!(
        view.manifest.entries["private_notes"].entry_id,
        note.entry_id
    );
    assert!(view.excerpts.is_empty());
    assert_eq!(view.omissions[&note.entry_id], Miss::Stale);

    next.inputs
        .insert("notes".into(), initial.inputs["notes"].clone());
    let (_, view) = f.select(&next);
    assert!(view.excerpts[0].entry.bytes > note.bytes);
    // The old payload fits, but its bytes must never stand in for a larger current delta.
    for notes_bound in [false, true] {
        f.policy = policy();
        if notes_bound {
            f.policy.limits.notes_bytes = note.bytes;
        } else {
            f.policy.limits.retrieval_bytes = note.bytes;
        }
        let (_, view) = f.select(&next);
        assert!(view.excerpts.is_empty());
        assert_eq!(view.manifest.omissions["private_notes"], Miss::OverBound);
        assert_eq!(view.omissions[&note.entry_id], Miss::OverBound);
    }
}

#[test]
fn independent_slots_and_cross_task_private_notes_never_enter_context() {
    let f = Fixture::new();
    for (run, node) in [
        ("task-one", "root.reviewer"),
        ("other-task", "root.discover"),
    ] {
        let mut input = f.input.clone();
        f.notes(&mut input, run, node);
        let (selected, view) = f.select(&input);
        assert_eq!(view.manifest.omissions["private_notes"], Miss::WrongLineage);
        let contract = contract(&f.cas);
        let id = contract
            .prepare(&f.cas, &selected, &[], "Independent review.")
            .unwrap();
        assert!(
            !String::from_utf8(contract.read_context(&f.cas, &id).unwrap().1)
                .unwrap()
                .contains("PRIVATE_HYPOTHESIS")
        );
    }
}

#[test]
fn cross_task_retrieval_requires_explicit_source_facts_capability_and_revalidation() {
    let mut f = Fixture::new();
    let mut with_notes = f.input.clone();
    f.notes(&mut with_notes, "other-task", "other.worker");
    let foreign_nodes = BTreeSet::from(["other.worker".into()]);
    select(
        &f.cas,
        &mut with_notes,
        &f.policy,
        Scope {
            run: "other-task",
            slot: "foreign",
            nodes: &foreign_nodes,
        },
    )
    .unwrap();
    let view = read(&f.cas, &with_notes);
    let fact = view.manifest.entries["fact"].entry_id.clone();
    let private = view.manifest.entries["private_notes"].entry_id.clone();
    // An import on another effective slot is treated like a cross-Task query. It can never
    // bring its private index entries into the receiver's context.
    let mut input = f.input.clone();
    input.inputs.insert(
        QUERY_PORT.into(),
        f.query(
            &view,
            BTreeSet::from([fact.clone()]),
            "other-task",
            "other.worker",
        ),
    );
    let (_, selected) = f.select(&input);
    assert_eq!(selected.omissions["query"], Miss::WrongLineage);
    f.policy.cross_task = CrossTaskKnowledgeV1::SourceFacts;
    assert_eq!(f.select(&input).1.excerpts[0].text, "source fact");
    input.inputs.insert(
        QUERY_PORT.into(),
        f.query(
            &view,
            BTreeSet::from([private]),
            "other-task",
            "other.worker",
        ),
    );
    assert_eq!(f.select(&input).1.omissions["query"], Miss::WrongLineage);
    let mut changed = source(&f.cas, "changed fact", "dependency", "unrelated");
    changed.inputs.insert(
        QUERY_PORT.into(),
        f.query(
            &view,
            BTreeSet::from([fact.clone()]),
            "other-task",
            "other.worker",
        ),
    );
    assert_eq!(f.select(&changed).1.omissions[&fact], Miss::Stale);
}

#[test]
fn bounds_missing_eviction_and_invalid_query_are_explicit_and_deterministic() {
    let mut f = Fixture::new();
    f.policy.sources.insert(
        "second".into(),
        KnowledgeSourceV1 {
            path: "${source}/dependency.txt".into(),
            offset: 0,
            bytes: 64,
            dependencies: BTreeSet::new(),
        },
    );
    f.policy.sources.insert(
        "missing".into(),
        KnowledgeSourceV1 {
            path: "${source}/absent.txt".into(),
            offset: 0,
            bytes: 64,
            dependencies: BTreeSet::new(),
        },
    );
    let (_, manifest) = f.select(&f.input);
    assert_eq!(manifest.manifest.omissions["missing"], Miss::Missing);
    let mut input = f.input.clone();
    input.inputs.insert(
        QUERY_PORT.into(),
        f.query(
            &manifest,
            manifest
                .manifest
                .entries
                .values()
                .map(|e| e.entry_id.clone())
                .collect(),
            "task-one",
            "root.discover",
        ),
    );
    f.policy.limits.retrieval_items = 1;
    let (_, view) = f.select(&input);
    assert_eq!(view.excerpts.len(), 1);
    assert_eq!(
        view.omissions
            .values()
            .filter(|r| **r == Miss::OverBound)
            .count(),
        1
    );
    f.policy.limits.source_bytes = 5;
    // Requested ranges must still fit the source limit; reduce their explicit ranges too.
    for spec in f.policy.sources.values_mut() {
        spec.bytes = 5;
    }
    let (_, view) = f.select(&f.input);
    assert_eq!(view.manifest.omissions["fact"], Miss::OverBound);
    // Missing old manifest is recorded as eviction, never silently rebuilt as a different query.
    let object = f
        .directory
        .path()
        .join("cas/objects")
        .join(&manifest.index_id[7..9])
        .join(&manifest.index_id[9..]);
    std::fs::remove_file(object).unwrap();
    assert_eq!(f.select(&input).1.omissions["query"], Miss::Evicted);
    let object = f
        .directory
        .path()
        .join("cas/objects")
        .join(&manifest.index_id[7..9])
        .join(&manifest.index_id[9..]);
    std::fs::write(object, b"corrupt index bytes").unwrap();
    assert_eq!(f.select(&input).1.omissions["query"], Miss::Corrupt);
}

#[test]
fn a_worker_cannot_request_an_undelivered_index_or_promote_a_hypothesis() {
    let f = Fixture::new();
    let (selected, view) = f.select(&f.input);
    let mut query = serde_json::to_value(KnowledgeQueryV1 {
        schema: QUERY_TYPE.into(),
        index_id: view.index_id.clone(),
        entries: BTreeSet::from([view.manifest.entries["fact"].entry_id.clone()]),
    })
    .unwrap();
    assert_eq!(
        query_references(&f.cas, &selected, &query).unwrap(),
        BTreeSet::from([view.index_id])
    );
    query["entries"] = json!([format!("sha256:{}", "0".repeat(64))]);
    assert!(query_references(&f.cas, &selected, &query).is_err());
    let mut entry = view.manifest.entries["fact"].clone();
    entry.kind = KnowledgeKindV1::Hypothesis;
    assert!(entry.validate().is_err());
}

#[test]
fn command_and_native_codex_adapters_receive_only_the_selected_evidenced_bytes() {
    use af_contracts::{Arg, Command};
    use af_runner::task::{WorkerAccess, invoke_command, invoke_model};
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let mut input = f.input.clone();
    f.notes(&mut input, "task-one", "root.discover");
    let (_, manifest) = f.select(&input);
    input.inputs.insert(
        QUERY_PORT.into(),
        f.query(
            &manifest,
            BTreeSet::from([manifest.manifest.entries["fact"].entry_id.clone()]),
            "task-one",
            "root.discover",
        ),
    );
    let (selected, _) = f.select(&input);
    let contract = contract(&f.cas);
    let id = contract
        .prepare(&f.cas, &selected, &[], "Use the selected fact.")
        .unwrap();
    let script = r#"import json,sys,pathlib
request=json.load(sys.stdin)
assert 'notes' not in request['inputs'] and 'knowledge_query' not in request['inputs']
assert 'PRIVATE_HYPOTHESIS' not in json.dumps(request)
view=request['inputs']['knowledge'][0]['payload']
assert len(view['excerpts']) == 1
assert view['excerpts'][0]['text'] == 'source fact'
assert view['excerpts'][0]['entry']['kind'] == 'fact'
reply={'schema':'af.worker-reply/1','outputs':{'answer':[{'text':view['excerpts'][0]['text']}]}}
if '-o' in sys.argv:
    pathlib.Path(sys.argv[sys.argv.index('-o')+1]).write_text(json.dumps(reply))
    print(json.dumps({'type':'turn.completed','usage':{'input_tokens':1,'cached_input_tokens':0,'output_tokens':1}}))
else: print(json.dumps(reply))
"#;
    let harness = f.directory.path().join("harness.py");
    std::fs::write(&harness, script).unwrap();
    let command = Command::new(
        "/usr/bin/python3",
        vec![Arg::literal(harness.to_str().unwrap())],
    );
    let returned = invoke_command(
        &f.cas,
        f.directory.path(),
        &f.directory.path().join("runtime"),
        &command,
        &contract,
        &id,
        std::time::Duration::from_secs(30),
        None,
        &[],
        WorkerAccess::ReadOnly,
    );
    assert_eq!(
        returned.reply.unwrap().outputs["answer"][0]["text"],
        "source fact"
    );
    // Use a fake executable by exact path. No installed Provider CLI, identity or token is used.
    let native = f.directory.path().join("codex-fixture");
    std::fs::write(&native, format!("#!/usr/bin/python3\n{script}")).unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = af_runner_codex::task::CodexTaskAdapter::new(&Command::new(
        native.to_str().unwrap(),
        vec![
            Arg::literal("--model"),
            Arg::literal("gpt-6.1-sol"),
            Arg::literal("-c"),
            Arg::literal("model_reasoning_effort=\"medium\""),
        ],
    ))
    .unwrap();
    let returned = invoke_model(
        &f.cas,
        f.directory.path(),
        &adapter,
        &contract,
        &id,
        std::time::Duration::from_secs(30),
        WorkerAccess::ReadOnly,
        None,
        10000,
    );
    assert_eq!(
        returned.reply.unwrap().outputs["answer"][0]["text"],
        "source fact"
    );
    assert!(!returned.raw_artifact_ids.is_empty());
}

#[test]
fn manifest_and_context_byte_overflow_record_omissions_without_unbounded_fallback() {
    let mut f = Fixture::new();
    f.policy.sources.insert(
        "second".into(),
        KnowledgeSourceV1 {
            path: "${source}/dependency.txt".into(),
            offset: 0,
            bytes: 64,
            dependencies: BTreeSet::new(),
        },
    );
    f.policy.limits.manifest_bytes = 1024;
    let (_, view) = f.select(&f.input);
    assert!(serde_json::to_vec(&view.manifest).unwrap().len() <= 1024);
    assert!(view.manifest.entries.is_empty());
    assert_eq!(view.manifest.omissions["fact"], Miss::OverBound);
    assert_eq!(view.manifest.omissions["second"], Miss::OverBound);
    f.policy.limits.manifest_bytes = 16 * 1024;
    let long = "factual data ".repeat(200);
    f.policy.sources.get_mut("fact").unwrap().bytes = long.len() as u64;
    let input = source(&f.cas, &long, "dependency", "unrelated");
    let (_, manifest) = f.select(&input);
    let mut query_input = input.clone();
    let id = manifest.manifest.entries["fact"].entry_id.clone();
    query_input.inputs.insert(
        QUERY_PORT.into(),
        f.query(
            &manifest,
            BTreeSet::from([id.clone()]),
            "task-one",
            "root.discover",
        ),
    );
    // The manifest plus metadata fit; delivery of the selected long excerpt does not.
    f.policy.limits.context_bytes = serde_json::to_vec(&manifest).unwrap().len() as u64 + 512;
    let (_, view) = f.select(&query_input);
    assert!(view.excerpts.is_empty());
    assert_eq!(view.omissions[&id], Miss::ContextBound);
    assert!(serde_json::to_vec(&view).unwrap().len() as u64 <= f.policy.limits.context_bytes);
    f.policy.limits.context_bytes = 1024;
    let mut refused = input;
    let error = select(
        &f.cas,
        &mut refused,
        &f.policy,
        Scope {
            run: "task-one",
            slot: "author",
            nodes: &f.nodes,
        },
    )
    .unwrap_err();
    assert!(error.contains("bound cannot hold"));
}

#[test]
fn private_notes_exempts_only_bound_attempt_identity_from_credential_checks() {
    let f = Fixture::new();
    let mut input = f.input.clone();
    f.notes(&mut input, "task-one", "root.discover");
    let original = f
        .cas
        .get_artifact(&input.inputs["notes"].artifact_ids[0])
        .unwrap();
    // The runtime derives 26 hexadecimal characters, unlike the repeated-letter fixtures.
    let attempt = "0123456789abcdef0123456789";
    assert!(af_telemetry::redact::looks_like_credential(attempt));
    for field in [
        None,
        Some("model"),
        Some("question"),
        Some("hint"),
        Some("path"),
        Some("digest"),
        Some("binding"),
    ] {
        let mut payload = original.payload.clone();
        payload["attempt_id"] = json!(attempt);
        match field {
            Some("model") => payload["model_of_change"] = json!(attempt),
            Some("question") => payload["open_questions"] = json!([attempt]),
            Some("hint") => payload["hints"] = json!([{"path":"fact.txt","note":attempt}]),
            Some("path") => payload["inspected"] = json!([{"path":format!("{attempt}.txt")}]),
            Some("digest") => payload["inspected"][0]["tree_entry_digest"] = json!(attempt),
            Some("binding") => payload["attempt_id"] = json!("b".repeat(26)),
            None => (),
            _ => unreachable!(),
        }
        let id = f
            .cas
            .put_artifact(
                af_contracts::task::WORKER_NOTES_V1,
                Producer::Attempt {
                    run_id: "task-one".into(),
                    node_id: "root.discover".into(),
                    attempt_id: attempt.into(),
                },
                original.input_artifacts.clone(),
                None,
                payload,
            )
            .unwrap()
            .0;
        input.inputs.get_mut("notes").unwrap().artifact_ids = vec![id];
        let (_, view) = f.select(&input);
        if let Some(field) = field {
            assert!(
                !view.manifest.entries.contains_key("private_notes"),
                "{field}"
            );
            assert_eq!(
                view.manifest.omissions["private_notes"],
                if field == "digest" || field == "binding" {
                    Miss::Corrupt
                } else {
                    Miss::CredentialShaped
                },
                "{field}"
            );
        } else {
            assert_eq!(
                view.manifest.entries["private_notes"].freshness,
                KnowledgeFreshnessV1::Current
            );
            assert_eq!(
                view.manifest.entries["private_notes"].source_artifact_id,
                input.inputs["notes"].artifact_ids[0]
            );
            let warm: af_contracts::WarmSetV3 = serde_json::from_value(
                f.cas
                    .get_artifact(view.warm_set_id.as_ref().unwrap())
                    .unwrap()
                    .payload,
            )
            .unwrap();
            assert_eq!(warm.source_attempt_id.as_deref(), Some(attempt));
            let entry = &view.manifest.entries["private_notes"];
            input.inputs.insert(
                QUERY_PORT.into(),
                f.query(
                    &view,
                    BTreeSet::from([entry.entry_id.clone()]),
                    "task-one",
                    "root.discover",
                ),
            );
            let (_, retrieved) = f.select(&input);
            assert!(retrieved.omissions.is_empty());
            let private: PrivateKnowledgeV1 =
                serde_json::from_str(&retrieved.excerpts[0].text).unwrap();
            assert_eq!(private.notes.attempt_id, attempt);
            input.inputs.remove(QUERY_PORT);
        }
    }
}
