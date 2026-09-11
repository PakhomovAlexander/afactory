use std::collections::BTreeMap;

use review_attempt::task_budget::{NodeAllowance, TaskBudget};
use review_core::task::{TaskLimitsV1, VerificationReserveV1};

fn budget(attempts: u32, tokens: u64, deadline: u64) -> TaskBudget {
    TaskBudget::new(
        TaskLimitsV1 {
            tokens,
            max_attempts: attempts,
            deadline_unix_ms: deadline,
            verification: VerificationReserveV1 {
                tokens: 30,
                attempts: 1,
                wall_ms: 200,
            },
        },
        BTreeMap::from([
            (
                "implement".into(),
                NodeAllowance {
                    tokens_per_attempt: 40,
                    wall_ms_per_attempt: 100,
                    max_attempts: 2,
                    verification_attempts: 0,
                },
            ),
            (
                "review.verify".into(),
                NodeAllowance {
                    tokens_per_attempt: 30,
                    wall_ms_per_attempt: 200,
                    max_attempts: 1,
                    verification_attempts: 1,
                },
            ),
        ]),
    )
    .unwrap()
}

fn spend(budget: &mut TaskBudget, node: &str, now: u64, tokens: u64) -> String {
    let reservation = budget.prepare(node, now).unwrap();
    budget.begin(&reservation.id, now).unwrap();
    budget.settle(&reservation.id, tokens).unwrap();
    reservation.id
}

#[test]
fn implementation_transport_retry_cannot_consume_the_last_verifier_attempt() {
    let mut two = budget(2, 200, 1000);
    spend(&mut two, "implement", 1, 5); // A failed transport still consumes its Attempt.
    assert!(
        two.prepare("implement", 2)
            .unwrap_err()
            .contains("Attempt limit")
    );
    spend(&mut two, "review.verify", 2, 5);
    assert_eq!(two.begun_attempts(), 2);
    assert_eq!(two.committed_tokens(), 10);

    let mut three = budget(3, 200, 1000);
    spend(&mut three, "implement", 1, 5);
    spend(&mut three, "implement", 2, 5);
    spend(&mut three, "review.verify", 3, 5);
    assert_eq!(three.begun_attempts(), 3);
}

#[test]
fn child_attempt_cap_protects_its_verifier_even_when_the_parent_has_capacity() {
    let allowance = |verify| NodeAllowance {
        tokens_per_attempt: 10,
        wall_ms_per_attempt: 10,
        max_attempts: 3,
        verification_attempts: u32::from(verify),
    };
    let mut ledger = TaskBudget::new(
        TaskLimitsV1 {
            tokens: 1000,
            max_attempts: 20,
            deadline_unix_ms: 10000,
            verification: VerificationReserveV1 {
                tokens: 10,
                attempts: 1,
                wall_ms: 10,
            },
        },
        BTreeMap::from([
            ("root.nodes.review.nodes.assess".into(), allowance(false)),
            ("root.nodes.review.nodes.verify".into(), allowance(true)),
            (
                "root.nodes.review_other.nodes.assess".into(),
                allowance(false),
            ),
        ]),
    )
    .unwrap()
    .with_call_limits(BTreeMap::from([("root.nodes.review".into(), 2)]))
    .unwrap();
    spend(&mut ledger, "root.nodes.review.nodes.assess", 1, 4);
    assert!(
        ledger
            .prepare("root.nodes.review.nodes.assess", 2)
            .unwrap_err()
            .contains("Call root.nodes.review")
    );
    spend(&mut ledger, "root.nodes.review_other.nodes.assess", 2, 4);
    spend(&mut ledger, "root.nodes.review.nodes.verify", 3, 4);
    assert_eq!(ledger.committed_tokens(), 12, "children share parent spend");
    assert!(ledger.prepare("root.nodes.review.nodes.verify", 4).is_err());
}

#[test]
fn concurrent_reservations_protect_tokens_before_either_attempt_begins() {
    let mut ledger = budget(3, 90, 1000);
    let first = ledger.prepare("implement", 1).unwrap();
    assert!(
        ledger
            .prepare("implement", 1)
            .unwrap_err()
            .contains("token limit")
    );
    let verifier = ledger.prepare("review.verify", 1).unwrap();
    assert_eq!(ledger.reserved_tokens(), 70);
    ledger.begin(&first.id, 2).unwrap();
    ledger.begin(&verifier.id, 2).unwrap();
    ledger.settle(&first.id, 10).unwrap();
    ledger.settle(&verifier.id, 20).unwrap();
    assert_eq!(ledger.committed_tokens(), 30);
}

#[test]
fn deadline_protection_survives_waiting_and_backwards_clocks_fail_closed() {
    let mut ledger = budget(3, 200, 1000);
    assert!(
        ledger
            .prepare("implement", 701)
            .unwrap_err()
            .contains("deadline")
    );
    let held = ledger.prepare("implement", 700).unwrap();
    assert_eq!(held.deadline_unix_ms, 800);
    assert!(ledger.begin(&held.id, 800).is_err());
    assert!(ledger.begin(&held.id, 699).is_err());
    ledger.release(&held.id).unwrap();
    assert!(ledger.prepare("review.verify", 699).is_err());
    spend(&mut ledger, "review.verify", 800, 10);
}

#[test]
fn only_unstarted_work_can_release_capacity_and_duplicate_settlement_is_exact() {
    let mut ledger = budget(2, 100, 1000);
    let first = ledger.prepare("implement", 1).unwrap();
    ledger.release(&first.id).unwrap();
    ledger.release(&first.id).unwrap();
    assert_eq!(ledger.reserved_tokens(), 0);
    assert!(ledger.begin(&first.id, 2).is_err());
    let second = ledger.prepare("implement", 2).unwrap();
    assert_ne!(first.id, second.id);
    ledger.begin(&second.id, 2).unwrap();
    assert!(ledger.release(&second.id).is_err());
    ledger.settle(&second.id, 12).unwrap();
    ledger.settle(&second.id, 12).unwrap();
    assert!(ledger.settle(&second.id, 11).is_err());
    assert_eq!(ledger.committed_tokens(), 12);
    assert_eq!(ledger.begun_attempts(), 1);
}

#[test]
fn provider_overrun_is_fully_charged_and_prevents_even_prepared_dispatch() {
    let mut ledger = budget(3, 200, 1000);
    let first = ledger.prepare("implement", 1).unwrap();
    let verifier = ledger.prepare("review.verify", 1).unwrap();
    ledger.begin(&first.id, 1).unwrap();
    ledger.settle(&first.id, 41).unwrap();
    assert!(ledger.breached());
    assert_eq!(ledger.committed_tokens(), 41);
    assert!(ledger.begin(&verifier.id, 2).is_err());
    assert!(ledger.prepare("implement", 2).is_err());
    ledger.release(&verifier.id).unwrap();
    assert_eq!(ledger.reserved_tokens(), 0);
}

#[test]
fn replaying_recorded_transitions_preserves_ids_charges_and_remaining_capacity() {
    fn history() -> (TaskBudget, String) {
        let mut ledger = budget(3, 120, 1000);
        spend(&mut ledger, "implement", 1, 30);
        let abandoned = ledger.prepare("implement", 2).unwrap();
        ledger.begin(&abandoned.id, 2).unwrap();
        // Missing usage after a crash is conservatively charged at the original reservation.
        ledger.settle(&abandoned.id, abandoned.tokens).unwrap();
        (ledger, abandoned.id)
    }
    let (mut live, live_id) = history();
    let (mut replay, replay_id) = history();
    assert_eq!(live_id, replay_id);
    assert_eq!(live.committed_tokens(), 70);
    assert_eq!(
        live.prepare("review.verify", 3).unwrap(),
        replay.prepare("review.verify", 3).unwrap()
    );
    assert!(replay.prepare("implement", 3).is_err());
}

#[test]
fn command_workers_consume_attempts_and_wall_capacity_with_zero_tokens() {
    let mut ledger = TaskBudget::new(
        TaskLimitsV1 {
            tokens: 0,
            max_attempts: 2,
            deadline_unix_ms: 100,
            verification: VerificationReserveV1 {
                tokens: 0,
                attempts: 1,
                wall_ms: 30,
            },
        },
        BTreeMap::from([
            (
                "write".into(),
                NodeAllowance {
                    tokens_per_attempt: 0,
                    wall_ms_per_attempt: 20,
                    max_attempts: 2,
                    verification_attempts: 0,
                },
            ),
            (
                "verify".into(),
                NodeAllowance {
                    tokens_per_attempt: 0,
                    wall_ms_per_attempt: 30,
                    max_attempts: 1,
                    verification_attempts: 1,
                },
            ),
        ]),
    )
    .unwrap();
    spend(&mut ledger, "write", 1, 0);
    assert!(ledger.prepare("write", 2).is_err());
    spend(&mut ledger, "verify", 2, 0);
    assert_eq!(ledger.begun_attempts(), 2);
    assert_eq!(ledger.committed_tokens(), 0);
}
