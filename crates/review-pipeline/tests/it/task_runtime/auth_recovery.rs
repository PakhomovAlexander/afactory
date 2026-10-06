//! ADR-0141 across the runtime, Store and production verification probe: a typed native auth
//! failure suspends the Task before terminalization, a stale "authenticated" status cannot
//! pass for recovery, and only a verified claim continues the original work exactly once.

use super::*;
use review_core::task::auth_recovery::*;
use review_pipeline::task::recovery::*;
use review_runner::native_failure::NativeFailureKind;
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
use review_store::store::task::auth_recovery::{AuthRecoveryStatus, auth_context_key};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const PROBE: &[u8] = b"Reply with exactly: OK\n";

/// A native client whose credentials are revoked until the fixture "logs in". Failures carry
/// only the closed classification a real adapter derives from the native protocol.
struct Native {
    logged_in: AtomicBool,
    /// Contended refreshes still to report, per call kind, before the client recovers.
    contended_probe: AtomicUsize,
    contended_worker: AtomicUsize,
    quota: AtomicBool,
    calls: Mutex<Vec<&'static str>>,
}

impl Native {
    fn new() -> Self {
        Self {
            logged_in: AtomicBool::new(false),
            contended_probe: AtomicUsize::new(0),
            contended_worker: AtomicUsize::new(0),
            quota: AtomicBool::new(false),
            calls: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

impl WorkerModelAdapter for Native {
    fn provider_kind(&self) -> &'static str {
        "fixture"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some(("typed-model".into(), "high".into()))
    }
    fn invoke(
        &self,
        cas: &Cas,
        _: &std::path::Path,
        input: Vec<u8>,
        _: std::time::Duration,
        _: review_runner::task::WorkerAccess,
        _: Option<&std::sync::atomic::AtomicBool>,
        _: &[(String, String)],
    ) -> ModelWorkerReturn {
        let probe = input == PROBE;
        self.calls
            .lock()
            .unwrap()
            .push(if probe { "probe" } else { "worker" });
        let failure = if self.quota.load(Ordering::SeqCst) {
            Some(NativeFailureKind::Quota)
        } else if if probe {
            &self.contended_probe
        } else {
            &self.contended_worker
        }
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
        {
            Some(NativeFailureKind::AuthRefreshContended)
        } else if !self.logged_in.load(Ordering::SeqCst) {
            Some(NativeFailureKind::AuthRevoked)
        } else {
            None
        };
        if let Some(kind) = failure {
            return ModelWorkerReturn {
                native_failure: Some(kind),
                usage_observation: None,
                raw_artifact_ids: vec![],
                message: Err(format!("Fixture Worker failed: {}", kind.diagnostic())),
                usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(3)),
            };
        }
        let (bytes, cost) = if probe {
            (b"OK".to_vec(), 7)
        } else {
            (
                serde_json::to_vec(&json!({"schema":"af.worker-reply/1",
                    "outputs":{"output":[{"outcome":"passed","text":"Checked document"}]}}))
                .unwrap(),
                11,
            )
        };
        ModelWorkerReturn {
            native_failure: None,
            usage_observation: None,
            raw_artifact_ids: vec![cas.put(&bytes).unwrap()],
            message: Ok(bytes),
            usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(
                cost,
            )),
        }
    }
}

/// The machine side: token-free status and the binding it would dispatch with now.
struct Machine {
    authenticated: AtomicBool,
    model: Mutex<Option<String>>,
}

impl AuthContextHost for Machine {
    fn current(&self, required: &TaskAuthBindingV1) -> Result<TaskAuthBindingV1, String> {
        let mut current = required.clone();
        if let Some(model) = self.model.lock().unwrap().clone() {
            current.model = model;
        }
        Ok(current)
    }
    fn authenticated(&self, _: &TaskAuthBindingV1) -> Result<bool, String> {
        Ok(self.authenticated.load(Ordering::SeqCst))
    }
}

fn recoverable() -> Fixture {
    let mut f = Fixture::with_model("unused", true);
    f.task.limits.max_attempts = 12;
    f.task.limits.verification.tokens = 1100;
    f.task.limits.verification.attempts = 2;
    f.task.limits.verification.wall_ms = 6000;
    f.revision_id = f
        .cas
        .put_artifact(
            TASK_REVISION_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&f.task).unwrap(),
        )
        .unwrap()
        .0;
    f.compiler = f
        .compiler
        .with_provider_admission(OperatorAttemptCost {
            tokens: 100,
            wall_ms: 1000,
        })
        .with_auth_recovery(AuthRecoveryAllowanceV1 {
            probes: 2,
            tokens_per_probe: 50,
            wall_ms_per_probe: 5000,
        })
        .unwrap();
    (f.plan, f.graph) = f
        .compiler
        .compile(&f.cas, &f.revision_id, "builtin/document")
        .unwrap();
    f.plan_id = put_plan(&f, &f.plan.clone());
    f
}

fn put_plan(f: &Fixture, plan: &ExecutionPlanV1) -> String {
    f.cas
        .put_artifact(
            EXECUTION_PLAN_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(plan).unwrap(),
        )
        .unwrap()
        .0
}

/// Everything one Task needs to run under the production host, authority and probe.
macro_rules! captured {
    ($f:expr, $native:expr => $models:ident, $domain:ident, $host:ident, $authority:ident) => {
        let $models: BTreeMap<String, TaskModelBinding<'_>> = $f
            .plan
            .bindings
            .iter()
            .map(|(slot, binding)| {
                (
                    slot.clone(),
                    TaskModelBinding {
                        binding: binding.clone(),
                        adapter: $native as &dyn WorkerModelAdapter,
                    },
                )
            })
            .collect();
        let $domain = review_pipeline::task::provider::ProviderTaskDomain {
            graph: &$f.graph,
            models: &$models,
            inner: &DocumentDomain,
        };
        let $host = CapturedTaskHost::capture_with_models(
            &$f.cas,
            &$f.compiler,
            &$f.task,
            &$f.plan,
            $f.graph.clone(),
            &EmptyTaskEnvironment,
            &$domain,
            &$models,
        )
        .unwrap();
        let $authority = CapturedTaskAuthority::new(&$f.compiler, &$host, &NoTaskDeveloper);
    };
}

fn admitted(
    store: &mut EventStore,
    cas: &Cas,
    revision_id: &str,
    plan_id: &str,
    authority: &dyn review_store::store::task::TaskAuthority,
) -> review_store::store::task::TaskLease {
    let lease = store.open_task(cas, revision_id, "writer", 60_000).unwrap();
    store
        .propose_task_plan(cas, &lease, plan_id, authority)
        .unwrap();
    store.admit_task_plan(cas, &lease, authority).unwrap();
    lease
}

fn ledger(f: &Fixture, task: &str) -> (u128, u64) {
    let execution = f
        .store
        .task_projection(&f.cas, task)
        .unwrap()
        .unwrap()
        .execution
        .unwrap();
    (
        execution.budget.committed_tokens(),
        execution.budget.begun_attempts(),
    )
}

fn phase(f: &Fixture, task: &str) -> TaskPhaseV1 {
    f.store
        .task_projection(&f.cas, task)
        .unwrap()
        .unwrap()
        .phase
}

const SUSPENDED: TaskPhaseV1 = TaskPhaseV1::Waiting {
    reason: TaskWaitingReasonV1::NeedsProviderAuth,
};

#[test]
fn a_stale_authenticated_status_cannot_recover_and_a_verified_login_resumes_exactly_once() {
    let native = Native::new();
    let mut f = recoverable();
    let (revision_id, plan_id) = (f.revision_id.clone(), f.plan_id.clone());
    captured!(f, &native => models, domain, host, authority);
    let lease = admitted(&mut f.store, &f.cas, &revision_id, &plan_id, &authority);
    let task = lease.task_id().to_owned();
    let who = TaskAuthParticipantV1 {
        requester_ref: "user-1".into(),
        coordinator_ref: "chat-1".into(),
    };
    let report = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host)
        .unwrap()
        .with_auth_participant(who.clone())
        .execute()
        .unwrap();
    assert!(!report.complete());
    // The failing admission stopped everything: no business Worker and no terminal result.
    assert_eq!(native.calls(), vec!["probe"]);
    assert_eq!(phase(&f, &task), SUSPENDED);
    assert_eq!(ledger(&f, &task), (3, 1));
    let state = f.store.task_projection(&f.cas, &task).unwrap().unwrap();
    let (_, suspension) = state.auth.active_suspension().unwrap();
    assert_eq!(
        suspension.failures[0].failure,
        TaskAuthFailureV1::AuthRevoked
    );
    assert_eq!(suspension.participant.as_ref(), Some(&who));
    let key = suspension.contexts.keys().next().unwrap().clone();

    let machine = Machine {
        authenticated: AtomicBool::new(false),
        model: Mutex::new(None),
    };
    let probe = ModelAuthProbe { models: &models };
    // Missing auth: the private handoff, with no paid call.
    assert_eq!(
        verify_and_claim(
            &Mutex::new(&mut f.store),
            &f.cas,
            &lease,
            &authority,
            &machine,
            &probe,
            None
        )
        .unwrap(),
        AuthRecoveryStep::LoginRequired {
            context_keys: vec![key.clone()]
        }
    );
    assert_eq!(native.calls().len(), 1);
    assert_eq!(ledger(&f, &task), (3, 1));
    // A fake "authenticated" status whose real inference is still revoked: one bounded probe,
    // charged on the Task ledger, and still the private handoff, never a setup success.
    machine.authenticated.store(true, Ordering::SeqCst);
    assert_eq!(
        verify_and_claim(
            &Mutex::new(&mut f.store),
            &f.cas,
            &lease,
            &authority,
            &machine,
            &probe,
            None
        )
        .unwrap(),
        AuthRecoveryStep::LoginRequired {
            context_keys: vec![key.clone()]
        }
    );
    assert_eq!(ledger(&f, &task), (6, 2));
    assert_eq!(
        f.store.provider_auth_recovery(&key).unwrap().status,
        AuthRecoveryStatus::Failed
    );
    assert_eq!(phase(&f, &task), SUSPENDED);
    // A changed model is drift: blocked before any call.
    *machine.model.lock().unwrap() = Some("another-model".into());
    assert_eq!(
        verify_and_claim(
            &Mutex::new(&mut f.store),
            &f.cas,
            &lease,
            &authority,
            &machine,
            &probe,
            None
        )
        .unwrap(),
        AuthRecoveryStep::Blocked {
            reason: TaskAuthBlockV1::BindingChanged
        }
    );
    *machine.model.lock().unwrap() = None;
    assert_eq!(ledger(&f, &task), (6, 2));

    // Stage 1 completes a private login: recorded as unverified, then verified by a probe.
    f.store.record_provider_auth_login(&key, "login-1").unwrap();
    native.logged_in.store(true, Ordering::SeqCst);
    let step = verify_and_claim(
        &Mutex::new(&mut f.store),
        &f.cas,
        &lease,
        &authority,
        &machine,
        &probe,
        None,
    )
    .unwrap();
    let AuthRecoveryStep::Resumed {
        replayed: false, ..
    } = step
    else {
        panic!("{step:?}")
    };
    assert_eq!(ledger(&f, &task), (13, 3));
    assert_eq!(phase(&f, &task), TaskPhaseV1::Running {});

    // Continuation is the original Task: the admission retries under the captured allowance
    // and the business Worker runs once.
    f.store.recover_task_attempts(&f.cas, &lease).unwrap();
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    drop(runtime);
    assert_eq!(
        native.calls(),
        vec!["probe", "probe", "probe", "probe", "worker"]
    );
    assert_eq!(ledger(&f, &task), (3 + 3 + 7 + 7 + 11, 5));
    // An interrupted host that retries recovery finishes the same claim and runs nothing.
    let again = verify_and_claim(
        &Mutex::new(&mut f.store),
        &f.cas,
        &lease,
        &authority,
        &machine,
        &probe,
        None,
    )
    .unwrap();
    assert!(matches!(
        again,
        AuthRecoveryStep::Resumed { replayed: true, .. }
    ));
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    drop(runtime);
    assert_eq!(native.calls().len(), 5);
    assert_eq!(ledger(&f, &task), (31, 5));

    // The originating coordinator hears "login required" once and "resumed" once.
    let outcomes = f.store.task_auth_outcomes(&f.cas, &task).unwrap();
    let told: Vec<_> = outcomes
        .iter()
        .map(|o| {
            (
                o.outcome.clone(),
                o.participant.clone().unwrap().coordinator_ref,
            )
        })
        .collect();
    assert_eq!(
        told,
        vec![
            (TaskAuthOutcomeV1::LoginRequired {}, "chat-1".to_string()),
            (
                TaskAuthOutcomeV1::Blocked {
                    reason: TaskAuthBlockV1::BindingChanged
                },
                "chat-1".to_string()
            ),
            (TaskAuthOutcomeV1::Resumed {}, "chat-1".to_string()),
        ]
    );
    for outcome in &outcomes {
        assert!(
            f.store
                .acknowledge_task_auth_outcome(&key, outcome.sequence, "chat-1", "message-1")
                .unwrap()
        );
    }
    // No recovery row carries native text, a URL or credential material.
    let rows = serde_json::to_string(
        &f.store
            .provider_auth_recoveries()
            .unwrap()
            .iter()
            .map(|r| format!("{r:?}"))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    for needle in ["https://", "token", "code="] {
        assert!(!rows.contains(needle), "{needle}");
    }
}

/// Point the fixture at one Task's own revision and recompiled plan.
fn select(f: &mut Fixture, task: &str) {
    f.task.task_id = task.into();
    f.revision_id = f
        .cas
        .put_artifact(
            TASK_REVISION_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&f.task).unwrap(),
        )
        .unwrap()
        .0;
    (f.plan, f.graph) = f
        .compiler
        .compile(&f.cas, &f.revision_id, "builtin/document")
        .unwrap();
    f.plan_id = put_plan(f, &f.plan.clone());
}

#[test]
fn concurrent_failures_sharing_a_provider_need_one_login_and_one_verification() {
    let native = Native::new();
    let mut f = recoverable();
    let mut leases = BTreeMap::new();
    for task in ["task-a", "task-b"] {
        select(&mut f, task);
        let (revision_id, plan_id) = (f.revision_id.clone(), f.plan_id.clone());
        captured!(f, &native => models, domain, host, authority);
        let _ = &models;
        let lease = admitted(&mut f.store, &f.cas, &revision_id, &plan_id, &authority);
        TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host)
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(phase(&f, task), SUSPENDED);
        leases.insert(task, lease);
    }
    let recoveries = f.store.provider_auth_recoveries().unwrap();
    assert_eq!(recoveries.len(), 1, "one auth context, one recovery link");
    assert_eq!(recoveries[0].generation, 1);
    assert_eq!(recoveries[0].participants.len(), 2);
    native.logged_in.store(true, Ordering::SeqCst);
    let machine = Machine {
        authenticated: AtomicBool::new(true),
        model: Mutex::new(None),
    };
    for task in ["task-a", "task-b"] {
        select(&mut f, task);
        captured!(f, &native => models, domain, host, authority);
        let _ = &host;
        let probe = ModelAuthProbe { models: &models };
        let step = verify_and_claim(
            &Mutex::new(&mut f.store),
            &f.cas,
            &leases[task],
            &authority,
            &machine,
            &probe,
            None,
        )
        .unwrap();
        assert!(matches!(step, AuthRecoveryStep::Resumed { .. }), "{step:?}");
    }
    // Two failed admissions, one verification probe (on the first Task's own ledger).
    assert_eq!(native.calls(), vec!["probe", "probe", "probe"]);
    assert_eq!(ledger(&f, "task-a"), (3 + 7, 2));
    assert_eq!(ledger(&f, "task-b"), (3, 1));
    // A different Provider label is a different auth context with its own recovery.
    let mut other = TaskAuthBindingV1::of_plan_binding(f.plan.bindings.values().next().unwrap())
        .unwrap()
        .context;
    let shared = auth_context_key(&other).unwrap();
    other.provider = "other".into();
    assert_ne!(auth_context_key(&other).unwrap(), shared);
    assert_eq!(
        f.store
            .provider_auth_recovery(&auth_context_key(&other).unwrap())
            .unwrap()
            .status,
        AuthRecoveryStatus::Healthy
    );
}

#[test]
fn quota_failures_never_suspend_or_ask_for_a_login() {
    let native = Native::new();
    native.logged_in.store(true, Ordering::SeqCst);
    native.quota.store(true, Ordering::SeqCst);
    let mut f = recoverable();
    let (revision_id, plan_id) = (f.revision_id.clone(), f.plan_id.clone());
    captured!(f, &native => models, domain, host, authority);
    let _ = &models;
    let lease = admitted(&mut f.store, &f.cas, &revision_id, &plan_id, &authority);
    let report = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host)
        .unwrap()
        .execute()
        .unwrap();
    assert!(!report.complete());
    assert_eq!(phase(&f, lease.task_id()), TaskPhaseV1::Running {});
    assert!(f.store.provider_auth_recoveries().unwrap().is_empty());
    let state = f
        .store
        .task_projection(&f.cas, lease.task_id())
        .unwrap()
        .unwrap();
    assert!(state.auth.suspensions.is_empty());
}

#[test]
fn transient_contention_is_retried_within_its_allowance_without_suspending() {
    let native = Native::new();
    native.logged_in.store(true, Ordering::SeqCst);
    native.contended_worker.store(1, Ordering::SeqCst);
    let mut f = recoverable();
    let (revision_id, plan_id) = (f.revision_id.clone(), f.plan_id.clone());
    captured!(f, &native => models, domain, host, authority);
    let _ = &models;
    let lease = admitted(&mut f.store, &f.cas, &revision_id, &plan_id, &authority);
    let report = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host)
        .unwrap()
        .execute()
        .unwrap();
    assert!(report.complete(), "{report:?}");
    assert_eq!(native.calls(), vec!["probe", "worker", "worker"]);
    assert_eq!(ledger(&f, lease.task_id()), (7 + 3 + 11, 3));
    assert!(f.store.provider_auth_recoveries().unwrap().is_empty());
}

#[test]
fn persistent_contention_suspends_and_recovers_without_any_login() {
    let native = Native::new();
    native.logged_in.store(true, Ordering::SeqCst);
    native.contended_probe.store(1, Ordering::SeqCst);
    let mut f = recoverable();
    let (revision_id, plan_id) = (f.revision_id.clone(), f.plan_id.clone());
    captured!(f, &native => models, domain, host, authority);
    let lease = admitted(&mut f.store, &f.cas, &revision_id, &plan_id, &authority);
    let task = lease.task_id().to_owned();
    // Admission is one paid probe per run, so its contention suspends instead of retrying.
    let report = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host)
        .unwrap()
        .execute()
        .unwrap();
    assert!(!report.complete());
    assert_eq!(phase(&f, &task), SUSPENDED);
    let state = f.store.task_projection(&f.cas, &task).unwrap().unwrap();
    let (_, suspension) = state.auth.active_suspension().unwrap();
    assert_eq!(
        suspension.failures[0].failure,
        TaskAuthFailureV1::AuthRefreshContended
    );
    let key = suspension.contexts.keys().next().unwrap().clone();
    let machine = Machine {
        authenticated: AtomicBool::new(true),
        model: Mutex::new(None),
    };
    let probe = ModelAuthProbe { models: &models };
    let step = verify_and_claim(
        &Mutex::new(&mut f.store),
        &f.cas,
        &lease,
        &authority,
        &machine,
        &probe,
        None,
    )
    .unwrap();
    assert!(matches!(step, AuthRecoveryStep::Resumed { .. }), "{step:?}");
    assert!(
        f.store
            .provider_auth_recovery(&key)
            .unwrap()
            .login_refs
            .is_empty()
    );
    f.store.recover_task_attempts(&f.cas, &lease).unwrap();
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    drop(runtime);
    assert_eq!(native.calls(), vec!["probe", "probe", "probe", "worker"]);
    assert_eq!(ledger(&f, &task), (3 + 7 + 7 + 11, 4));
}
