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
        let mut usage = review_runner::TokenUsage::charge_only(usage);
        if self.wide && n > 0 {
            usage.input_tokens = Some(u64::MAX);
        }
        ModelWorkerReturn {
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
    admitted_plan_with_provider(cas, store, mode, "claude")
}

fn admitted_plan_with_provider(
    cas: &Cas,
    store: &mut EventStore,
    mode: &str,
    provider_kind: &str,
) -> (
    LegacyReviewPlanCompiler,
    review_store::store::task::TaskLease,
    ExecutionPlanV1,
) {
    let definition = PIPELINE
        .replace("version = 2", "version = 4\n[gate]\nprovider=\"trusted_local\"\nrequired_isolation=\"none\"\nmode=\"ephemeral-write\"")
        .replace("runner = { program = \"/bin/true\" }", &format!("package=\"fixture\"\ngated_by=\"gate\"\nexecution={{credential_mode=\"{mode}\"}}"))
        + "\n[[nodes]]\nid=\"gate\"\nkind=\"gate\"\noutputs=[\"decision\"]\n[[checks]]\nname=\"required\"\nprogram=\"/bin/sh\"\nargs=[{value=\"-c\"},{value=\"exit 0\"}]\n";
    let round = if provider_kind == "claude" {
        capture::open_round_with_package(cas, store, &definition)
    } else {
        assert_eq!(provider_kind, "codex");
        captured_fixture::open_round_authority(cas, store, &definition, Some(BTreeMap::from([
            ("reviewer.toml".into(), b"name=\"fixture\"\nversion=\"1.0.0\"\nsubjects=[\"whole-tree\"]\n[runner]\nprogram=\"codex\"\nargs=[{value=\"--model\"},{value=\"codex-fixture\"},{value=\"-c\"},{value=\"model_reasoning_effort=high\"}]\n".to_vec()),
            ("reviewer.md".into(), b"Review the exact declared Subject. Captured instruction marker.".to_vec()),
        ])))
    };
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
        settings,
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
            admitted_plan_with_provider(&cas, &mut store, "trusted_unsafe", provider_kind);
        let model = Model {
            provider_kind,
            calls: AtomicUsize::new(0),
            admitted,
            retry,
            wide,
        };
        let shared = SharedEventStore::new(&mut store);
        for _ in 0..2 {
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
                assert_eq!(frame.artifact_type, TASK_REVIEW_ATTEMPT_PROVENANCE_V1);
                let provenance: TaskReviewAttemptProvenanceV1 =
                    serde_json::from_value(frame.payload).unwrap();
                assert_eq!(provenance.charged_tokens.get(), 11);
                let usage = cas
                    .get_artifact(provenance.usage_id.as_ref().unwrap())
                    .unwrap();
                assert_eq!(usage.payload["chargeable_tokens"], "11");
                if wide {
                    assert_eq!(usage.payload["input_tokens"], u64::MAX.to_string());
                }
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
        "version",
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
            "version" => candidate.event_type = EventType::RunReportV4,
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
