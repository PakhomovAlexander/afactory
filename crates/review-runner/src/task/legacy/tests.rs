use super::*;
use review_core::task::ArtifactInputV1;
use review_source_git::Manifest;

struct Fixture {
    _dir: tempfile::TempDir,
    cas: Cas,
    input: TaskInvocationV1,
    authority: LegacyTaskContext,
}

fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "legacy-test".into(),
        node_id: None,
        operation_id: "fixture@1".into(),
    }
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .join("fixtures/task-contracts/v1");
        let revision: Value =
            serde_json::from_slice(&std::fs::read(root.join("task-revision.json")).unwrap())
                .unwrap();
        let revision_id = cas
            .put_artifact(
                review_core::task::TASK_REVISION_V1,
                producer(),
                vec![],
                None,
                revision.clone(),
            )
            .unwrap()
            .0;
        let mut plan: Value =
            serde_json::from_slice(&std::fs::read(root.join("execution-plan.json")).unwrap())
                .unwrap();
        plan["task_revision_id"] = json!(revision_id);
        let plan_id = cas
            .put_artifact(
                review_core::task::EXECUTION_PLAN_V1,
                producer(),
                vec![revision_id.clone()],
                None,
                plan,
            )
            .unwrap()
            .0;
        let mut result = Self {
            _dir: dir,
            cas,
            input: TaskInvocationV1 {
                plan_id: plan_id.clone(),
                node: "root.nodes.evaluate".into(),
                inputs: BTreeMap::new(),
            },
            authority: LegacyTaskContext {
                task_id: revision["task_id"].as_str().unwrap().into(),
                task_revision_id: revision_id,
                plan_id,
                budget_tokens: 777,
            },
        };
        result.port(
            "requirements",
            "af/Requirements@1",
            None,
            json!({"text":"Implement the tiny change"}),
        );
        result.port("source", "af/SourceTree@1", None, json!({}));
        result
    }

    fn port(&mut self, name: &str, kind: &str, snapshot: Option<String>, payload: Value) {
        let id = self
            .cas
            .put_artifact(kind, producer(), vec![], snapshot.clone(), payload)
            .unwrap()
            .0;
        let value: ArtifactInputV1 = serde_json::from_value(json!({
            "artifact_ids":[id], "artifact_type":kind, "cardinality":"one", "snapshot_id":snapshot
        }).as_object().unwrap().iter().filter(|(_,v)| !v.is_null()).map(|(k,v)|(k.clone(),v.clone())).collect::<serde_json::Map<_,_>>().into()).unwrap();
        self.input.inputs.insert(name.into(), value);
    }

    fn contract(&self, protocol: LegacyTaskProtocol, explicit: bool) -> WorkerContract {
        let contract = WorkerContract::capture(
            &self.cas,
            json!({"type":"object"}),
            BTreeMap::from([("report".into(), json!({"type":"object"}))]),
        )
        .unwrap();
        if explicit {
            contract
                .with_legacy_protocol_and_budget(&self.cas, protocol, self.authority.budget_tokens)
                .unwrap()
        } else {
            contract.with_legacy_protocol(&self.cas, protocol).unwrap()
        }
    }

    fn prepare(&self, protocol: LegacyTaskProtocol) -> Result<(TaskContext, Vec<u8>), String> {
        let contract = self.contract(protocol, true);
        let id = contract.prepare_legacy(
            &self.cas,
            &self.input,
            &[],
            "Perform the declared task.",
            self.authority.clone(),
        )?;
        contract.read_context(&self.cas, &id)
    }

    fn evaluation(&mut self, before: &Value, after: &Value) {
        let snapshot = |manifest: &Value, parent: Option<String>| {
            let manifest_id = self.cas.put_json(manifest).unwrap();
            let parsed: Manifest = serde_json::from_value(manifest.clone()).unwrap();
            self.cas.put_json(&json!({"manifest_id":manifest_id,"content_digest":parsed.content_digest(),"parent_snapshot_id":parent})).unwrap()
        };
        let parent = snapshot(before, None);
        let current = snapshot(after, Some(parent));
        self.port("source", "af/SourceTree@1", Some(current), json!({}));
        let check = self.cas.put_json(&json!({"status":"passed"})).unwrap();
        self.port(
            "checks",
            "af/TaskCheckReceipt@1",
            None,
            json!({"checks":{"acceptance":check}}),
        );
    }
}

fn wire(bytes: &[u8]) -> Value {
    let text = std::str::from_utf8(bytes).unwrap();
    serde_json::from_str(
        text.split_once("```json\n")
            .unwrap()
            .1
            .split_once("\n```")
            .unwrap()
            .0,
    )
    .unwrap()
}

#[test]
fn explicit_context_binds_task_plan_and_wire_budget_without_business_metadata() {
    let f = Fixture::new();
    let contract = f.contract(LegacyTaskProtocol::ImplementV1, true);
    let id = contract
        .prepare_legacy(&f.cas, &f.input, &[], "Implement.", f.authority.clone())
        .unwrap();
    let (context, bytes) = contract.read_context(&f.cas, &id).unwrap();
    assert_eq!(wire(&bytes)["task_id"], f.authority.task_id);
    assert_eq!(wire(&bytes)["budget"], json!({"reserved_tokens":777}));
    assert_eq!(
        f.cas
            .get_json(&f.input.inputs["requirements"].artifact_ids[0])
            .unwrap()["payload"],
        json!({"text":"Implement the tiny change"})
    );
    assert_eq!(f.cas.get_json(&id).unwrap()["type"], TASK_CONTEXT_V2);
    assert_eq!(context.legacy, Some(f.authority.clone()));
    assert!(
        contract
            .prepare(&f.cas, &f.input, &[], "Implement.")
            .is_err()
    );
    for field in ["task_id", "task_revision_id", "plan_id", "budget_tokens"] {
        let mut changed = serde_json::to_value(&f.authority).unwrap();
        changed[field] = if field == "budget_tokens" {
            json!(0)
        } else if field == "task_id" {
            json!("different-task")
        } else {
            json!(format!("sha256:{}", "a".repeat(64)))
        };
        let changed: LegacyTaskContext = serde_json::from_value(changed).unwrap();
        assert!(
            contract
                .prepare_legacy(&f.cas, &f.input, &[], "Implement.", changed)
                .is_err(),
            "{field}"
        );
    }
    // Re-enveloping cannot strip or change authority while retaining a usable context.
    let original: ArtifactEnvelope = serde_json::from_value(f.cas.get_json(&id).unwrap()).unwrap();
    for field in ["task_id", "plan_id", "budget_tokens", "legacy"] {
        let mut payload = original.payload.clone();
        if field == "legacy" {
            payload.as_object_mut().unwrap().remove("legacy");
        } else if field == "budget_tokens" {
            payload["legacy"][field] = json!(0);
        } else {
            payload["legacy"][field] = json!("wrong");
        }
        let forged = f
            .cas
            .put_artifact(
                if field == "legacy" {
                    TASK_CONTEXT_V1
                } else {
                    TASK_CONTEXT_V2
                },
                original.producer.clone(),
                original.input_artifacts.clone(),
                None,
                payload,
            )
            .unwrap()
            .0;
        assert!(contract.read_context(&f.cas, &forged).is_err(), "{field}");
    }
}

#[test]
fn old_context_serialization_and_legacy_wire_remain_readable_exactly() {
    let mut f = Fixture::new();
    f.port("requirements", "af/Requirements@1", None, json!({"text":"Implement the tiny change", "task_id":"old-task", "budget":{"reserved_tokens":19}}));
    let contract = f.contract(LegacyTaskProtocol::ImplementV1, false);
    let id = contract
        .prepare(&f.cas, &f.input, &[], "Implement.")
        .unwrap();
    let before = f.cas.get(&id).unwrap();
    let (context, bytes) = contract.read_context(&f.cas, &id).unwrap();
    assert_eq!(wire(&bytes)["task_id"], "old-task");
    assert_eq!(wire(&bytes)["budget"], json!({"reserved_tokens":19}));
    assert!(context.legacy.is_none());
    assert!(
        serde_json::to_value(&context)
            .unwrap()
            .get("legacy")
            .is_none()
    );
    assert_eq!(f.cas.get_json(&id).unwrap()["type"], TASK_CONTEXT_V1);
    assert_eq!(
        contract
            .prepare(&f.cas, &f.input, &[], "Implement.")
            .unwrap(),
        id
    );
    assert_eq!(f.cas.get(&id).unwrap(), before);
    assert!(
        f.contract(LegacyTaskProtocol::ImplementV1, true)
            .read_context(&f.cas, &id)
            .is_err()
    );
}

fn manifest(count: usize, content: &str) -> Value {
    json!({"path_encoding":"percent_v2", "entries":(0..count).map(|i|json!({
        "path":format!("src/file-{i:06}.txt"),"kind":"file","content":content,"size":1
    })).collect::<Vec<_>>()})
}

#[test]
fn large_host_manifest_with_tiny_mutation_keeps_delivered_context_small() {
    let mut f = Fixture::new();
    // Deliberately absent content blobs: deriving a path diff must not rehash every file.
    let digest = format!("sha256:{}", "a".repeat(64));
    let before = manifest(9000, &digest);
    assert!(serde_json::to_vec(&before).unwrap().len() > MAX_WORKER_BYTES);
    let mut after = before.clone();
    after["entries"][4321]["content"] = json!(format!("sha256:{}", "b".repeat(64)));
    f.evaluation(&before, &after);
    let (context, bytes) = f.prepare(LegacyTaskProtocol::EvaluateV1).unwrap();
    assert_eq!(
        wire(&bytes)["mutations"],
        json!({"added":[],"deleted":[],"modified":["src/file-004321.txt"]})
    );
    assert!(bytes.len() < 4096);
    assert_eq!(context.manifest.rendered_bytes, bytes.len() as u64);
    for entry in context
        .manifest
        .entries
        .iter()
        .filter(|e| e.name.ends_with("_manifest"))
    {
        assert_eq!(entry.rendered_bytes, 0);
    }
    // The legacy generation keeps its original refusal/identity behavior.
    f.port(
        "requirements",
        "af/Requirements@1",
        None,
        json!({"text":"old", "task_id":"old", "budget":{"reserved_tokens":1}}),
    );
    assert!(
        f.contract(LegacyTaskProtocol::EvaluateV1, false)
            .prepare(&f.cas, &f.input, &[], "Evaluate.")
            .is_err()
    );
}

#[test]
fn malformed_or_excessive_metadata_and_large_delivered_diff_are_refused() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let clean = manifest(2, &digest);
    for mutation in ["ordering", "path", "digest", "unknown", "snapshot"] {
        let mut f = Fixture::new();
        let mut changed = clean.clone();
        match mutation {
            "ordering" => changed["entries"].as_array_mut().unwrap().swap(0, 1),
            "path" => changed["entries"][0]["path"] = json!("../outside"),
            "digest" => changed["entries"][0]["content"] = json!("invalid"),
            "unknown" => changed["entries"][0]["ignored"] = json!(true),
            _ => (),
        }
        f.evaluation(&clean, &changed);
        if mutation == "snapshot" {
            let current_id = f.input.inputs["source"].snapshot_id.as_ref().unwrap();
            let mut current = f.cas.get_json(current_id).unwrap();
            current["content_digest"] = json!(format!("sha256:{}", "b".repeat(64)));
            let id = f.cas.put_json(&current).unwrap();
            f.port("source", "af/SourceTree@1", Some(id), json!({}));
        }
        assert!(
            f.prepare(LegacyTaskProtocol::EvaluateV1).is_err(),
            "{mutation}"
        );
    }
    let mut f = Fixture::new();
    let excessive = manifest(65000, &digest);
    assert!(serde_json::to_vec(&excessive).unwrap().len() as u64 > MAX_LEGACY_METADATA_BYTES);
    f.evaluation(&clean, &excessive);
    let error = f.prepare(LegacyTaskProtocol::EvaluateV1).unwrap_err();
    assert!(error.contains("limit is 8388608"), "{error}");
    // Each Manifest fits the host limit, but a large path change still exceeds Worker input.
    let mut f = Fixture::new();
    let large_diff = manifest(40000, &digest);
    assert!((serde_json::to_vec(&large_diff).unwrap().len() as u64) < MAX_LEGACY_METADATA_BYTES);
    f.evaluation(&manifest(0, &digest), &large_diff);
    assert!(
        f.prepare(LegacyTaskProtocol::EvaluateV1)
            .unwrap_err()
            .contains("Worker context exceeds byte bound")
    );
}
