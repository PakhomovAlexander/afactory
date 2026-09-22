use super::super::capture::captured_fixture;
use super::*;
use review_core::task::plan::{ExecutionPlanV1, WorkerExecutionV1};
use review_pipeline::task::host::TaskModelBinding;
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Model {
    provider_kind: &'static str,
    calls: AtomicUsize,
    admitted: bool,
    retry: bool,
    wide: bool,
}
impl WorkerModelAdapter for Model {
    fn provider_kind(&self) -> &'static str {
        self.provider_kind
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some((format!("{}-fixture", self.provider_kind), "high".into()))
    }
    fn invoke(
        &self,
        cas: &Cas,
        _: &std::path::Path,
        input: Vec<u8>,
        _: std::time::Duration,
        writable: bool,
    ) -> ModelWorkerReturn {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let (message, usage) = if n == 0 {
            assert!(!writable);
            assert_eq!(input, b"Reply with exactly: OK\n");
            (if self.admitted { "OK" } else { "unavailable" }, 1)
        } else {
            assert!(self.admitted);
            assert_eq!(writable, self.provider_kind == "codex");
            assert!(
                n <= if self.retry { 2 } else { 1 },
                "replay must reuse the selected Attempt"
            );
            let prompt = String::from_utf8(input).unwrap();
            assert!(prompt.contains("Captured instruction marker."));
            assert!(!prompt.contains("PRIVATE_MALFORMED_RESPONSE"));
            if self.retry && n == 1 {
                ("PRIVATE_MALFORMED_RESPONSE", 11)
            } else {
                assert_eq!(prompt.contains("invalid_output_contract"), self.retry);
                (
                    r#"{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]}"#,
                    11,
                )
            }
        };
        let mut usage = review_core::task::usage::TaskTokenUsageV3::charge_only(usage);
        if self.wide && n > 0 {
            usage.input_tokens = Some((u128::from(u64::MAX) + 20).into());
        }
        ModelWorkerReturn {
            usage_observation: None,
            message: Ok(message.as_bytes().to_vec()),
            usage: Some(usage),
            raw_artifact_ids: vec![cas.put(message.as_bytes()).unwrap()],
        }
    }
}

fn admitted_plan(
    cas: &Cas,
    store: &mut EventStore,
    mode: &str,
) -> (
    LegacyReviewPlanCompiler,
    review_store::store::task::TaskLease,
    ExecutionPlanV1,
) {
    admitted_plan_with_provider(cas, store, mode, "claude", None)
}

/// The packaged `codex` fixture Worker's files.
fn codex_package() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        ("reviewer.toml".into(), b"name=\"fixture\"\nversion=\"1.0.0\"\nsubjects=[\"whole-tree\"]\n[runner]\nprogram=\"codex\"\nargs=[{value=\"--model\"},{value=\"codex-fixture\"},{value=\"-c\"},{value=\"model_reasoning_effort=high\"}]\n".to_vec()),
        ("reviewer.md".into(), b"Review the exact declared Subject. Captured instruction marker.".to_vec()),
    ])
}

fn admitted_plan_with_provider(
    cas: &Cas,
    store: &mut EventStore,
    mode: &str,
    provider_kind: &str,
    focus: Option<&str>,
) -> (
    LegacyReviewPlanCompiler,
    review_store::store::task::TaskLease,
    ExecutionPlanV1,
) {
    let definition = PIPELINE
        .replace("version = 2", "version = 4\n[gate]\nprovider=\"trusted_local\"\nrequired_isolation=\"none\"\nmode=\"ephemeral-write\"")
        .replace("runner = { program = \"/bin/true\" }", &format!("package=\"fixture\"\ngated_by=\"gate\"\nexecution={{credential_mode=\"{mode}\"}}"))
        + "\n[[nodes]]\nid=\"gate\"\nkind=\"gate\"\noutputs=[\"decision\"]\n[[checks]]\nname=\"required\"\nprogram=\"/bin/sh\"\nargs=[{value=\"-c\"},{value=\"exit 0\"}]\n";
    let package = if provider_kind == "claude" {
        capture::claude_package()
    } else {
        assert_eq!(provider_kind, "codex");
        codex_package()
    };
    let round = captured_fixture::open_round_authority_with_focus(
        cas,
        store,
        &definition,
        Some(package),
        focus,
    );
    let mut settings = plan::settings();
    settings.provider_admission.tokens = 32;
    settings.executions.insert(
        "reviewer".into(),
        WorkerExecutionV1::Model {
            provider: format!("{provider_kind}-personal"),
            provider_kind: provider_kind.into(),
            principal_id: "fixture-personal-account".into(),
            model: format!("{provider_kind}-fixture"),
            effort: "high".into(),
        },
    );
    let compiler = LegacyReviewPlanCompiler::capture(
        cas,
        CapturedLegacyReviewRound::load(cas, store, "review", &round).unwrap(),
        cas.put(b"native Review test engine").unwrap(),
        plan::without_probes(settings),
    )
    .unwrap();
    let mut limits = capture::limits();
    limits.tokens = 100_000;
    let task = compiler
        .prepare_revision(cas, "native-host-review", limits)
        .unwrap();
    let revision = plan::artifact(cas, review_core::task::TASK_REVISION_V1, &task);
    let (compiled, _) = compiler.compile(cas, &revision).unwrap();
    let plan_id = plan::artifact(cas, review_core::task::EXECUTION_PLAN_V1, &compiled);
    let authority = CapturedTaskAuthority::for_legacy_review(
        &compiler,
        &plan::RefuseExecution,
        &NoTaskDeveloper,
    );
    let lease = store.open_task(cas, &revision, "developer", 60000).unwrap();
    store
        .propose_task_plan(cas, &lease, &plan_id, &authority)
        .unwrap();
    store.admit_task_plan(cas, &lease, &authority).unwrap();
    (compiler, lease, compiled)
}

#[test]
fn packaged_review_uses_one_captured_provider_binding_for_admission_and_business_work() {
    check_native_provider_reuse(false);
}

#[test]
fn native_provider_only_execution_reuses_original_attempts_before_full_review() {
    check_native_provider_reuse(true);
}

fn check_native_provider_reuse(provider_only: bool) {
    for (provider_kind, admitted, retry, wide) in [
        ("claude", true, false, false),
        ("claude", true, true, false),
        ("claude", false, false, false),
        ("claude", true, false, true),
        ("codex", true, false, false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let (compiler, lease, plan) =
            admitted_plan_with_provider(&cas, &mut store, "trusted_unsafe", provider_kind, None);
        let model = Model {
            provider_kind,
            calls: AtomicUsize::new(0),
            admitted,
            retry,
            wide,
        };
        let shared = SharedEventStore::new(&mut store);
        for pass in 0..if provider_only { 4 } else { 2 } {
            let models = plan
                .bindings
                .iter()
                .map(|(slot, binding)| {
                    (
                        slot.clone(),
                        TaskModelBinding {
                            binding: binding.clone(),
                            adapter: &model as &dyn WorkerModelAdapter,
                        },
                    )
                })
                .collect();
            let host =
                LegacyReviewTaskHost::new(&cas, shared.clone(), &compiler, lease.clone(), models)
                    .unwrap();
            let authority =
                CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
            let runtime =
                TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host)
                    .unwrap();
            if provider_only && pass < 2 {
                let doctor = runtime.execute_provider_admissions().unwrap();
                assert_eq!(doctor.ready(), admitted, "{doctor:?}");
                assert_eq!(doctor.outcomes.len(), 1);
                let projection = runtime.projection().unwrap();
                assert!(
                    projection.run_reports.is_empty(),
                    "doctor is not a graph report"
                );
                let execution = projection.execution.unwrap();
                assert_eq!(
                    execution.invocations.len(),
                    1,
                    "no Gates, roots or Workers ran"
                );
                let node = &doctor.outcomes[0].0;
                assert!(execution.invocations.contains_key(node));
                assert_eq!(execution.budget.committed_tokens(), 1);
                assert_eq!(execution.budget.begun_attempts(), 1);
                assert_eq!(execution.attempt_accounting()[0].reservation.node, *node);
                assert_eq!(
                    model.calls.load(Ordering::SeqCst),
                    1,
                    "failed and selected probes retain the original cap"
                );
                assert!(host.selected_attempt_evidence().unwrap().is_empty());
                assert!(
                    !shared
                        .lock()
                        .unwrap()
                        .replay("review")
                        .unwrap()
                        .iter()
                        .any(|e| e.event_type == EventType::RunReportV6)
                );
                continue; // Drop and reconstruct the host/runtime from the same durable Task.
            }
            let report = runtime.execute().unwrap();
            assert_eq!(report.complete(), admitted, "{report:?}");
            let execution = runtime.projection().unwrap().execution.unwrap();
            if admitted {
                use review_core::task::review_compat::*;
                let metadata = execution
                    .outputs
                    .values()
                    .find_map(|(_, output)| output.outputs.get("metadata"))
                    .unwrap();
                let metadata: TaskReviewResultMetadataV1 = serde_json::from_value(
                    cas.get_artifact(&metadata.artifact_ids[0]).unwrap().payload,
                )
                .unwrap();
                let frame = cas.get_artifact(&metadata.provenance_artifact_id).unwrap();
                assert_eq!(
                    frame.artifact_type,
                    if wide {
                        TASK_REVIEW_ATTEMPT_PROVENANCE_V2
                    } else {
                        TASK_REVIEW_ATTEMPT_PROVENANCE_V1
                    }
                );
                let provenance: TaskReviewAttemptProvenanceV2 = if wide {
                    serde_json::from_value(frame.payload).unwrap()
                } else {
                    serde_json::from_value::<TaskReviewAttemptProvenanceV1>(frame.payload)
                        .unwrap()
                        .into()
                };
                assert_eq!(provenance.charged_tokens.get(), 11);
                let usage = cas
                    .get_artifact(provenance.usage_id.as_ref().unwrap())
                    .unwrap();
                assert_eq!(usage.payload["chargeable_tokens"], "11");
                let evidence = host.selected_attempt_evidence().unwrap();
                assert_eq!(
                    evidence.len(),
                    1,
                    "Provider admission and retry failures are not selected Review evidence"
                );
                assert_eq!(evidence[0].node, "reviewer");
                assert_eq!(evidence[0].attempt_id, provenance.attempt_id);
                assert_eq!(evidence[0].cost_tokens, 11);
                assert_eq!(evidence[0].usage.chargeable_tokens.get(), 11);
                assert_eq!(evidence[0].raw_artifact, provenance.raw_artifact_id);
                assert_eq!(evidence[0].result_artifact, provenance.result_artifact_id);
                if wide {
                    assert_eq!(
                        usage.payload["input_tokens"],
                        (u128::from(u64::MAX) + 20).to_string()
                    );
                    assert_eq!(
                        usage.artifact_type,
                        review_core::task::usage::TASK_TOKEN_USAGE_V3
                    );
                    assert_eq!(
                        evidence[0].usage.input_tokens.map(|n| n.get()),
                        Some(u128::from(u64::MAX) + 20)
                    );
                }
            } else {
                assert!(host.selected_attempt_evidence().unwrap().is_empty());
            }
            assert_eq!(
                execution.budget.committed_tokens(),
                if retry {
                    23
                } else if admitted {
                    12
                } else {
                    1
                }
            );
            assert_eq!(
                execution.budget.begun_attempts(),
                if retry {
                    4
                } else if admitted {
                    3
                } else {
                    2
                }
            );
        }
        assert_eq!(
            model.calls.load(Ordering::SeqCst),
            if retry {
                3
            } else if admitted {
                2
            } else {
                1
            }
        );
    }
}

/// `af review render` shows the prompt an adapter composes from the package and the focus
/// (`from_package`, `with_focus`, `render_input`); the Task host composes the prompt it
/// dispatches from the captured package and the Campaign's recorded focus. The instructions,
/// the focus narrowing and the output contract must be the same bytes on both sides; what
/// follows is the shared input rendering over Campaign-bound inputs render cannot know.
#[test]
fn dispatched_review_prompt_starts_with_the_rendered_instructions_and_focus() {
    use review_core::task::review_compat::{TaskReviewContextV1, TaskReviewResultSelectedV1};
    use review_runner::ReviewerAdapter;
    const FOCUS: &str = "the parser";
    for provider_kind in ["claude", "codex"] {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let (compiler, lease, plan) = admitted_plan_with_provider(
            &cas,
            &mut store,
            "trusted_unsafe",
            provider_kind,
            Some(FOCUS),
        );
        let model = Model {
            provider_kind,
            calls: AtomicUsize::new(0),
            admitted: true,
            retry: false,
            wide: false,
        };
        let models = plan
            .bindings
            .iter()
            .map(|(slot, binding)| {
                (
                    slot.clone(),
                    TaskModelBinding {
                        binding: binding.clone(),
                        adapter: &model as &dyn WorkerModelAdapter,
                    },
                )
            })
            .collect();
        let shared = SharedEventStore::new(&mut store);
        let host =
            LegacyReviewTaskHost::new(&cas, shared.clone(), &compiler, lease.clone(), models)
                .unwrap();
        let authority =
            CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
        let runtime =
            TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host)
                .unwrap();
        assert!(runtime.execute().unwrap().complete());
        let selected = shared
            .lock()
            .unwrap()
            .replay("review")
            .unwrap()
            .into_iter()
            .find(|event| event.event_type == EventType::TaskReviewResultSelectedV1)
            .unwrap();
        let selected: TaskReviewResultSelectedV1 =
            serde_json::from_value(selected.payload).unwrap();
        let context: TaskReviewContextV1 =
            serde_json::from_value(cas.get_artifact(&selected.context_id).unwrap().payload)
                .unwrap();
        let dispatched = cas.get(&context.rendered_input_id).unwrap();
        let dispatched_manifest = cas.get_json(&context.context_manifest_id).unwrap();
        let result_contract =
            cas.get_json(&context.reviewer_inputs_id).unwrap()["result_contract"].clone();
        let inputs = review_runner::ReviewerInputs {
            result_contract: if result_contract.is_null() {
                Default::default()
            } else {
                serde_json::from_value(result_contract).unwrap()
            },
            ..Default::default()
        };

        let files = if provider_kind == "claude" {
            capture::claude_package()
        } else {
            codex_package()
        };
        let package = review_runner::ResolvedReviewer::new(
            "fixture",
            "1.0.0",
            "fixture-digest",
            directory.path(),
            review_core::Command::new(provider_kind, vec![]),
            files,
        );
        let rendered = if provider_kind == "claude" {
            review_runner_claude::ClaudeAdapter::from_package(&package)
                .unwrap()
                .with_focus(FOCUS)
                .render_input(&inputs)
        } else {
            review_runner_codex::CodexAdapter::from_package(&package)
                .unwrap()
                .with_focus(FOCUS)
                .render_input(&inputs)
        }
        .unwrap();
        let instructions = &rendered.manifest.entries[0];
        assert_eq!(instructions.name, "worker_instructions");
        let prefix = &rendered.bytes[..instructions.rendered_bytes as usize];
        assert!(
            String::from_utf8_lossy(prefix).contains("\n\n## Focus for this run\n\nthe parser"),
            "{provider_kind}: render lost the focus"
        );
        assert_eq!(
            dispatched_manifest["entries"][0]["name"], "worker_instructions",
            "{provider_kind}"
        );
        assert_eq!(
            dispatched_manifest["entries"][0]["rendered_bytes"], instructions.rendered_bytes,
            "{provider_kind}: dispatched and rendered instructions differ in length"
        );
        assert!(
            dispatched.starts_with(prefix),
            "{provider_kind}: the Task host dispatched other instructions than render shows"
        );
    }
}

#[test]
fn captured_review_refuses_credential_mode_and_account_substitution_before_dispatch() {
    for mode in ["credential_free", "trusted_unsafe"] {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let (compiler, lease, plan) = admitted_plan(&cas, &mut store, mode);
        let model = Model {
            provider_kind: "claude",
            calls: AtomicUsize::new(0),
            admitted: true,
            retry: false,
            wide: false,
        };
        let models = plan
            .bindings
            .iter()
            .map(|(slot, binding)| {
                let mut binding = binding.clone();
                if mode == "trusted_unsafe" {
                    if let WorkerExecutionV1::Model { principal_id, .. } = &mut binding.execution {
                        *principal_id = "different-account".into();
                    }
                }
                (
                    slot.clone(),
                    TaskModelBinding {
                        binding,
                        adapter: &model as &dyn WorkerModelAdapter,
                    },
                )
            })
            .collect();
        let shared = SharedEventStore::new(&mut store);
        let error =
            LegacyReviewTaskHost::new(&cas, shared.clone(), &compiler, lease.clone(), models)
                .err()
                .expect("unsafe substitution admitted");
        assert!(
            error.contains(if mode == "credential_free" {
                "credential mode"
            } else {
                "exact account"
            }),
            "{error}"
        );
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        assert!(
            shared
                .lock()
                .unwrap()
                .attempt_wall(&review_store::store::task::task_run_id(lease.task_id()).unwrap())
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn review_report_binds_wide_task_charge_and_freezes_its_accounting_prefix() {
    use review_core::task::execution::TaskExecutionRecordV1;
    use review_graph::task::{CompiledOperator, ReviewOperation};
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let path = directory.path().join("events.sqlite");
    let before_path = directory.path().join("before-conclusion.sqlite");
    let mut store = EventStore::open(&path).unwrap();
    let (compiler, lease, compiled) = admitted_plan(&cas, &mut store, "trusted_unsafe");
    let model = Model {
        provider_kind: "claude",
        calls: AtomicUsize::new(0),
        admitted: true,
        retry: false,
        wide: true,
    };
    let models = || {
        compiled
            .bindings
            .iter()
            .map(|(slot, binding)| {
                (
                    slot.clone(),
                    TaskModelBinding {
                        binding: binding.clone(),
                        adapter: &model as &dyn WorkerModelAdapter,
                    },
                )
            })
            .collect()
    };
    let (provider_attempt, reviewer_attempt) = {
        let shared = SharedEventStore::new(&mut store);
        let host =
            LegacyReviewTaskHost::new(&cas, shared.clone(), &compiler, lease.clone(), models())
                .unwrap();
        let authority =
            CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
        let runtime =
            TaskRuntime::with_store(shared, &cas, lease.clone(), &authority, &host).unwrap();
        assert!(runtime.execute().unwrap().complete());
        let execution = runtime.projection().unwrap().execution.unwrap();
        let artifacts = execution.settled_artifacts();
        let provider = artifacts
            .iter()
            .find(|(_, (node, _))| {
                matches!(
                    execution.graph.nodes[node].operator,
                    CompiledOperator::ProviderAdmission { .. }
                )
            })
            .unwrap()
            .0
            .clone();
        let reviewer = artifacts
            .iter()
            .find(|(_, (node, _))| {
                matches!(
                    execution.graph.nodes[node].operator,
                    CompiledOperator::ReviewDomain {
                        operation: ReviewOperation::Reviewer { .. },
                        ..
                    }
                )
            })
            .unwrap()
            .0
            .clone();
        (provider, reviewer)
    };
    let observe = |store: &mut EventStore, attempt: &str, charge: u64| {
        let usage = review_core::task::usage::TaskTokenUsageV1 {
            chargeable_tokens: charge.into(),
            ..Default::default()
        };
        let usage_id = cas
            .put_artifact(
                review_core::task::usage::TASK_TOKEN_USAGE_V1,
                review_core::Producer::KernelOperation {
                    run_id: review_store::store::task::task_run_id(lease.task_id()).unwrap(),
                    node_id: None,
                    operation_id: "late-provider-usage@1".into(),
                },
                vec![],
                None,
                serde_json::to_value(usage).unwrap(),
            )
            .unwrap()
            .0;
        store
            .observe_task_usage(
                &cas,
                &lease,
                TaskExecutionRecordV1::UsageObserved {
                    attempt_id: attempt.into(),
                    charged_tokens: u128::from(charge),
                    usage_id,
                    raw_artifact_ids: vec![],
                },
            )
            .unwrap();
    };
    observe(&mut store, &provider_attempt, u64::MAX);
    let prefix = store
        .task_projection(&cas, lease.task_id())
        .unwrap()
        .unwrap()
        .next_sequence
        - 1;
    drop(store);
    std::fs::copy(&path, &before_path).unwrap();
    let mut store = EventStore::open(&path).unwrap();
    {
        let shared = SharedEventStore::new(&mut store);
        let host =
            LegacyReviewTaskHost::new(&cas, shared, &compiler, lease.clone(), models()).unwrap();
        let result = host.assemble_recorded_result(&cas).unwrap();
        assert_eq!(
            result.execution,
            review_core::task::TaskExecutionV1::Exhausted
        );
        assert_eq!(
            result.acceptance,
            review_core::task::TaskAcceptanceV1::Inconclusive
        );
    }
    let conclusion = store
        .replay("review")
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == EventType::RunReportV6)
        .unwrap();
    let report: review_core::RunReportPayloadV6 =
        serde_json::from_value(conclusion.payload.clone()).unwrap();
    assert_eq!(report.spent_tokens.get(), u64::MAX as u128 + 11);
    assert_eq!(
        report.verdict,
        review_core::RunVerdictV3::Fail {
            reason: review_core::RunFailureReasonV3::Exhausted
        }
    );
    assert_eq!(report.task_accounting.through_sequence, prefix);
    assert!(matches!(
        report.execution,
        review_core::RunReportExecutionV6::Bound { .. }
    ));
    let report_id = report.task_accounting.task_report_id.clone();
    // Each forged candidate is tried against the same real, open pre-conclusion Store.
    for corruption in [
        "none",
        "total",
        "prefix",
        "task",
        "revision",
        "plan",
        "report",
        "refs",
        "type",
        "execution",
        "bindings",
        "verdict",
        "incomplete_binding_omission",
    ] {
        let fork_path = directory.path().join(format!("{corruption}.sqlite"));
        std::fs::copy(&before_path, &fork_path).unwrap();
        let mut fork = EventStore::open(fork_path).unwrap();
        let shared = SharedEventStore::new(&mut fork);
        let host =
            LegacyReviewTaskHost::new(&cas, shared.clone(), &compiler, lease.clone(), models())
                .unwrap();
        let authority =
            CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
        let mut candidate =
            review_store::NewEvent::new(conclusion.event_type, conclusion.payload.clone())
                .referencing(conclusion.artifact_refs.clone());
        candidate.causation_id = conclusion.causation_id.clone();
        candidate.correlation_id = conclusion.correlation_id.clone();
        match corruption {
            "none" => {}
            "verdict" => candidate.payload["verdict"] = serde_json::json!({"kind":"pass"}),
            "incomplete_binding_omission" => {
                candidate.payload["verdict"] = serde_json::json!({"kind":"incomplete","missing_nodes":[{"node":"gate","reason":"not started"}]});
                for outcome in candidate.payload["outcomes"].as_array_mut().unwrap() {
                    if outcome["node"] == "gate" {
                        outcome["outcome"] =
                            serde_json::json!({"kind":"failed","error":"not started"});
                    }
                }
                candidate.payload["execution"]["execution_bindings"] = serde_json::json!([]);
            }
            "total" => candidate.payload["spent_tokens"] = serde_json::json!("0"),
            "prefix" => {
                candidate.payload["task_accounting"]["through_sequence"] =
                    serde_json::json!(prefix - 1)
            }
            "task" => {
                candidate.payload["task_accounting"]["task_id"] = serde_json::json!("another-task")
            }
            "revision" | "plan" | "report" => {
                let field = match corruption {
                    "revision" => "task_revision_id",
                    "plan" => "plan_id",
                    _ => "task_report_id",
                };
                candidate.payload["task_accounting"][field] =
                    serde_json::json!(format!("sha256:{}", "0".repeat(64)));
            }
            "refs" => candidate
                .artifact_refs
                .retain(|id| id != &report.task_accounting.plan_id),
            // The only other event the Task Review entry point publishes.
            "type" => candidate.event_type = EventType::TaskReviewResultSelectedV1,
            "execution" => candidate.payload["execution"] = serde_json::json!({"kind":"unbound"}),
            "bindings" => {
                candidate.payload["execution"]["execution_bindings"][0]["admitted"] =
                    serde_json::json!(false)
            }
            _ => unreachable!(),
        }
        assert!(
            shared
                .lock()
                .unwrap()
                .append("review", &cas, candidate.clone())
                .is_err(),
            "ordinary append must not claim Task accounting: {corruption}"
        );
        let accepted = shared
            .lock()
            .unwrap()
            .publish_task_review_report(&cas, &lease, &report_id, candidate, &authority);
        assert_eq!(
            accepted.is_ok(),
            corruption == "none",
            "{corruption}: {accepted:?}"
        );
    }
    observe(&mut store, &reviewer_attempt, u64::MAX);
    observe(&mut store, &reviewer_attempt, u64::MAX);
    let state = store
        .task_projection(&cas, lease.task_id())
        .unwrap()
        .unwrap();
    assert_eq!(
        state.execution.unwrap().budget.committed_tokens(),
        2 * u64::MAX as u128
    );
    {
        let shared = SharedEventStore::new(&mut store);
        let host =
            LegacyReviewTaskHost::new(&cas, shared, &compiler, lease.clone(), models()).unwrap();
        host.assemble_recorded_result(&cas).unwrap();
    }
    let reports: Vec<_> = store
        .replay("review")
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type.is_run_report())
        .collect();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].payload, conclusion.payload);
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn late_provider_usage_after_doctor_blocks_business_without_reprobing() {
    use review_core::task::execution::TaskExecutionRecordV1;
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let path = directory.path().join("events.sqlite");
    let mut store = EventStore::open(&path).unwrap();
    let (compiler, lease, plan) = admitted_plan(&cas, &mut store, "trusted_unsafe");
    let model = Model {
        provider_kind: "claude",
        calls: AtomicUsize::new(0),
        admitted: true,
        retry: false,
        wide: false,
    };
    let models = || {
        plan.bindings
            .iter()
            .map(|(slot, binding)| {
                (
                    slot.clone(),
                    TaskModelBinding {
                        binding: binding.clone(),
                        adapter: &model as &dyn WorkerModelAdapter,
                    },
                )
            })
            .collect()
    };
    let (attempt_id, selected) = {
        let shared = SharedEventStore::new(&mut store);
        let host =
            LegacyReviewTaskHost::new(&cas, shared.clone(), &compiler, lease.clone(), models())
                .unwrap();
        let authority =
            CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
        let runtime =
            TaskRuntime::with_store(shared, &cas, lease.clone(), &authority, &host).unwrap();
        assert!(runtime.execute_provider_admissions().unwrap().ready());
        let state = runtime.projection().unwrap();
        assert!(state.run_reports.is_empty());
        let execution = state.execution.unwrap();
        assert_eq!(execution.budget.begun_attempts(), 1);
        (
            execution.attempt_accounting()[0].attempt_id.clone(),
            execution.outputs,
        )
    };
    let usage = review_core::task::usage::TaskTokenUsageV1 {
        chargeable_tokens: u64::MAX.into(),
        ..Default::default()
    };
    let usage_id = cas
        .put_artifact(
            review_core::task::usage::TASK_TOKEN_USAGE_V1,
            review_core::Producer::KernelOperation {
                run_id: review_store::store::task::task_run_id(lease.task_id()).unwrap(),
                node_id: None,
                operation_id: "late-provider-usage@1".into(),
            },
            vec![],
            None,
            serde_json::to_value(usage).unwrap(),
        )
        .unwrap()
        .0;
    store
        .observe_task_usage(
            &cas,
            &lease,
            TaskExecutionRecordV1::UsageObserved {
                attempt_id,
                charged_tokens: u128::from(u64::MAX),
                usage_id,
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
    drop(store);
    let mut store = EventStore::open(&path).unwrap();
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(&cas, shared.clone(), &compiler, lease.clone(), models())
        .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime = TaskRuntime::with_store(shared, &cas, lease, &authority, &host).unwrap();
    assert!(
        runtime.execute_provider_admissions().unwrap().ready(),
        "the selected capability receipt is immutable"
    );
    assert!(
        !runtime.execute().unwrap().complete(),
        "late usage must block business dispatch"
    );
    let execution = runtime.projection().unwrap().execution.unwrap();
    for (node, output) in selected {
        assert_eq!(execution.outputs.get(&node), Some(&output));
    }
    assert!(host.selected_attempt_evidence().unwrap().is_empty());
    assert_eq!(execution.budget.committed_tokens(), u128::from(u64::MAX));
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert!(execution.budget.breached());
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn incomplete_billing_on_captured_reviewers_never_publishes_a_selected_result() {
    struct Malformed(AtomicUsize);
    impl WorkerModelAdapter for Malformed {
        fn provider_kind(&self) -> &'static str {
            "claude"
        }
        fn model_settings(&self) -> Option<(String, String)> {
            Some(("claude-fixture".into(), "high".into()))
        }
        fn invoke(
            &self,
            cas: &Cas,
            _: &std::path::Path,
            input: Vec<u8>,
            _: std::time::Duration,
            writable: bool,
        ) -> ModelWorkerReturn {
            assert!(!writable);
            let call = self.0.fetch_add(1, Ordering::SeqCst);
            assert!(call <= 2);
            if call == 0 {
                assert_eq!(input, b"Reply with exactly: OK\n");
            } else {
                assert!(
                    String::from_utf8(input)
                        .unwrap()
                        .contains("Captured instruction marker.")
                );
            }
            let usage = review_core::task::usage::TaskTokenUsageV3::charge_only(if call == 0 {
                1
            } else {
                11
            });
            ModelWorkerReturn {
                usage_observation: (call > 0).then(|| {
                    review_core::task::usage::TaskUsageObservationV1 {
                        reported_usage: Some(usage.clone()),
                        charge_complete: false,
                    }
                }),
                usage: Some(usage),
                message: if call == 0 {
                    Ok(b"OK".to_vec())
                } else {
                    Err("malformed native billing counter".into())
                },
                raw_artifact_ids: vec![cas.put(b"native usage fixture").unwrap()],
            }
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let (compiler, lease, plan) = admitted_plan(&cas, &mut store, "trusted_unsafe");
    let model = Malformed(AtomicUsize::new(0));
    let models = plan
        .bindings
        .iter()
        .map(|(slot, binding)| {
            (
                slot.clone(),
                TaskModelBinding {
                    binding: binding.clone(),
                    adapter: &model as &dyn WorkerModelAdapter,
                },
            )
        })
        .collect();
    let shared = SharedEventStore::new(&mut store);
    let host =
        LegacyReviewTaskHost::new(&cas, shared.clone(), &compiler, lease.clone(), models).unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    assert!(!runtime.execute().unwrap().complete());
    let state = runtime.projection().unwrap();
    let execution = state.execution.unwrap();
    let reviewer_node=execution.graph.nodes.iter().find(|(_,node)|matches!(&node.operator,review_graph::task::CompiledOperator::ReviewDomain{review_node,..} if review_node=="reviewer")).unwrap().0;
    let reviewers: Vec<_> = execution
        .attempt_accounting()
        .into_iter()
        .filter(|row| &row.reservation.node == reviewer_node)
        .collect();
    assert!(!reviewers.is_empty());
    for row in &reviewers {
        assert_eq!(row.charged_tokens, u128::from(row.reservation.tokens));
        assert!(matches!(
            row.result,
            Some(review_core::task::execution::TaskAttemptResultV1::Failed { .. })
        ));
        let run = review_store::store::task::task_run_id(lease.task_id()).unwrap();
        let observation = shared
            .lock()
            .unwrap()
            .task_attempt_usage_observation(&run, &row.attempt_id)
            .unwrap()
            .unwrap();
        assert!(!observation.charge_complete);
        assert_eq!(
            observation.reported_usage.unwrap().chargeable_tokens.get(),
            11
        );
    }
    assert!(!execution.outputs.contains_key(reviewer_node));
    assert!(
        !host
            .publish_recorded_round_conclusion(&cas)
            .unwrap()
            .can_continue
    );
}
