use super::*;
use review_core::task::plan::{ExecutionPlanV1, WorkerExecutionV1};
use review_pipeline::task::host::TaskModelBinding;
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Model {
    calls: AtomicUsize,
    admitted: bool,
    retry: bool,
}
impl WorkerModelAdapter for Model {
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
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let (message, usage) = if n == 0 {
            assert!(!writable);
            assert_eq!(input, b"Reply with exactly: OK\n");
            (if self.admitted { "OK" } else { "unavailable" }, 1)
        } else {
            assert!(self.admitted);
            assert!(writable);
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
        ModelWorkerReturn {
            message: Ok(message.as_bytes().to_vec()),
            usage: Some(review_runner::TokenUsage::charge_only(usage)),
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
    let definition = PIPELINE
        .replace("version = 2", "version = 4\n[gate]\nprovider=\"trusted_local\"\nrequired_isolation=\"none\"\nmode=\"ephemeral-write\"")
        .replace("runner = { program = \"/bin/true\" }", &format!("package=\"fixture\"\ngated_by=\"gate\"\nexecution={{credential_mode=\"{mode}\"}}"))
        + "\n[[nodes]]\nid=\"gate\"\nkind=\"gate\"\noutputs=[\"decision\"]\n[[checks]]\nname=\"required\"\nprogram=\"/bin/sh\"\nargs=[{value=\"-c\"},{value=\"exit 0\"}]\n";
    let round = capture::open_round_with_package(cas, store, &definition);
    let mut settings = plan::settings();
    settings.provider_admission.tokens = 32;
    settings.executions.insert(
        "reviewer".into(),
        WorkerExecutionV1::Model {
            provider: "claude-personal".into(),
            provider_kind: "claude".into(),
            principal_id: "fixture-personal-account".into(),
            model: "claude-fixture".into(),
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
    for (admitted, retry) in [(true, false), (true, true), (false, false)] {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let (compiler, lease, plan) = admitted_plan(&cas, &mut store, "trusted_unsafe");
        let model = Model {
            calls: AtomicUsize::new(0),
            admitted,
            retry,
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
            calls: AtomicUsize::new(0),
            admitted: true,
            retry: false,
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
