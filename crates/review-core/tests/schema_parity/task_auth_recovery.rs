//! ADR-0141: runtime Provider auth recovery contracts. Every typed value satisfies its schema,
//! and both sides refuse undeclared fields, secret-shaped references and widened vocabularies.

use super::{assert_invalid, assert_valid, validator};
use review_core::EventType;
use review_core::task::auth_recovery::*;
use review_core::task::event::{TaskChangeV1, TaskTransitionV1};
use review_core::task::{TaskLimitsV1, TaskWaitingReasonV1, VerificationReserveV1};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn digest(n: char) -> String {
    format!("sha256:{}", n.to_string().repeat(64))
}

fn binding() -> TaskAuthBindingV1 {
    TaskAuthBindingV1 {
        context: ProviderAuthContextV1 {
            provider: "work".into(),
            provider_kind: "claude".into(),
            principal_id: "principal-1".into(),
        },
        model: "claude-opus-5-5".into(),
        effort: "high".into(),
        invocation_policy_id: digest('1'),
    }
}

fn suspension() -> TaskAuthSuspensionV1 {
    TaskAuthSuspensionV1 {
        task_id: "task-1".into(),
        task_revision_id: digest('2'),
        plan_id: digest('3'),
        deadline_unix_ms: 1_800_000_000_000,
        failures: vec![TaskAuthFailedAttemptV1 {
            attempt_id: "a".repeat(26),
            node: "root.providers.admit0".into(),
            reservation_id: "reservation:0".into(),
            charged_tokens: u128::from(u64::MAX).into(),
            failure: TaskAuthFailureV1::AuthRefreshFailed,
        }],
        contexts: BTreeMap::from([(
            digest('4'),
            TaskAuthRequirementV1 {
                binding: binding(),
                generation: 3,
            },
        )]),
        participant: Some(TaskAuthParticipantV1 {
            requester_ref: "user:42".into(),
            coordinator_ref: "chat@7".into(),
        }),
    }
}

/// Both sides refuse each forged field.
fn refuse<T: serde::de::DeserializeOwned>(
    schema: &str,
    value: &Value,
    forged: &[(&str, Value)],
    validate: impl Fn(T) -> Result<(), String>,
) {
    for (pointer, bad) in forged {
        let mut copy = value.clone();
        *copy
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("{pointer}")) = bad.clone();
        assert_invalid(schema, &copy, pointer);
        let typed = serde_json::from_value::<T>(copy)
            .map_err(|e| e.to_string())
            .and_then(&validate);
        assert!(typed.is_err(), "{schema}{pointer} accepted by Rust");
    }
}

#[test]
fn suspensions_and_claims_are_closed_exact_and_secret_free() {
    let value = serde_json::to_value(suspension()).unwrap();
    suspension().validate().unwrap();
    assert_valid("task-auth-suspension-v1.json", &value);
    let mut unrouted = suspension();
    unrouted.participant = None;
    unrouted.failures.clear();
    unrouted.validate().unwrap();
    assert_valid(
        "task-auth-suspension-v1.json",
        &serde_json::to_value(unrouted).unwrap(),
    );
    let key = digest('4');
    refuse::<TaskAuthSuspensionV1>(
        "task-auth-suspension-v1.json",
        &value,
        &[
            ("/plan_id", json!("plan")),
            ("/deadline_unix_ms", json!(0)),
            ("/failures/0/failure", json!("quota")),
            ("/failures/0/charged_tokens", json!(7)),
            ("/failures/0/attempt_id", json!("short")),
            (
                "/participant/coordinator_ref",
                json!("https://chat.example/x"),
            ),
            ("/participant/requester_ref", json!("user 42")),
            (&format!("/contexts/{key}/generation"), json!(0)),
            (&format!("/contexts/{key}/binding/model"), json!(" ")),
            (
                &format!("/contexts/{key}/binding/context/principal_id"),
                json!("a\u{7}b"),
            ),
            ("/contexts", json!({})),
        ],
        |value| value.validate(),
    );
    let mut extra = value.clone();
    extra["access_token"] = json!("x");
    assert_invalid("task-auth-suspension-v1.json", &extra, "undeclared field");
    assert!(serde_json::from_value::<TaskAuthSuspensionV1>(extra).is_err());

    let claim = TaskAuthResumeClaimV1 {
        task_id: "task-1".into(),
        suspension_id: digest('5'),
        task_revision_id: digest('2'),
        plan_id: digest('3'),
        contexts: BTreeMap::from([(
            digest('4'),
            TaskAuthClaimedContextV1 {
                generation: 3,
                verified_sequence: 0,
            },
        )]),
    };
    claim.validate().unwrap();
    let value = serde_json::to_value(&claim).unwrap();
    assert_valid("task-auth-resume-claim-v1.json", &value);
    refuse::<TaskAuthResumeClaimV1>(
        "task-auth-resume-claim-v1.json",
        &value,
        &[
            ("/suspension_id", json!("latest")),
            ("/contexts", json!({})),
            (&format!("/contexts/{key}/generation"), json!(0)),
        ],
        |value| value.validate(),
    );
}

#[test]
fn continuation_links_retain_exact_predecessor_accounting() {
    let link = TaskContinuationV1 {
        predecessor_task_id: "task-1".into(),
        predecessor_revision_id: digest('1'),
        predecessor_plan_id: digest('2'),
        predecessor_result_id: digest('3'),
        predecessor_chargeable_tokens: u128::MAX.into(),
        predecessor_begun_attempts: 3,
        original_limits: TaskLimitsV1 {
            tokens: 1000,
            max_attempts: 4,
            deadline_unix_ms: 1_800_000_000_000,
            verification: VerificationReserveV1 {
                tokens: 200,
                attempts: 1,
                wall_ms: 1000,
            },
        },
        reason: TaskContinuationReasonV1::ProviderAuth,
    };
    link.validate().unwrap();
    assert!(
        link.remaining_limits().is_err(),
        "spent beyond the original"
    );
    let value = serde_json::to_value(&link).unwrap();
    assert_eq!(
        value["predecessor_chargeable_tokens"],
        u128::MAX.to_string()
    );
    assert_valid("task-continuation-v1.json", &value);
    refuse::<TaskContinuationV1>(
        "task-continuation-v1.json",
        &value,
        &[
            ("/predecessor_result_id", json!("result")),
            ("/predecessor_chargeable_tokens", json!("01")),
            ("/reason", json!("login")),
            ("/original_limits/max_attempts", json!(0)),
        ],
        |value| value.validate(),
    );
}

#[test]
fn recovery_log_rows_are_closed_events_of_one_context() {
    let changes = [
        ProviderAuthRecoveryChangeV1::Failed {
            task_id: "task-1".into(),
            suspension_id: digest('5'),
            failure: Some(TaskAuthFailureV1::AuthRevoked),
        },
        ProviderAuthRecoveryChangeV1::Failed {
            task_id: "task-1".into(),
            suspension_id: digest('5'),
            failure: None,
        },
        ProviderAuthRecoveryChangeV1::Authenticated {
            login_ref: "login-1".into(),
        },
        ProviderAuthRecoveryChangeV1::ProbeFailed {
            task_id: "task-1".into(),
            attempt_id: "b".repeat(26),
            outcome: TaskAuthProbeOutcomeV1::AuthFailed {
                failure: TaskAuthFailureV1::AuthExpired,
            },
            overrun: false,
        },
        ProviderAuthRecoveryChangeV1::ProbeFailed {
            task_id: "task-1".into(),
            attempt_id: "b".repeat(26),
            outcome: TaskAuthProbeOutcomeV1::Acknowledged {},
            overrun: true,
        },
        ProviderAuthRecoveryChangeV1::Verified {
            task_id: "task-1".into(),
            attempt_id: "b".repeat(26),
        },
        ProviderAuthRecoveryChangeV1::Outcome {
            task_id: "task-1".into(),
            suspension_id: digest('5'),
            outcome: TaskAuthOutcomeV1::Blocked {
                reason: TaskAuthBlockV1::DeadlineExpired,
            },
            participant: None,
        },
        ProviderAuthRecoveryChangeV1::Outcome {
            task_id: "task-1".into(),
            suspension_id: digest('5'),
            outcome: TaskAuthOutcomeV1::Resumed {},
            participant: Some(TaskAuthParticipantV1 {
                requester_ref: "user-1".into(),
                coordinator_ref: "chat-1".into(),
            }),
        },
        ProviderAuthRecoveryChangeV1::Notified {
            outcome_sequence: 7,
            coordinator_ref: "chat-1".into(),
            delivery_ref: "message-9".into(),
        },
    ];
    for change in changes {
        let event = ProviderAuthRecoveryEventV1 {
            schema: PROVIDER_AUTH_RECOVERY_EVENT_V1.into(),
            context_key: digest('4'),
            context: binding().context,
            generation: 2,
            now_unix_ms: 100,
            change,
        };
        event.validate().unwrap();
        let value = serde_json::to_value(&event).unwrap();
        assert_valid("provider-auth-recovery-event-v1.json", &value);
        review_core::json::admit(&value).unwrap();
        let mut extra = value.clone();
        extra["change"]["url"] = json!("https://claude.ai/oauth/authorize");
        assert_invalid("provider-auth-recovery-event-v1.json", &extra, "undeclared");
        assert!(serde_json::from_value::<ProviderAuthRecoveryEventV1>(extra).is_err());
    }
    let event = ProviderAuthRecoveryEventV1 {
        schema: PROVIDER_AUTH_RECOVERY_EVENT_V1.into(),
        context_key: digest('4'),
        context: binding().context,
        generation: 2,
        now_unix_ms: 100,
        change: ProviderAuthRecoveryChangeV1::Authenticated {
            login_ref: "login-1".into(),
        },
    };
    let value = serde_json::to_value(&event).unwrap();
    refuse::<ProviderAuthRecoveryEventV1>(
        "provider-auth-recovery-event-v1.json",
        &value,
        &[
            ("/change/login_ref", json!("code=SECRET#STATE")),
            ("/generation", json!(0)),
            ("/schema", json!("af/ProviderAuthRecoveryEvent@2")),
            ("/context/provider", json!("../auth")),
        ],
        |value| value.validate(),
    );
    // A successful acknowledgement is never a probe failure unless it overran.
    let mut failed = serde_json::to_value(ProviderAuthRecoveryEventV1 {
        change: ProviderAuthRecoveryChangeV1::ProbeFailed {
            task_id: "task-1".into(),
            attempt_id: "b".repeat(26),
            outcome: TaskAuthProbeOutcomeV1::Acknowledged {},
            overrun: true,
        },
        ..event
    })
    .unwrap();
    failed["change"]["overrun"] = json!(false);
    assert!(
        serde_json::from_value::<ProviderAuthRecoveryEventV1>(failed)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn auth_recovery_transitions_and_the_auth_pause_are_versioned_task_events() {
    let id = digest('a');
    let changes = [
        TaskChangeV1::AuthSuspended {
            suspension_id: id.clone(),
        },
        TaskChangeV1::AuthProbeReserved {
            context_key: id.clone(),
            generation: 1,
            attempt_id: "c".repeat(26),
            reservation_id: "reservation:4".into(),
            reserved_tokens: 4096,
            deadline_unix_ms: 200,
        },
        TaskChangeV1::AuthProbeStarted {
            attempt_id: "c".repeat(26),
        },
        TaskChangeV1::AuthProbeReleased {
            attempt_id: "c".repeat(26),
            reason: "authority drifted".into(),
        },
        TaskChangeV1::AuthProbeSettled {
            attempt_id: "c".repeat(26),
            charged_tokens: u128::MAX.into(),
            outcome: TaskAuthProbeOutcomeV1::AuthFailed {
                failure: TaskAuthFailureV1::AuthMissing,
            },
            usage_id: Some(id.clone()),
        },
        TaskChangeV1::AuthProbeSettled {
            attempt_id: "c".repeat(26),
            charged_tokens: 7u128.into(),
            outcome: TaskAuthProbeOutcomeV1::Abandoned {},
            usage_id: None,
        },
        TaskChangeV1::AuthResumeClaimed {
            claim_id: id.clone(),
        },
    ];
    for change in changes {
        let transition = TaskTransitionV1 {
            writer: "writer-1".into(),
            epoch: 1,
            now_unix_ms: 100,
            change,
        };
        transition.validate().unwrap();
        let mut value = serde_json::to_value(&transition).unwrap();
        assert_valid("task-transition-v5.json", &value);
        review_core::event::validate_event_payload(EventType::TaskTransitionV5, &value).unwrap();
        value["change"]["refresh_token"] = json!("x");
        assert!(!validator("task-transition-v5.json").is_valid(&value));
        assert!(
            review_core::event::validate_event_payload(EventType::TaskTransitionV5, &value)
                .is_err()
        );
    }
    // A probe reservation must expire after it is recorded and name a positive charge.
    let expired = TaskTransitionV1 {
        writer: "writer-1".into(),
        epoch: 1,
        now_unix_ms: 100,
        change: TaskChangeV1::AuthProbeReserved {
            context_key: id.clone(),
            generation: 1,
            attempt_id: "c".repeat(26),
            reservation_id: "reservation:4".into(),
            reserved_tokens: 1,
            deadline_unix_ms: 100,
        },
    };
    assert!(expired.validate().is_err());
    let mut zero = serde_json::to_value(&expired).unwrap();
    zero["change"]["deadline_unix_ms"] = json!(200);
    zero["change"]["reserved_tokens"] = json!(0);
    assert_invalid("task-transition-v5.json", &zero, "zero-token probe");
    // The pause is a waiting reason a plain `waiting` change or a source refresh cannot set.
    let waiting = json!({"writer":"writer-1","epoch":1,"now_unix_ms":100,
        "change":{"kind":"waiting","reason":"needs_provider_auth"}});
    assert_valid("task-transition-v5.json", &waiting);
    let refreshed = TaskTransitionV1 {
        writer: "writer-1".into(),
        epoch: 1,
        now_unix_ms: 100,
        change: TaskChangeV1::SourceRefreshed {
            revision_id: id.clone(),
            plan_id: None,
            waiting: Some(TaskWaitingReasonV1::NeedsProviderAuth),
        },
    };
    assert!(refreshed.validate().is_err());
    assert_invalid(
        "task-transition-v5.json",
        &serde_json::to_value(&refreshed).unwrap(),
        "refresh into the auth pause",
    );
    assert_valid(
        "task-phase-v1.json",
        &json!({"kind":"waiting","reason":"needs_provider_auth"}),
    );
}

#[test]
fn a_compiled_task_captures_its_recovery_allowance_beside_existing_graphs() {
    let allowance = AuthRecoveryAllowanceV1 {
        probes: 2,
        tokens_per_probe: 4096,
        wall_ms_per_probe: 45_000,
    };
    allowance.validate().unwrap();
    let schema = validator("compiled-task-v1.json");
    let base = json!({"schema":"af.compiled-task/1","nodes":{},"order":[],"inputs":{},
        "outputs":{},"coverage":{},"calls":{},"slots":{},"max_parallel":1,"allowances":{}});
    assert!(
        schema.is_valid(&base),
        "existing captures stay valid unchanged"
    );
    let mut captured = base.clone();
    captured["auth_recovery"] = serde_json::to_value(&allowance).unwrap();
    assert!(schema.is_valid(&captured));
    for (field, bad) in [
        ("probes", json!(0)),
        ("probes", json!(17)),
        ("tokens_per_probe", json!(0)),
    ] {
        let mut forged = captured.clone();
        forged["auth_recovery"][field] = bad;
        assert!(!schema.is_valid(&forged), "{field}");
        let typed: AuthRecoveryAllowanceV1 =
            serde_json::from_value(forged["auth_recovery"].clone()).unwrap();
        assert!(typed.validate().is_err(), "{field}");
    }
}

#[test]
fn the_recover_document_is_closed_and_ties_its_exit_code_to_its_state() {
    let key = digest('4');
    let document = json!({
        "schema": TASK_AUTH_RECOVERY_V1,
        "task_id": "task-1",
        "state": "login_required",
        "contexts": [{"context_key": key, "provider": "work", "provider_kind": "claude",
            "generation": 1, "status": "failed", "login_recorded": false}],
        "accounting": {"chargeable_tokens": "10", "verification_tokens": "5",
            "begun_attempts": 2, "limit_tokens": 4096, "deadline_unix_ms": 1},
        "notifications": [{"context_key": key, "outcome_sequence": 2, "generation": 1,
            "outcome": "login_required", "requester_ref": "user-1", "coordinator_ref": "chat-1"}],
        "undeliverable": 0,
        "login": [{"provider": "work", "provider_kind": "claude"}],
        "exit_code": 3
    });
    assert_valid("task-auth-recovery-v1.json", &document);
    for (pointer, bad) in [
        ("/exit_code", json!(0)),
        ("/state", json!("recovered")),
        ("/accounting/chargeable_tokens", json!(10)),
        (
            "/notifications/0/coordinator_ref",
            json!("https://chat.example/1"),
        ),
        ("/contexts/0/status", json!("authenticated")),
    ] {
        let mut forged = document.clone();
        *forged.pointer_mut(pointer).unwrap() = bad;
        assert_invalid("task-auth-recovery-v1.json", &forged, pointer);
    }
    let mut extra = document.clone();
    extra["url"] = json!("https://claude.ai/oauth/authorize");
    assert_invalid("task-auth-recovery-v1.json", &extra, "undeclared field");
    let mut blocked = document.clone();
    blocked["state"] = json!("blocked");
    assert_invalid(
        "task-auth-recovery-v1.json",
        &blocked,
        "blocked needs its reason",
    );
    blocked["reason"] = json!("allowance_missing");
    assert_valid("task-auth-recovery-v1.json", &blocked);
    let mut terminal = document;
    terminal["state"] = json!("terminal");
    terminal["exit_code"] = json!(0);
    assert_invalid(
        "task-auth-recovery-v1.json",
        &terminal,
        "terminal needs result",
    );
    terminal["result"] = json!({"result_id": digest('9'), "execution": "exhausted",
        "acceptance": "inconclusive"});
    terminal["continuation"] = json!({"available": true});
    assert_valid("task-auth-recovery-v1.json", &terminal);
}
