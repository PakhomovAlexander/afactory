//! Runtime Provider auth recovery on the common Store (ADR-0141). Each test names the
//! regression it pins; all of them are credential-free and deterministic.

use super::*;
use crate::store::task::auth_recovery::*;
use review_attempt::task_budget::NodeAllowance;
use review_core::task::auth_recovery::*;
use review_core::task::execution::{TaskAttemptResultV1, TaskExecutionRecordV1};
use review_core::task::plan::{EffectiveWorkerBindingV1, WorkerExecutionV1};
use review_core::task::{TaskExecutionV1, VerificationReserveV1};
use review_graph::task::CompiledTask;

const WRITE: &str = "root.nodes.write";
use TaskAuthFailureV1::{AuthRefreshContended, AuthRevoked};

fn put<T: serde::Serialize>(cas: &Cas, kind: &str, value: &T) -> String {
    cas.put_artifact(
        kind,
        producer(),
        vec![],
        None,
        serde_json::to_value(value).unwrap(),
    )
    .unwrap()
    .0
}

struct Setup {
    tokens: u64,
    write_tokens: u64,
    probes: Option<(u32, u64)>,
    deadline_in_ms: u64,
    generated: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            tokens: 1000,
            write_tokens: 100,
            probes: Some((2, 100)),
            deadline_in_ms: 1_000_000,
            generated: false,
        }
    }
}

fn model(f: &Fixture, provider: &str, kind: &str, principal: &str) -> EffectiveWorkerBindingV1 {
    EffectiveWorkerBindingV1 {
        package_digest: f.revision.authority.policy_id.clone(),
        package_artifact_id: f.revision.authority.policy_id.clone(),
        execution: WorkerExecutionV1::Model {
            provider: provider.into(),
            provider_kind: kind.into(),
            principal_id: principal.into(),
            model: "fixture-model".into(),
            effort: "high".into(),
        },
        invocation_policy_id: f.revision.authority.policy_id.clone(),
    }
}

/// The source fixture, with exact limits, a write node that can spend `write_tokens` and a
/// captured recovery allowance. Two Model slots bind two distinct auth contexts.
fn recoverable(setup: Setup) -> Fixture {
    let mut f = super::source::fixture(setup.generated);
    f.revision.limits = task::TaskLimitsV1 {
        tokens: setup.tokens,
        max_attempts: 10,
        deadline_unix_ms: now().unwrap() + setup.deadline_in_ms,
        verification: VerificationReserveV1 {
            tokens: 0,
            attempts: 0,
            wall_ms: 0,
        },
    };
    f.revision_id = put(&f.cas, task::TASK_REVISION_V1, &f.revision);
    let mut graph: CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.allowances.insert(
        WRITE.into(),
        NodeAllowance {
            tokens_per_attempt: setup.write_tokens,
            wall_ms_per_attempt: 100,
            max_attempts: 3,
            verification_attempts: 0,
        },
    );
    graph.auth_recovery = setup
        .probes
        .map(|(probes, tokens)| AuthRecoveryAllowanceV1 {
            probes,
            tokens_per_probe: tokens,
            wall_ms_per_probe: 100,
        });
    f.plan.compiled_graph_id = put(&f.cas, "af/CompiledTask@1", &graph);
    f.plan.task_revision_id = f.revision_id.clone();
    f.plan.limits = f.revision.limits.clone();
    f.plan.bindings = BTreeMap::from([
        ("author".into(), model(&f, "work", "claude", "principal-a")),
        (
            "reviewer".into(),
            model(&f, "other", "codex", "principal-b"),
        ),
    ]);
    f.plan_id = put(&f.cas, task::EXECUTION_PLAN_V1, &f.plan);
    f.authority.result_allowed = true;
    f
}

fn binding(f: &Fixture, slot: &str) -> TaskAuthBindingV1 {
    TaskAuthBindingV1::of_plan_binding(&f.plan.bindings[slot]).unwrap()
}

fn key(binding: &TaskAuthBindingV1) -> String {
    auth_context_key(&binding.context).unwrap()
}

fn state(f: &Fixture, task: &str) -> TaskProjection {
    f.store.task_projection(&f.cas, task).unwrap().unwrap()
}

fn participant(requester: &str, coordinator: &str) -> TaskAuthParticipantV1 {
    TaskAuthParticipantV1 {
        requester_ref: requester.into(),
        coordinator_ref: coordinator.into(),
    }
}

/// Open, plan and admit one Task of this fixture's request under its own ID and plan.
fn start(f: &mut Fixture, task_id: &str) -> TaskLease {
    let mut revision = f.revision.clone();
    revision.task_id = task_id.into();
    let revision_id = put(&f.cas, task::TASK_REVISION_V1, &revision);
    let mut plan = f.plan.clone();
    plan.task_revision_id = revision_id.clone();
    let plan_id = put(&f.cas, task::EXECUTION_PLAN_V1, &plan);
    let lease = f
        .store
        .open_task(&f.cas, &revision_id, "writer-1", 1_000_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &plan_id, &f.authority)
        .unwrap();
    if plan.requires_developer_approval() {
        f.store
            .decide_task_plan(
                &f.cas,
                &lease,
                &plan_id,
                PlanDecisionKindV1::Approved,
                "Inspected exact plan",
                &f.authority,
            )
            .unwrap();
    }
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let (saved_revision, saved_revision_id, saved_plan_id) = (
        std::mem::replace(&mut f.revision, revision),
        std::mem::replace(&mut f.revision_id, revision_id),
        std::mem::replace(&mut f.plan_id, plan_id),
    );
    f.record_execution_inputs(&lease);
    f.revision = saved_revision;
    f.revision_id = saved_revision_id;
    f.plan_id = saved_plan_id;
    lease
}

fn attempt_on(f: &mut Fixture, lease: &TaskLease) -> execution::PreparedTaskAttempt {
    let context = f
        .cas
        .put_json(&json!({"purpose":"auth recovery fixture", "nonce": now().unwrap()}))
        .unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, lease, WRITE, &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, lease, &attempt, &f.authority)
        .unwrap();
    attempt
}

/// One started write Attempt failing authentication with this exact charge.
fn fail(f: &mut Fixture, lease: &TaskLease, charge: u128) -> String {
    let attempt = attempt_on(f, lease);
    let diagnostic_id = f
        .cas
        .put_json(&json!({"schema":"af.task-diagnostic/1","error":"Provider credentials were revoked (auth_revoked)"}))
        .unwrap();
    f.store
        .settle_task_attempt(
            &f.cas,
            lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: charge,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id,
                    feedback_id: None,
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
    attempt.id().into()
}

fn suspend(
    f: &mut Fixture,
    lease: &TaskLease,
    failed: &str,
    failure: TaskAuthFailureV1,
    slots: &[&str],
    who: Option<TaskAuthParticipantV1>,
) -> (String, TaskAuthSuspensionV1) {
    let contexts: Vec<_> = slots
        .iter()
        .map(|slot| (binding(f, slot), Some(failure)))
        .collect();
    f.store
        .suspend_task_for_provider_auth(&f.cas, lease, &[(failed.into(), failure)], &contexts, who)
        .unwrap()
}

/// Reserve, start and settle one probe for this context.
fn probe(
    f: &mut Fixture,
    lease: &TaskLease,
    slot: &str,
    charge: u128,
    outcome: TaskAuthProbeOutcomeV1,
) -> Result<TaskAuthProbeSettlement, StoreError> {
    let current = binding(f, slot);
    let mut probe =
        f.store
            .reserve_task_auth_probe(&f.cas, lease, &key(&current), &current, &f.authority)?;
    f.store
        .start_task_auth_probe(&f.cas, lease, &mut probe, &current, &f.authority)?;
    f.store
        .settle_task_auth_probe(&f.cas, lease, &probe, charge, outcome, None)
}

fn acknowledged() -> TaskAuthProbeOutcomeV1 {
    TaskAuthProbeOutcomeV1::Acknowledged {}
}

/// Wait until this policy time has passed: loaded machines never race a fixed sleep.
fn sleep_past(unix_ms: u64) {
    while now().unwrap() <= unix_ms {
        std::thread::sleep(std::time::Duration::from_millis(
            (unix_ms + 1).saturating_sub(now().unwrap()).clamp(1, 200),
        ));
    }
}

fn budget(f: &Fixture, task: &str) -> (u128, u128, u64) {
    let state = state(f, task);
    let budget = &state.execution.as_ref().unwrap().budget;
    (
        budget.committed_tokens(),
        budget.reserved_tokens(),
        budget.begun_attempts(),
    )
}

fn claimed(result: TaskAuthClaim) -> (String, bool) {
    match result {
        TaskAuthClaim::Claimed {
            claim_id, replayed, ..
        } => (claim_id, replayed),
        other => panic!("expected a claim, got {other:?}"),
    }
}

#[test]
fn suspension_retains_the_failed_attempt_and_pauses_before_terminalization() {
    let mut f = recoverable(Setup::default());
    let lease = start(&mut f, "task-1");
    let failed = fail(&mut f, &lease, 37);
    let (id, suspension) = suspend(
        &mut f,
        &lease,
        &failed,
        AuthRevoked,
        &["author"],
        Some(participant("user-1", "chat-1")),
    );
    f.store = EventStore::open(&f.path).unwrap();
    let task = state(&f, "task-1");
    assert_eq!(
        task.phase,
        TaskPhaseV1::Waiting {
            reason: TaskWaitingReasonV1::NeedsProviderAuth
        }
    );
    assert_eq!(task.auth.active_suspension().unwrap().0, id);
    let failure = &suspension.failures[0];
    assert_eq!(
        (failure.attempt_id.as_str(), failure.charged_tokens.get()),
        (failed.as_str(), 37)
    );
    assert_eq!(budget(&f, "task-1"), (37, 0, 1));
    // A plain resume, a generic wait and a satisfied result cannot bypass the suspension.
    assert!(
        f.store
            .resume_task(&f.cas, &lease, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("verified claim")
    );
    assert!(
        f.store
            .reserve_task_attempt(&f.cas, &lease, WRITE, &f.authority)
            .is_err()
    );
    let recovery = f
        .store
        .provider_auth_recovery(&key(&binding(&f, "author")))
        .unwrap();
    assert_eq!(
        (recovery.generation, recovery.status),
        (1, AuthRecoveryStatus::Failed)
    );
    // Nothing secret can be stored: every recovery row is a closed, typed event.
    let rows: Vec<String> = f
        .store
        .conn
        .prepare("SELECT payload FROM provider_auth_recovery")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].contains("https://") && !rows[0].contains("token"));
}

/// prior-1: probes and live reservations consume the same original Task allowance.
#[test]
fn verification_probes_consume_the_original_task_ledger_without_phantom_headroom() {
    let mut f = recoverable(Setup {
        tokens: 1000,
        write_tokens: 900,
        probes: Some((2, 100)),
        ..Setup::default()
    });
    let lease = start(&mut f, "task-1");
    let failed = fail(&mut f, &lease, 900);
    suspend(&mut f, &lease, &failed, AuthRevoked, &["author"], None);
    let current = binding(&f, "author");
    let mut first = f
        .store
        .reserve_task_auth_probe(&f.cas, &lease, &key(&current), &current, &f.authority)
        .unwrap();
    assert_eq!(budget(&f, "task-1"), (900, 100, 1));
    // A second probe is not admitted while the first holds the last 100 tokens, whatever
    // the captured allowance says.
    assert!(
        f.store
            .reserve_task_auth_probe(&f.cas, &lease, &key(&current), &current, &f.authority)
            .is_err()
    );
    f.store
        .start_task_auth_probe(&f.cas, &lease, &mut first, &current, &f.authority)
        .unwrap();
    let settled = f
        .store
        .settle_task_auth_probe(
            &f.cas,
            &lease,
            &first,
            100,
            TaskAuthProbeOutcomeV1::Failed {},
            None,
        )
        .unwrap();
    assert!(!settled.verified && !settled.overrun);
    f.store = EventStore::open(&f.path).unwrap();
    assert_eq!(budget(&f, "task-1"), (1000, 0, 2));
    let execution = state(&f, "task-1").execution.unwrap();
    assert_eq!(execution.budget.remaining_limits().tokens, 0);
    assert_eq!(state(&f, "task-1").auth.probe_charge(), 100);
    // The allowance still names a second probe; the Task ledger has no headroom for it.
    let sequence = state(&f, "task-1").next_sequence;
    let error = f
        .store
        .reserve_task_auth_probe(&f.cas, &lease, &key(&current), &current, &f.authority)
        .unwrap_err();
    assert!(error.to_string().contains("token limit"), "{error}");
    assert_eq!(state(&f, "task-1").next_sequence, sequence);
    assert_eq!(budget(&f, "task-1"), (1000, 0, 2));
}

/// prior-2: every paid verification dispatch rechecks revision, plan, approval, deadline,
/// principal, executable binding, model and effort, and spends nothing when any drifted.
#[test]
fn drifted_authority_blocks_verification_with_zero_new_usage() {
    type Drift = fn(&mut Fixture, &TaskLease) -> (TaskAuthBindingV1, Option<TaskAuthBlockV1>);
    let drifts: [(&str, bool, Drift); 6] = [
        ("revoked authorization", true, |f, _| {
            f.authority.current = false;
            (
                binding(f, "author"),
                Some(TaskAuthBlockV1::AuthorityChanged),
            )
        }),
        ("changed model", false, |f, _| {
            let mut current = binding(f, "author");
            current.model = "another-model".into();
            (current, Some(TaskAuthBlockV1::BindingChanged))
        }),
        ("changed effort", false, |f, _| {
            let mut current = binding(f, "author");
            current.effort = "low".into();
            (current, Some(TaskAuthBlockV1::BindingChanged))
        }),
        ("changed executable policy", false, |f, _| {
            let mut current = binding(f, "author");
            current.invocation_policy_id = f.plan_id.clone();
            (current, Some(TaskAuthBlockV1::BindingChanged))
        }),
        ("changed principal", false, |f, _| {
            let mut current = binding(f, "author");
            current.context.principal_id = "principal-z".into();
            (current, Some(TaskAuthBlockV1::BindingChanged))
        }),
        ("revoked plan approval", true, |f, lease| {
            let plan_id = state(f, lease.task_id()).plan_id.unwrap();
            f.store
                .revoke_task_approval(&f.cas, lease, &plan_id, "stop", &f.authority)
                .unwrap();
            (binding(f, "author"), None)
        }),
    ];
    for (name, generated, drift) in drifts {
        let mut f = recoverable(Setup {
            generated,
            ..Setup::default()
        });
        let lease = start(&mut f, "task-1");
        let failed = fail(&mut f, &lease, 7);
        suspend(&mut f, &lease, &failed, AuthRevoked, &["author"], None);
        let context = key(&binding(&f, "author"));
        let (current, expected) = drift(&mut f, &lease);
        let before = (budget(&f, "task-1"), state(&f, "task-1").next_sequence);
        let error = f
            .store
            .reserve_task_auth_probe(&f.cas, &lease, &context, &current, &f.authority)
            .unwrap_err();
        match expected {
            Some(reason) => assert!(
                matches!(error, StoreError::AuthRecoveryBlocked(found) if found == reason),
                "{name}: {error}"
            ),
            None => assert!(
                error.to_string().contains("not suspended"),
                "{name}: {error}"
            ),
        }
        assert_eq!(
            (budget(&f, "task-1"), state(&f, "task-1").next_sequence),
            before,
            "{name}"
        );
        assert!(state(&f, "task-1").auth.probes.is_empty(), "{name}");
    }

    // Drift between reservation and dispatch is caught at start; the unstarted credit returns.
    let mut f = recoverable(Setup {
        generated: true,
        ..Setup::default()
    });
    let lease = start(&mut f, "task-1");
    let failed = fail(&mut f, &lease, 7);
    suspend(&mut f, &lease, &failed, AuthRevoked, &["author"], None);
    let current = binding(&f, "author");
    let mut reserved = f
        .store
        .reserve_task_auth_probe(&f.cas, &lease, &key(&current), &current, &f.authority)
        .unwrap();
    f.authority.current = false;
    assert!(matches!(
        f.store
            .start_task_auth_probe(&f.cas, &lease, &mut reserved, &current, &f.authority)
            .unwrap_err(),
        StoreError::AuthRecoveryBlocked(TaskAuthBlockV1::AuthorityChanged)
    ));
    f.store
        .release_task_auth_probe(&f.cas, &lease, &reserved, "authority drifted")
        .unwrap();
    assert_eq!(budget(&f, "task-1"), (7, 0, 1));

    // A source refresh replaces the revision and plan: the old suspension cannot be verified
    // or claimed, and nothing is spent.
    let mut f = recoverable(Setup::default());
    let lease = start(&mut f, "task-1");
    let failed = fail(&mut f, &lease, 7);
    suspend(&mut f, &lease, &failed, AuthRevoked, &["author"], None);
    let changed = super::source::next(&f);
    let (changed_id, changed_plan) = super::source::plan_for(&f, &changed);
    f.store
        .refresh_task_source(
            &f.cas,
            &lease,
            &changed_id,
            Some(&changed_plan),
            None,
            &f.authority,
        )
        .unwrap();
    let current = binding(&f, "author");
    assert!(
        f.store
            .reserve_task_auth_probe(&f.cas, &lease, &key(&current), &current, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("not suspended")
    );
    assert!(
        f.store
            .claim_task_auth_resume(&f.cas, &lease, &f.authority)
            .is_err()
    );
    assert_eq!(budget(&f, "task-1"), (7, 0, 1));

    // An expired original deadline blocks before dispatch. No deadline is manufactured.
    let mut f = recoverable(Setup {
        deadline_in_ms: 4_000,
        ..Setup::default()
    });
    let lease = start(&mut f, "task-1");
    let failed = fail(&mut f, &lease, 7);
    suspend(&mut f, &lease, &failed, AuthRevoked, &["author"], None);
    sleep_past(state(&f, "task-1").revision.limits.deadline_unix_ms);
    let current = binding(&f, "author");
    assert!(matches!(
        f.store
            .reserve_task_auth_probe(&f.cas, &lease, &key(&current), &current, &f.authority)
            .unwrap_err(),
        StoreError::AuthRecoveryBlocked(TaskAuthBlockV1::DeadlineExpired)
    ));
    assert!(matches!(
        f.store
            .claim_task_auth_resume(&f.cas, &lease, &f.authority)
            .unwrap_err(),
        StoreError::AuthRecoveryBlocked(TaskAuthBlockV1::DeadlineExpired)
    ));
    assert_eq!(budget(&f, "task-1"), (7, 0, 1));
    let blocked = f
        .store
        .record_task_auth_block(&f.cas, "task-1", TaskAuthBlockV1::DeadlineExpired)
        .unwrap()
        .unwrap();
    assert_eq!(
        blocked.outcome,
        TaskAuthOutcomeV1::Blocked {
            reason: TaskAuthBlockV1::DeadlineExpired
        }
    );

    // A plan without a captured allowance gets none from a login.
    let mut f = recoverable(Setup {
        probes: None,
        ..Setup::default()
    });
    let lease = start(&mut f, "task-1");
    let failed = fail(&mut f, &lease, 7);
    suspend(&mut f, &lease, &failed, AuthRevoked, &["author"], None);
    let context = key(&binding(&f, "author"));
    f.store
        .record_provider_auth_login(&context, "login-1")
        .unwrap();
    let current = binding(&f, "author");
    assert!(matches!(
        f.store
            .reserve_task_auth_probe(&f.cas, &lease, &context, &current, &f.authority)
            .unwrap_err(),
        StoreError::AuthRecoveryBlocked(TaskAuthBlockV1::AllowanceMissing)
    ));
    assert_eq!(budget(&f, "task-1"), (7, 0, 1));
}

/// prior-3: an acknowledgement whose usage exceeds its reservation verifies nothing.
#[test]
fn an_overrun_acknowledgement_keeps_its_exact_charge_and_fences_the_task() {
    let mut f = recoverable(Setup::default());
    let lease = start(&mut f, "task-1");
    let failed = fail(&mut f, &lease, 7);
    suspend(
        &mut f,
        &lease,
        &failed,
        AuthRevoked,
        &["author"],
        Some(participant("user-1", "chat-1")),
    );
    let settled = probe(&mut f, &lease, "author", 150, acknowledged()).unwrap();
    assert!(settled.overrun && !settled.verified);
    f.store = EventStore::open(&f.path).unwrap();
    assert_eq!(budget(&f, "task-1"), (157, 0, 2));
    assert!(state(&f, "task-1").execution.unwrap().budget.breached());
    let recovery = f
        .store
        .provider_auth_recovery(&key(&binding(&f, "author")))
        .unwrap();
    assert_eq!(recovery.status, AuthRecoveryStatus::Failed);
    assert!(recovery.probe_failures[0].overrun);
    assert_eq!(
        recovery.outcomes[0].outcome,
        TaskAuthOutcomeV1::Blocked {
            reason: TaskAuthBlockV1::BudgetBreached
        }
    );
    assert!(matches!(
        f.store
            .claim_task_auth_resume(&f.cas, &lease, &f.authority)
            .unwrap_err(),
        StoreError::AuthRecoveryBlocked(TaskAuthBlockV1::BudgetBreached)
    ));
    assert!(probe(&mut f, &lease, "author", 1, acknowledged()).is_err());
    assert!(
        f.store
            .reserve_task_attempt(&f.cas, &lease, WRITE, &f.authority)
            .is_err()
    );
    assert_eq!(budget(&f, "task-1"), (157, 0, 2));
}

/// prior-4: a newer auth failure invalidates an older claim at the effective dispatch boundary.
#[test]
fn a_newer_failure_invalidates_an_older_claimed_resume_atomically() {
    let mut f = recoverable(Setup::default());
    let a = start(&mut f, "task-a");
    let b = start(&mut f, "task-b");
    let failed_a = fail(&mut f, &a, 7);
    let failed_b = fail(&mut f, &b, 7);
    suspend(&mut f, &a, &failed_a, AuthRevoked, &["author"], None);
    suspend(&mut f, &b, &failed_b, AuthRevoked, &["author"], None);
    let context = key(&binding(&f, "author"));
    let recovery = f.store.provider_auth_recovery(&context).unwrap();
    assert_eq!(
        (recovery.generation, recovery.participants.len()),
        (1, 2),
        "concurrent failures share one generation and one login"
    );
    assert!(
        probe(&mut f, &a, "author", 5, acknowledged())
            .unwrap()
            .verified
    );
    // B's login/probe is unnecessary: the shared generation is verified.
    claimed(
        f.store
            .claim_task_auth_resume(&f.cas, &b, &f.authority)
            .unwrap(),
    );
    // A claims ...
    claimed(
        f.store
            .claim_task_auth_resume(&f.cas, &a, &f.authority)
            .unwrap(),
    );
    // ... B records a newer failure, opening the next generation ...
    let again = fail(&mut f, &b, 7);
    suspend(&mut f, &b, &again, AuthRevoked, &["author"], None);
    assert_eq!(
        f.store.provider_auth_recovery(&context).unwrap().generation,
        2
    );
    // ... and A's dispatch under the invalidated generation is refused before any append.
    let sequence = state(&f, "task-a").next_sequence;
    let error = f
        .store
        .reserve_task_attempt(&f.cas, &a, WRITE, &f.authority)
        .unwrap_err();
    assert!(
        matches!(error, StoreError::AuthRecoveryInvalidated(_)),
        "{error}"
    );
    assert_eq!(state(&f, "task-a").next_sequence, sequence);
    assert_eq!(
        budget(&f, "task-a"),
        (7 + 5, 0, 2),
        "failure and probe, nothing more"
    );
}

/// prior-4: the recheck is inside the write transaction, not only before it.
#[test]
fn a_failure_racing_a_prepared_dispatch_fence_fails_the_dispatch_write() {
    let mut f = recoverable(Setup::default());
    let a = start(&mut f, "task-a");
    let b = start(&mut f, "task-b");
    let failed_a = fail(&mut f, &a, 7);
    let failed_b = fail(&mut f, &b, 7);
    suspend(&mut f, &a, &failed_a, AuthRevoked, &["author"], None);
    suspend(
        &mut f,
        &b,
        &failed_b,
        AuthRevoked,
        &["author"],
        Some(participant("user-b", "chat-b")),
    );
    probe(&mut f, &a, "author", 5, acknowledged()).unwrap();
    claimed(
        f.store
            .claim_task_auth_resume(&f.cas, &a, &f.authority)
            .unwrap(),
    );
    claimed(
        f.store
            .claim_task_auth_resume(&f.cas, &b, &f.authority)
            .unwrap(),
    );
    let fence = f
        .store
        .auth_dispatch_fence(
            &f.cas,
            &TaskTransitionV1 {
                writer: "writer-1".into(),
                epoch: 1,
                now_unix_ms: now().unwrap(),
                change: TaskChangeV1::Resumed {},
            },
            Some(&state(&f, "task-a")),
        )
        .unwrap()
        .expect("a claimed Task's dispatch carries a fence");
    // An unrelated recovery row (B's acknowledged outcome) does not invalidate A's claim.
    let context = key(&binding(&f, "author"));
    let before = f.store.provider_auth_recovery(&context).unwrap();
    let outcome = before
        .outcomes
        .iter()
        .find(|o| o.task_id == "task-b")
        .unwrap()
        .sequence;
    assert!(
        f.store
            .acknowledge_task_auth_outcome(&context, outcome, "chat-b", "message-1")
            .unwrap()
    );
    assert!(
        f.store
            .provider_auth_recovery(&context)
            .unwrap()
            .next_sequence
            > before.next_sequence
    );
    fence.publish(&f.store.conn).unwrap();
    // Between the check and the append, B records a newer failure.
    let again = fail(&mut f, &b, 7);
    suspend(&mut f, &b, &again, AuthRevoked, &["author"], None);
    let error = fence.publish(&f.store.conn).unwrap_err();
    assert!(
        matches!(error, StoreError::AuthRecoveryInvalidated(_)),
        "{error}"
    );
}

/// prior-5: work requiring several failed contexts dispatches only after each is verified.
#[test]
fn every_still_required_context_must_be_verified_before_dispatch() {
    let mut f = recoverable(Setup::default());
    let lease = start(&mut f, "task-1");
    let failed = fail(&mut f, &lease, 7);
    suspend(
        &mut f,
        &lease,
        &failed,
        AuthRevoked,
        &["author", "reviewer"],
        None,
    );
    let (a, b) = (key(&binding(&f, "author")), key(&binding(&f, "reviewer")));
    assert!(
        probe(&mut f, &lease, "author", 5, acknowledged())
            .unwrap()
            .verified
    );
    match f
        .store
        .claim_task_auth_resume(&f.cas, &lease, &f.authority)
        .unwrap()
    {
        TaskAuthClaim::Unverified { context_keys } => assert_eq!(context_keys, vec![b.clone()]),
        other => panic!("{other:?}"),
    }
    let task = state(&f, "task-1");
    assert!(matches!(task.phase, TaskPhaseV1::Waiting { .. }));
    assert!(
        task.auth.probes.values().all(|p| p.context_key == a),
        "no call reached the broken context"
    );
    assert!(
        f.store
            .reserve_task_attempt(&f.cas, &lease, WRITE, &f.authority)
            .is_err()
    );
    assert!(
        probe(&mut f, &lease, "reviewer", 5, acknowledged())
            .unwrap()
            .verified
    );
    claimed(
        f.store
            .claim_task_auth_resume(&f.cas, &lease, &f.authority)
            .unwrap(),
    );
    assert_eq!(state(&f, "task-1").phase, TaskPhaseV1::Running {});
    f.store
        .reserve_task_attempt(&f.cas, &lease, WRITE, &f.authority)
        .unwrap();
}

/// prior-6: a crash after the durable claim, or around a probe or resumed dispatch, finishes the
/// existing work instead of stranding it or running it twice.
#[test]
fn crashes_after_claims_probes_and_resumed_dispatch_recover_exactly_once() {
    let mut f = recoverable(Setup::default());
    let lease = start(&mut f, "task-1");
    let failed = fail(&mut f, &lease, 7);
    suspend(&mut f, &lease, &failed, AuthRevoked, &["author"], None);
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    // The probe writer dies after durably starting its paid call.
    let short = f
        .store
        .take_task_lease(&f.cas, "task-1", "writer-2", 3_000)
        .unwrap();
    let current = binding(&f, "author");
    let mut lost = f
        .store
        .reserve_task_auth_probe(&f.cas, &short, &key(&current), &current, &f.authority)
        .unwrap();
    f.store
        .start_task_auth_probe(&f.cas, &short, &mut lost, &current, &f.authority)
        .unwrap();
    sleep_past(state(&f, "task-1").lease_until_unix_ms());
    f.store = EventStore::open(&f.path).unwrap();
    let lease = f
        .store
        .take_task_lease(&f.cas, "task-1", "writer-3", 1_000_000)
        .unwrap();
    assert!(
        f.store
            .reserve_task_auth_probe(&f.cas, &lease, &key(&current), &current, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("pending")
    );
    f.store.recover_task_auth_probes(&f.cas, &lease).unwrap();
    assert_eq!(
        budget(&f, "task-1"),
        (107, 0, 2),
        "abandoned keeps its reservation"
    );
    assert!(
        probe(&mut f, &lease, "author", 5, acknowledged())
            .unwrap()
            .verified
    );
    let (claim, replayed) = claimed(
        f.store
            .claim_task_auth_resume(&f.cas, &lease, &f.authority)
            .unwrap(),
    );
    assert!(!replayed);
    // Crash after the durable claim, before any dispatch: retry finishes the same claim.
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let lease = f
        .store
        .take_task_lease(&f.cas, "task-1", "writer-4", 3_000)
        .unwrap();
    let sequence = state(&f, "task-1").next_sequence;
    let (again, replayed) = claimed(
        f.store
            .claim_task_auth_resume(&f.cas, &lease, &f.authority)
            .unwrap(),
    );
    assert!(replayed && again == claim);
    assert_eq!(
        state(&f, "task-1").next_sequence,
        sequence,
        "no second claim"
    );
    let resumed = f
        .store
        .provider_auth_recovery(&key(&current))
        .unwrap()
        .outcomes
        .iter()
        .filter(|o| o.outcome == TaskAuthOutcomeV1::Resumed {})
        .count();
    assert_eq!(resumed, 1, "one coordinator outcome per claim");
    // Crash after the resumed Attempt started: recovery charges it and never reruns it.
    let started = attempt_on(&mut f, &lease);
    sleep_past(state(&f, "task-1").lease_until_unix_ms());
    f.store = EventStore::open(&f.path).unwrap();
    let lease = f
        .store
        .take_task_lease(&f.cas, "task-1", "writer-5", 1_000_000)
        .unwrap();
    f.store.recover_task_attempts(&f.cas, &lease).unwrap();
    let rows = state(&f, "task-1").execution.unwrap().attempt_accounting();
    let row = rows.iter().find(|r| r.attempt_id == started.id()).unwrap();
    assert!(matches!(
        row.result,
        Some(TaskAttemptResultV1::Abandoned { .. })
    ));
    assert_eq!(budget(&f, "task-1"), (107 + 5 + 100, 0, 4));
}

/// prior-7: each Task's own coordinator is notified once, including transient recovery without
/// a login, and never the coordinator that happened to start the login.
#[test]
fn outcomes_route_to_each_participations_own_coordinator_exactly_once() {
    let mut f = recoverable(Setup::default());
    let x = start(&mut f, "task-x");
    let y = start(&mut f, "task-y");
    let quiet = start(&mut f, "task-quiet");
    for (lease, who) in [
        (&x, Some(participant("user-x", "chat-x"))),
        (&y, Some(participant("user-y", "chat-y"))),
        (&quiet, None),
    ] {
        let failed = fail(&mut f, lease, 7);
        suspend(&mut f, lease, &failed, AuthRevoked, &["author"], who);
    }
    let context = key(&binding(&f, "author"));
    // X's coordinator ran the login; that grants no route to Y's Task.
    f.store
        .record_provider_auth_login(&context, "login-x")
        .unwrap();
    f.store
        .record_provider_auth_login(&context, "login-x")
        .unwrap();
    assert!(
        probe(&mut f, &x, "author", 5, acknowledged())
            .unwrap()
            .verified
    );
    for lease in [&x, &y, &quiet] {
        claimed(
            f.store
                .claim_task_auth_resume(&f.cas, lease, &f.authority)
                .unwrap(),
        );
    }
    let recovery = f.store.provider_auth_recovery(&context).unwrap();
    assert_eq!(recovery.login_refs, vec!["login-x".to_string()]);
    let routes: Vec<_> = recovery
        .outcomes
        .iter()
        .map(|o| {
            (
                o.task_id.as_str(),
                o.participant.as_ref().map(|p| p.coordinator_ref.as_str()),
            )
        })
        .collect();
    assert_eq!(
        routes,
        vec![
            ("task-x", Some("chat-x")),
            ("task-y", Some("chat-y")),
            ("task-quiet", None)
        ]
    );
    assert_eq!(recovery.pending_notifications().count(), 2);
    let y_outcome = recovery.outcomes[1].sequence;
    let quiet_outcome = recovery.outcomes[2].sequence;
    assert!(
        f.store
            .acknowledge_task_auth_outcome(&context, y_outcome, "chat-x", "msg-1")
            .is_err(),
        "the login coordinator cannot take another Task's outcome"
    );
    assert!(
        f.store
            .acknowledge_task_auth_outcome(&context, quiet_outcome, "chat-x", "msg-1")
            .is_err(),
        "an outcome without a private route is never redirected"
    );
    assert!(
        f.store
            .acknowledge_task_auth_outcome(&context, y_outcome, "chat-y", "msg-2")
            .unwrap()
    );
    assert!(
        !f.store
            .acknowledge_task_auth_outcome(&context, y_outcome, "chat-y", "msg-2")
            .unwrap(),
        "a replayed acknowledgement is idempotent"
    );
    assert!(
        f.store
            .acknowledge_task_auth_outcome(&context, y_outcome, "chat-y", "msg-3")
            .is_err()
    );
    assert!(
        f.store
            .acknowledge_task_auth_outcome(&context, y_outcome, "https://x", "msg-3")
            .is_err()
    );
    let recovery = f.store.provider_auth_recovery(&context).unwrap();
    let pending: Vec<_> = recovery
        .pending_notifications()
        .map(|o| o.task_id.as_str())
        .collect();
    assert_eq!(pending, vec!["task-x"]);
}

#[test]
fn a_transient_refresh_contention_recovers_without_any_login() {
    let mut f = recoverable(Setup::default());
    let lease = start(&mut f, "task-1");
    let failed = fail(&mut f, &lease, 7);
    suspend(
        &mut f,
        &lease,
        &failed,
        AuthRefreshContended,
        &["author"],
        Some(participant("user-1", "chat-1")),
    );
    assert!(
        probe(&mut f, &lease, "author", 3, acknowledged())
            .unwrap()
            .verified
    );
    claimed(
        f.store
            .claim_task_auth_resume(&f.cas, &lease, &f.authority)
            .unwrap(),
    );
    let recovery = f
        .store
        .provider_auth_recovery(&key(&binding(&f, "author")))
        .unwrap();
    assert!(recovery.login_refs.is_empty());
    assert_eq!(recovery.pending_notifications().count(), 1);
    // Login can only join an open generation; a verified one stays verified.
    assert!(
        f.store
            .record_provider_auth_login(&key(&binding(&f, "author")), "late-login")
            .is_err()
    );
}

/// The full acceptance: an authenticated-looking status is not recovery; only an acknowledged
/// probe verifies, and a failed one asks every participant for the private login again.
#[test]
fn verification_failure_never_marks_recovery_and_asks_for_the_private_login() {
    let mut f = recoverable(Setup::default());
    let lease = start(&mut f, "task-1");
    let failed = fail(&mut f, &lease, 7);
    suspend(
        &mut f,
        &lease,
        &failed,
        AuthRevoked,
        &["author"],
        Some(participant("user-1", "chat-1")),
    );
    let context = key(&binding(&f, "author"));
    f.store
        .record_provider_auth_login(&context, "login-1")
        .unwrap();
    let settled = probe(
        &mut f,
        &lease,
        "author",
        2,
        TaskAuthProbeOutcomeV1::AuthFailed {
            failure: TaskAuthFailureV1::AuthRefreshFailed,
        },
    )
    .unwrap();
    assert!(!settled.verified);
    let recovery = f.store.provider_auth_recovery(&context).unwrap();
    assert_eq!(recovery.status, AuthRecoveryStatus::Failed);
    assert_eq!(
        recovery.outcomes[0].outcome,
        TaskAuthOutcomeV1::LoginRequired {}
    );
    match f
        .store
        .claim_task_auth_resume(&f.cas, &lease, &f.authority)
        .unwrap()
    {
        TaskAuthClaim::Unverified { .. } => (),
        other => panic!("{other:?}"),
    }
    // A non-auth failure is reported as such, never as a login request.
    let settled = probe(
        &mut f,
        &lease,
        "author",
        2,
        TaskAuthProbeOutcomeV1::Failed {},
    )
    .unwrap();
    assert!(!settled.verified);
    let recovery = f.store.provider_auth_recovery(&context).unwrap();
    assert_eq!(
        recovery.outcomes.last().unwrap().outcome,
        TaskAuthOutcomeV1::VerificationFailed {}
    );
    assert_eq!(budget(&f, "task-1"), (11, 0, 3));
}

/// prior-8: an already-terminal Task is continued only by an explicitly linked successor that
/// keeps the predecessor immutable, its accounting retained and the original bounds.
#[test]
fn a_terminal_predecessor_is_continued_by_one_linked_bounded_successor() {
    let mut f = recoverable(Setup {
        write_tokens: 600,
        ..Setup::default()
    });
    let lease = start(&mut f, "task-1");
    fail(&mut f, &lease, 600);
    let result = TaskResultV1 {
        task_revision_id: state(&f, "task-1").revision_id,
        execution: TaskExecutionV1::Incomplete,
        acceptance: task::TaskAcceptanceV1::Unsatisfied,
        domain_conclusion: "Provider authentication failed before recovery existed".into(),
        outputs: BTreeMap::new(),
        evidence: BTreeSet::new(),
        missing_obligations: BTreeSet::from(["checked".into()]),
    };
    let result_id = put(&f.cas, task::TASK_RESULT_V1, &result);
    f.store
        .finish_task(&f.cas, &lease, &result_id, &f.authority)
        .unwrap();
    let before = state(&f, "task-1");
    // A finished Task has no suspension to verify or claim, and no recovery record is needed.
    assert!(
        f.store
            .claim_task_auth_resume(&f.cas, &lease, &f.authority)
            .is_err()
    );
    let link = f.store.task_continuation_link(&f.cas, "task-1").unwrap();
    assert_eq!(link.predecessor_chargeable_tokens.get(), 600);
    assert_eq!(link.predecessor_begun_attempts, 1);
    assert_eq!(link.original_limits, before.revision.limits);
    let remaining = link.remaining_limits().unwrap();
    assert_eq!((remaining.tokens, remaining.max_attempts), (400, 9));
    assert_eq!(
        remaining.deadline_unix_ms,
        before.revision.limits.deadline_unix_ms
    );
    let link_id = put(&f.cas, TASK_CONTINUATION_V1, &link);
    let successor = |task_id: &str, edit: &dyn Fn(&mut TaskRevisionV1)| {
        let mut revision = before.revision.clone();
        revision.task_id = task_id.into();
        revision.limits = remaining.clone();
        revision.provenance.input_artifact_ids.push(link_id.clone());
        edit(&mut revision);
        revision
    };
    for (name, revision) in [
        (
            "wider tokens",
            successor("task-2", &|r| r.limits.tokens = 401),
        ),
        (
            "later deadline",
            successor("task-2", &|r| r.limits.deadline_unix_ms += 1),
        ),
        ("changed goal", successor("task-2", &|r| r.goal.push('!'))),
        (
            "changed authority",
            successor("task-2", &|r| {
                r.authority.allowed_effects.insert("write-source".into());
            }),
        ),
    ] {
        let id = put(&f.cas, task::TASK_REVISION_V1, &revision);
        assert!(
            f.store.open_task(&f.cas, &id, "writer-2", 1_000).is_err(),
            "{name}"
        );
    }
    let mut tampered = link.clone();
    tampered.predecessor_chargeable_tokens = 0u128.into();
    let tampered_id = put(&f.cas, TASK_CONTINUATION_V1, &tampered);
    let mut forged = successor("task-2", &|_| ());
    forged.provenance.input_artifact_ids.pop();
    forged.provenance.input_artifact_ids.push(tampered_id);
    forged.limits.tokens = 999;
    let forged_id = put(&f.cas, task::TASK_REVISION_V1, &forged);
    assert!(
        f.store
            .open_task(&f.cas, &forged_id, "writer-2", 1_000)
            .is_err()
    );

    let accepted = successor("task-2", &|_| ());
    let accepted_id = put(&f.cas, task::TASK_REVISION_V1, &accepted);
    f.store
        .open_task(&f.cas, &accepted_id, "writer-2", 1_000)
        .unwrap();
    assert_eq!(
        f.store.task_successor("task-1").unwrap().as_deref(),
        Some("task-2")
    );
    let opened = state(&f, "task-2");
    assert!(
        !opened.admitted && opened.plan_id.is_none(),
        "no inferred plan or approval"
    );
    assert_eq!(
        EventStore::revision_continuation(&f.cas, &opened.revision)
            .unwrap()
            .unwrap()
            .1,
        link
    );
    // The predecessor is never reopened or rewritten, and keeps its failed spend.
    let after = state(&f, "task-1");
    assert_eq!(after.next_sequence, before.next_sequence);
    assert_eq!(after.phase, before.phase);
    assert_eq!(budget(&f, "task-1"), (600, 0, 1));
    // One finished Task is continued at most once.
    let second = successor("task-3", &|_| ());
    let second_id = put(&f.cas, task::TASK_REVISION_V1, &second);
    assert!(
        f.store
            .open_task(&f.cas, &second_id, "writer-3", 1_000)
            .unwrap_err()
            .to_string()
            .contains("already continued")
    );
    assert!(f.store.task_continuation_link(&f.cas, "task-1").is_err());
}

/// An exhausted captured allowance blocks; a login cannot replenish it.
#[test]
fn an_exhausted_recovery_allowance_blocks_without_a_new_allowance_from_login() {
    let mut f = recoverable(Setup {
        probes: Some((1, 100)),
        ..Setup::default()
    });
    let lease = start(&mut f, "task-1");
    let failed = fail(&mut f, &lease, 7);
    suspend(&mut f, &lease, &failed, AuthRevoked, &["author"], None);
    let settled = probe(
        &mut f,
        &lease,
        "author",
        4,
        TaskAuthProbeOutcomeV1::Failed {},
    )
    .unwrap();
    assert!(!settled.verified);
    let context = key(&binding(&f, "author"));
    f.store
        .record_provider_auth_login(&context, "login-1")
        .unwrap();
    let sequence = state(&f, "task-1").next_sequence;
    assert!(matches!(
        probe(&mut f, &lease, "author", 4, acknowledged()).unwrap_err(),
        StoreError::AuthRecoveryBlocked(TaskAuthBlockV1::AllowanceExhausted)
    ));
    assert_eq!(state(&f, "task-1").next_sequence, sequence);
    assert_eq!(budget(&f, "task-1"), (11, 0, 2));
    assert_eq!(
        f.store.provider_auth_recovery(&context).unwrap().status,
        AuthRecoveryStatus::Failed
    );
}
