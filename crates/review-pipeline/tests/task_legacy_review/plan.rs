use super::*;
use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::plan::{ExecutionPlanV1, WorkerExecutionV1};
use review_core::task::{TaskResultV1, TaskRevisionV1};
use review_graph::task::OperatorAttemptCost;
#[path = "plan/integration.rs"]
mod integration;
#[path = "plan/owned.rs"]
mod owned;
#[path = "plan/provider_probe.rs"]
mod provider_probe;
use review_pipeline::task::host::{CapturedTaskAuthority, NoTaskDeveloper, TaskDomain};
use review_pipeline::task::legacy_review::plan::{LegacyReviewPlanCompiler, ReviewPlanSettings};
use review_pipeline::task::{TaskOperatorHost, TaskWorkOutput};
use review_store::store::task::execution::PreparedTaskAttempt;

// Admission must not reach execution or silently grant domain acceptance.
pub(super) struct RefuseExecution;
impl TaskOperatorHost for RefuseExecution {
    fn prepare_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &[String],
    ) -> Result<String, String> {
        panic!("plan admission rendered Worker context")
    }
    fn execute(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        panic!("plan admission executed work")
    }
}
impl TaskDomain for RefuseExecution {
    fn validate_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &[String],
        _: &str,
    ) -> Result<(), String> {
        Err("not executing".into())
    }
    fn validate_output(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        _: &TaskInvocationV1,
        _: &TaskOutputV1,
    ) -> Result<(), String> {
        Err("not executing".into())
    }
    fn validate_result(&self, _: &Cas, _: &TaskRevisionV1, _: &TaskResultV1) -> Result<(), String> {
        Err("not accepting".into())
    }
}

pub(super) fn settings() -> ReviewPlanSettings {
    ReviewPlanSettings {
        mode: "light".into(),
        resources: review_config::task::legacy_review::resources::ReviewResourcePolicy {
            uncapped_attempt_tokens: 1,
        },
        outputs: capture::outputs(),
        executions: BTreeMap::from([("reviewer".into(), WorkerExecutionV1::Command {})]),
        provider_admission: OperatorAttemptCost {
            tokens: 1,
            wall_ms: 1000,
        },
        allowed_effects: Default::default(),
    }
}
pub(super) fn artifact(cas: &Cas, ty: &str, value: impl serde::Serialize) -> String {
    cas.put_artifact(
        ty,
        review_core::Producer::KernelOperation {
            run_id: "review-plan-test".into(),
            node_id: None,
            operation_id: "capture".into(),
        },
        vec![],
        None,
        serde_json::to_value(value).unwrap(),
    )
    .unwrap()
    .0
}
fn path(root: &std::path::Path, id: &str) -> std::path::PathBuf {
    let hex = id.strip_prefix("sha256:").unwrap();
    root.join("objects").join(&hex[..2]).join(&hex[2..])
}

fn assert_cached_authority_is_fresh(
    cas: &Cas,
    root: &std::path::Path,
    compiler: &LegacyReviewPlanCompiler,
    task: &TaskRevisionV1,
    plan: &ExecutionPlanV1,
) {
    let manifest_id = &compiler.round().binding().campaign_manifest_id;
    let manifest: review_core::CampaignManifestV1 =
        serde_json::from_value(cas.get_json(manifest_id).unwrap()).unwrap();
    let snapshot: review_core::SourceSnapshot =
        serde_json::from_value(cas.get_json(&manifest.authority_snapshot_id).unwrap()).unwrap();
    let mut ids = std::collections::BTreeSet::from([
        plan.engine_id.clone(),
        plan.task_revision_id.clone(),
        plan.compiled_graph_id.clone(),
        compiler.policy_id().into(),
        manifest_id.clone(),
        manifest.pipeline.artifact_id,
        manifest.reviewer_lock.artifact_id,
        manifest.authority_snapshot_id,
        manifest.finding_genesis_id,
        manifest.demand_genesis_id,
        snapshot.artifact_manifest.unwrap(),
    ]);
    ids.extend(manifest.project_policy_ids);
    for reviewer in manifest.reviewers {
        ids.insert(reviewer.package_artifact_id.clone());
        let package: review_core::ReviewerPackageV1 =
            serde_json::from_value(cas.get_json(&reviewer.package_artifact_id).unwrap()).unwrap();
        ids.extend(package.files.into_values());
    }
    let wrappers = plan
        .dependencies
        .values()
        .map(|value| value.artifact_id.clone())
        .chain(
            plan.bindings
                .values()
                .map(|value| value.invocation_policy_id.clone()),
        )
        .chain(
            task.inputs
                .values()
                .flat_map(|value| value.artifact_ids.iter().cloned()),
        );
    for id in wrappers {
        ids.extend(cas.get_artifact(&id).unwrap().input_artifacts);
        ids.insert(id);
    }
    compiler.validate_plan(cas, task, plan).unwrap();
    for id in &ids {
        let location = path(root, id);
        let bytes = std::fs::read(&location).unwrap();
        for replacement in [Some(b"corrupt authority".as_slice()), None] {
            if let Some(bytes) = replacement {
                std::fs::write(&location, bytes).unwrap();
            } else {
                std::fs::remove_file(&location).unwrap();
            }
            assert!(
                compiler.validate_plan(cas, task, plan).is_err(),
                "cached authority accepted {id}"
            );
            if replacement.is_none() {
                assert!(!location.exists(), "validation recreated authority");
            }
            std::fs::write(&location, &bytes).unwrap();
            compiler.validate_plan(cas, task, plan).unwrap();
        }
    }
    let count = 16;
    let measure = |memo: bool| {
        let mut samples = Vec::new();
        for _ in 0..count {
            let start = std::time::Instant::now();
            if memo {
                compiler.validate_plan(cas, task, plan).unwrap();
            } else {
                compiler.recompile(cas, task, plan).unwrap();
            }
            samples.push(start.elapsed().as_micros());
        }
        samples.sort_unstable();
        (samples.iter().sum::<u128>(), samples[8], samples[15])
    };
    let full = measure(false);
    let memo = measure(true);
    eprintln!(
        "review-plan-validation: repetitions={count} authority_objects={} full_total_p50_p95_us={full:?} memo_total_p50_p95_us={memo:?}",
        ids.len()
    );
}

#[test]
fn captured_review_plan_admits_reopens_and_refuses_changed_or_missing_authority() {
    let directory = tempfile::tempdir().unwrap();
    let cas_root = directory.path().join("cas");
    let cas = Cas::open(&cas_root).unwrap();
    let store_path = directory.path().join("events.sqlite");
    let mut store = EventStore::open(&store_path).unwrap();
    let round = capture::open_round(&cas, &mut store);
    let engine = cas
        .put(b"installed Review frontend and engine identity")
        .unwrap();
    let compiler = LegacyReviewPlanCompiler::capture(
        &cas,
        CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap(),
        engine.clone(),
        settings(),
    )
    .unwrap();
    let task = compiler
        .prepare_revision(&cas, "review-task", capture::limits())
        .unwrap();
    assert_eq!(task.required_outputs.len(), 4);
    assert_eq!(task.acceptance.len(), 4);
    let revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
    let (plan, captured) = compiler.compile(&cas, &revision).unwrap();
    assert_plan_schemas(&cas, &compiler, &plan);
    assert_cached_authority_is_fresh(&cas, &cas_root, &compiler, &task, &plan);
    assert_eq!(plan.dependencies.len(), 2);
    assert_eq!(plan.bindings.len(), 1);
    assert!(plan.generated_origins.is_empty());
    assert!(!plan.requires_developer_approval());
    assert_eq!(
        captured.compilation.graph.calls["root"].coverage,
        captured.compilation.graph.coverage
    );
    assert_eq!(plan.acceptance.len(), 4);
    assert_eq!(
        captured.compilation.graph.token_scopes["review.round1"].tokens,
        50
    );
    for dependency in plan.dependencies.values() {
        let wrapper = cas.get_artifact(&dependency.artifact_id).unwrap();
        assert_eq!(wrapper.content_id, dependency.content_digest);
        assert_eq!(
            wrapper.payload["campaign_manifest_id"],
            compiler.round().binding().campaign_manifest_id
        );
    }
    let plan_id = artifact(&cas, review_core::task::EXECUTION_PLAN_V1, &plan);
    let authority =
        CapturedTaskAuthority::for_legacy_review(&compiler, &RefuseExecution, &NoTaskDeveloper);
    let lease = store
        .open_task(&cas, &revision, "developer", 60000)
        .unwrap();
    store
        .propose_task_plan(&cas, &lease, &plan_id, &authority)
        .unwrap();
    store.admit_task_plan(&cas, &lease, &authority).unwrap();
    let run = review_store::store::task::task_run_id(&task.task_id).unwrap();
    assert!(store.attempt_wall(&run).unwrap().is_empty());
    assert_eq!(store.len("review").unwrap(), 2);
    let events = store.len(&run).unwrap();
    let reopened = LegacyReviewPlanCompiler::reopen(
        &cas,
        CapturedLegacyReviewRound::load_recorded(&cas, &store, "review", &round).unwrap(),
        &engine,
        compiler.policy_id(),
    )
    .unwrap();
    assert_eq!(reopened.validate_plan(&cas, &task, &plan).unwrap(), vec![]);
    assert_eq!(reopened.compile(&cas, &revision).unwrap().0, plan);
    let mut changed = plan.clone();
    changed.acceptance.retain(|name, _| name == "findings");
    assert!(reopened.validate_plan(&cas, &task, &changed).is_err());
    let mut changed = plan.clone();
    changed.limits.tokens += 1;
    assert!(reopened.validate_plan(&cas, &task, &changed).is_err());
    let mut changed = plan.clone();
    changed.bindings.values_mut().next().unwrap().package_digest = engine.clone();
    assert!(reopened.validate_plan(&cas, &task, &changed).is_err());
    let mut graph = cas.get_artifact(&plan.compiled_graph_id).unwrap();
    graph.payload["max_parallel"] = serde_json::json!(8);
    let forged_graph = cas
        .put_artifact(
            graph.artifact_type,
            graph.producer,
            graph.input_artifacts,
            graph.subject_snapshot_id,
            graph.payload,
        )
        .unwrap()
        .0;
    let mut changed = plan.clone();
    changed.compiled_graph_id = forged_graph;
    assert!(reopened.validate_plan(&cas, &task, &changed).is_err());
    let mut checked = plan
        .dependencies
        .values()
        .map(|d| d.artifact_id.clone())
        .collect::<Vec<_>>();
    checked.extend([compiler.policy_id().into(), plan.compiled_graph_id.clone()]);
    checked.extend(
        plan.bindings
            .values()
            .map(|binding| binding.invocation_policy_id.clone()),
    );
    checked.extend(
        task.inputs
            .values()
            .flat_map(|input| input.artifact_ids.clone()),
    );
    for id in checked {
        let path = path(&cas_root, &id);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(
            reopened.validate_plan(&cas, &task, &plan).is_err(),
            "missing {id}"
        );
        assert!(!path.exists(), "read-only validation recreated {id}");
        std::fs::write(path, bytes).unwrap();
        reopened.validate_plan(&cas, &task, &plan).unwrap();
    }
    assert_eq!(store.len(&run).unwrap(), events);
    assert!(store.attempt_wall(&run).unwrap().is_empty());
    let store = EventStore::open(&store_path).unwrap();
    assert!(
        store
            .task_projection(&cas, &task.task_id)
            .unwrap()
            .unwrap()
            .admitted
    );
}

#[test]
fn captured_native_runner_requires_exact_model_binding_and_common_provider_budget() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let pipeline = PIPELINE.replace("runner = { program = \"/bin/true\" }", "runner = { program = \"claude\", args = [{value = \"--model\"}, {value = \"claude-fixture\"}, {value = \"--effort\"}, {value = \"high\"}] }");
    let round = capture::open_round_with_pipeline(&cas, &mut store, &pipeline);
    let engine = cas.put(b"fixture installed engine").unwrap();
    let capture = |settings| {
        LegacyReviewPlanCompiler::capture(
            &cas,
            CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap(),
            engine.clone(),
            settings,
        )
    };
    assert!(
        capture(settings()).is_err(),
        "Command binding must not bypass paid Provider admission"
    );
    let mut selected = settings();
    selected.executions.insert(
        "reviewer".into(),
        WorkerExecutionV1::Model {
            provider: "claude-personal".into(),
            provider_kind: "claude".into(),
            principal_id: "fixture-principal".into(),
            model: "claude-fixture".into(),
            effort: "high".into(),
        },
    );
    let compiler = capture(selected.clone()).unwrap();
    let task = compiler
        .prepare_revision(&cas, "native-review", capture::limits())
        .unwrap();
    let revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
    let (plan, compilation) = compiler.compile(&cas, &revision).unwrap();
    assert_plan_schemas(&cas, &compiler, &plan);
    let graph = &compilation.compilation.graph;
    assert_eq!(graph.allowances.len(), 2);
    assert_eq!(
        graph.allowances["root.providers.admit0"].tokens_per_attempt,
        1
    );
    assert!(graph.token_scopes["review.round1"].contains("root.providers.admit0"));
    assert_eq!(
        graph
            .budget(task.limits.clone())
            .unwrap()
            .committed_tokens(),
        0
    );
    let reviewer = &graph.nodes[&compilation.compilation.nodes["reviewer"].task_node];
    assert!(
        reviewer
            .conditions
            .iter()
            .any(|condition| condition.source.node == "root.providers.admit0")
    );
    compiler.validate_plan(&cas, &task, &plan).unwrap();
    let mut changed = plan.clone();
    if let WorkerExecutionV1::Model { provider, .. } =
        &mut changed.bindings.values_mut().next().unwrap().execution
    {
        *provider = "ambient-work".into();
    }
    assert!(compiler.validate_plan(&cas, &task, &changed).is_err());
    if let WorkerExecutionV1::Model { effort, .. } =
        selected.executions.get_mut("reviewer").unwrap()
    {
        *effort = "low".into();
    }
    assert!(capture(selected).is_err());
    assert_eq!(store.len("review").unwrap(), 2);
}

fn assert_plan_schemas(cas: &Cas, compiler: &LegacyReviewPlanCompiler, plan: &ExecutionPlanV1) {
    use std::sync::OnceLock;
    static VALIDATORS: OnceLock<BTreeMap<&'static str, jsonschema::Validator>> = OnceLock::new();
    let validators = VALIDATORS.get_or_init(|| {
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let common: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("schemas/task-contracts-v1.json")).unwrap(),
        )
        .unwrap();
        [
            (
                "af/LegacyReviewTaskPolicy@1",
                "legacy-review-task-policy-v1.json",
            ),
            (
                "af/LegacyReviewTaskPolicy@2",
                "legacy-review-task-policy-v2.json",
            ),
            (
                "af/LegacyReviewTaskPolicy@3",
                "legacy-review-task-policy-v3.json",
            ),
            (
                "af/TaskProviderProbePolicy@1",
                "task-provider-probe-policy-v1.json",
            ),
            ("af/CompiledTask@1", "compiled-task-v1.json"),
            (
                "af/LegacyReviewDependency@1",
                "legacy-review-dependency-v1.json",
            ),
            (
                "af/LegacyReviewInvocationPolicy@1",
                "legacy-review-invocation-policy-v1.json",
            ),
        ]
        .into_iter()
        .map(|(ty, file)| {
            let schema: serde_json::Value =
                serde_json::from_slice(&std::fs::read(root.join("schemas").join(file)).unwrap())
                    .unwrap();
            let mut registry = jsonschema::Registry::new();
            let id = common["$id"].as_str().unwrap().to_owned();
            registry = registry
                .add(id, jsonschema::Resource::from_contents(common.clone()))
                .unwrap();
            for file in [
                "legacy-review-task-policy-v1.json",
                "task-broker-binding-v1.json",
            ] {
                let value: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(root.join("schemas").join(file)).unwrap(),
                )
                .unwrap();
                let id = value["$id"].as_str().unwrap().to_owned();
                registry = registry
                    .add(id, jsonschema::Resource::from_contents(value))
                    .unwrap();
            }
            (ty, {
                let registry = registry.prepare().unwrap();
                jsonschema::options()
                    .with_registry(&registry)
                    .build(&schema)
                    .unwrap()
            })
        })
        .collect()
    });
    let ids = std::iter::once(compiler.policy_id())
        .chain(std::iter::once(plan.compiled_graph_id.as_str()))
        .chain(plan.dependencies.values().map(|d| d.artifact_id.as_str()))
        .chain(
            plan.bindings
                .values()
                .map(|b| b.invocation_policy_id.as_str()),
        );
    for id in ids {
        let artifact = cas.get_artifact(id).unwrap();
        let validator = &validators[artifact.artifact_type.as_str()];
        assert!(
            validator.is_valid(&artifact.payload),
            "{}: {:?}",
            artifact.artifact_type,
            validator
                .iter_errors(&artifact.payload)
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );
        let mut extra = artifact.payload.clone();
        extra["ambient_authority"] = serde_json::json!(true);
        assert!(!validator.is_valid(&extra));
        let mut empty = artifact.payload;
        empty.as_object_mut().unwrap().clear();
        assert!(!validator.is_valid(&empty));
    }
}
