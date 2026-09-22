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

fn install(
    budget: &mut TaskBudget,
    allowances: BTreeMap<String, NodeAllowance>,
    call_limits: BTreeMap<String, u32>,
    now_unix_ms: u64,
    preparation: bool,
) -> Result<(), String> {
    budget.install_graph_with_owned_templates(
        allowances,
        call_limits,
        BTreeMap::new(),
        BTreeMap::new(),
        now_unix_ms,
        preparation,
    )
}

fn spend(budget: &mut TaskBudget, node: &str, now: u64, tokens: u64) -> String {
    let reservation = budget.prepare(node, now).unwrap();
    budget.begin(&reservation.id, now).unwrap();
    budget
        .settle_exact(&reservation.id, u128::from(tokens))
        .unwrap();
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
fn running_usage_preserves_other_reservations_and_cannot_be_refunded_at_settlement() {
    let mut ledger = budget(3, 100, 1000);
    let author = ledger.prepare("implement", 1).unwrap();
    let verifier = ledger.prepare("review.verify", 1).unwrap();
    assert!(ledger.observe_charge_exact(&author.id, 10).is_err());
    ledger.begin(&author.id, 2).unwrap();
    for amount in [12, 12, 4] {
        ledger.observe_charge_exact(&author.id, amount).unwrap();
    }
    assert_eq!(ledger.committed_tokens(), 12);
    assert_eq!(ledger.reserved_tokens(), 58);
    assert_eq!(ledger.remaining_limits().tokens, 30);
    assert!(ledger.prepare("implement", 3).is_err());
    assert!(ledger.release(&author.id).is_err());
    assert!(ledger.settle_exact(&author.id, 7).is_err());
    assert_eq!(ledger.committed_tokens(), 12);
    assert_eq!(ledger.reserved_tokens(), 58);
    ledger.settle_exact(&author.id, 15).unwrap();
    ledger.settle_exact(&author.id, 15).unwrap();
    assert_eq!(ledger.committed_tokens(), 15);
    assert_eq!(ledger.reserved_tokens(), 30);
    ledger.begin(&verifier.id, 4).unwrap();
    ledger.observe_charge_exact(&verifier.id, 9).unwrap();
    ledger.settle_exact(&verifier.id, 9).unwrap();
    assert_eq!(ledger.committed_tokens(), 24);
    assert_eq!(ledger.reserved_tokens(), 0);
    assert_eq!(ledger.begun_attempts(), 2);
}

#[test]
fn running_overrun_stops_prepared_work_and_remains_payable_after_the_deadline() {
    let mut ledger = budget(3, 200, 1000);
    let author = ledger.prepare("implement", 1).unwrap();
    let verifier = ledger.prepare("review.verify", 1).unwrap();
    ledger.begin(&author.id, 2).unwrap();
    ledger.observe_charge_exact(&author.id, 55).unwrap();
    assert_eq!(ledger.committed_tokens(), 55);
    assert_eq!(ledger.reserved_tokens(), 30);
    assert!(ledger.breached());
    assert!(ledger.begin(&verifier.id, 3).is_err());
    assert!(ledger.prepare("implement", 3).is_err());
    assert!(ledger.invalidate_plan(1001).is_err());
    ledger.observe_charge_exact(&author.id, 58).unwrap();
    ledger.settle_exact(&author.id, 58).unwrap();
    ledger.release(&verifier.id).unwrap();
    ledger.invalidate_plan(1002).unwrap();
    ledger.observe_charge_exact(&author.id, 63).unwrap();
    assert_eq!(ledger.committed_tokens(), 63);
    assert_eq!(ledger.reserved_tokens(), 0);
    assert_eq!(ledger.begun_attempts(), 1);
}

#[test]
fn late_provider_usage_keeps_conservative_spend_and_leaves_other_reservations_intact() {
    let mut ledger = budget(3, 200, 1000);
    let abandoned = ledger.prepare("implement", 1).unwrap();
    ledger.begin(&abandoned.id, 2).unwrap();
    ledger
        .settle_exact(&abandoned.id, u128::from(abandoned.tokens))
        .unwrap();
    let verifier = ledger.prepare("review.verify", 3).unwrap();
    ledger.observe_charge_exact(&abandoned.id, 20).unwrap();
    assert_eq!(ledger.committed_tokens(), 40);
    ledger.observe_charge_exact(&abandoned.id, 60).unwrap();
    ledger.observe_charge_exact(&abandoned.id, 60).unwrap();
    assert_eq!(ledger.committed_tokens(), 60);
    assert_eq!(ledger.reserved_tokens(), 30);
    assert!(
        ledger.begin(&verifier.id, 4).is_err(),
        "observed overrun fences further dispatch"
    );
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
    ledger.settle_exact(&first.id, 10).unwrap();
    ledger.settle_exact(&verifier.id, 20).unwrap();
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
    ledger.settle_exact(&second.id, 12).unwrap();
    ledger.settle_exact(&second.id, 12).unwrap();
    assert!(ledger.settle_exact(&second.id, 11).is_err());
    assert_eq!(ledger.committed_tokens(), 12);
    assert_eq!(ledger.begun_attempts(), 1);
}

#[test]
fn provider_overrun_is_fully_charged_and_prevents_even_prepared_dispatch() {
    let mut ledger = budget(3, 200, 1000);
    let first = ledger.prepare("implement", 1).unwrap();
    let verifier = ledger.prepare("review.verify", 1).unwrap();
    ledger.begin(&first.id, 1).unwrap();
    ledger.settle_exact(&first.id, 41).unwrap();
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
        ledger
            .settle_exact(&abandoned.id, u128::from(abandoned.tokens))
            .unwrap();
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

fn planning_budget(tokens: u64, attempts: u32, deadline: u64) -> TaskBudget {
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
        BTreeMap::from([(
            "root.nodes.plan".into(),
            NodeAllowance {
                tokens_per_attempt: 40,
                wall_ms_per_attempt: 100,
                max_attempts: 2,
                verification_attempts: 0,
            },
        )]),
    )
    .unwrap()
    .with_deferred_verification()
    .unwrap()
}
fn business_allowances() -> BTreeMap<String, NodeAllowance> {
    BTreeMap::from([
        (
            "root.nodes.implement".into(),
            NodeAllowance {
                tokens_per_attempt: 40,
                wall_ms_per_attempt: 100,
                max_attempts: 2,
                verification_attempts: 0,
            },
        ),
        (
            "root.nodes.verify".into(),
            NodeAllowance {
                tokens_per_attempt: 30,
                wall_ms_per_attempt: 200,
                max_attempts: 1,
                verification_attempts: 1,
            },
        ),
    ])
}

#[test]
fn planning_protects_future_verification_and_cannot_reset_spend_at_the_execution_barrier() {
    for mut blocked in [
        planning_budget(60, 3, 1000),
        planning_budget(200, 1, 1000),
        planning_budget(200, 3, 250),
    ] {
        assert!(
            blocked.prepare("root.nodes.plan", 1).is_err(),
            "Planner consumed a future verifier's reservation"
        );
        assert_eq!(blocked.begun_attempts(), 0);
    }
    let mut budget = planning_budget(120, 3, 1000);
    let planner = spend(&mut budget, "root.nodes.plan", 1, 20);
    assert_eq!(budget.remaining_limits().tokens, 100);
    assert_eq!(budget.remaining_limits().max_attempts, 2);
    install(
        &mut budget,
        business_allowances(),
        BTreeMap::from([("root".into(), 3)]),
        2,
        false,
    )
    .unwrap();
    assert_eq!(budget.committed_tokens(), 20);
    assert_eq!(budget.begun_attempts(), 1);
    assert!(
        install(
            &mut budget,
            business_allowances(),
            BTreeMap::new(),
            3,
            false
        )
        .is_err()
    );
    let implementation = spend(&mut budget, "root.nodes.implement", 3, 10);
    assert_ne!(planner, implementation);
    assert!(
        budget.prepare("root.nodes.implement", 4).is_err(),
        "Earlier Planner Attempt was reset"
    );
    spend(&mut budget, "root.nodes.verify", 4, 5);
    assert_eq!(budget.begun_attempts(), 3);
    assert_eq!(budget.committed_tokens(), 35);
    budget.observe_charge_exact(&planner, 50).unwrap();
    assert_eq!(budget.committed_tokens(), 65);
    assert!(budget.breached());
}

#[test]
fn planning_barrier_requires_settled_attempts_and_keeps_expiry_and_late_overrun_authority() {
    let mut budget = planning_budget(200, 4, 1000);
    let reservation = budget.prepare("root.nodes.plan", 1).unwrap();
    assert!(
        install(
            &mut budget,
            business_allowances(),
            BTreeMap::new(),
            2,
            false
        )
        .is_err()
    );
    budget.begin(&reservation.id, 2).unwrap();
    assert!(
        install(
            &mut budget,
            business_allowances(),
            BTreeMap::new(),
            3,
            false
        )
        .is_err()
    );
    budget.settle_exact(&reservation.id, 10).unwrap();
    assert!(
        install(
            &mut budget,
            business_allowances(),
            BTreeMap::new(),
            1000,
            false
        )
        .is_err()
    );
    assert!(
        install(
            &mut budget,
            business_allowances(),
            BTreeMap::new(),
            900,
            false
        )
        .is_err()
    );
    install(
        &mut budget,
        business_allowances(),
        BTreeMap::from([("root".into(), 3)]),
        3,
        false,
    )
    .unwrap();
    assert_eq!(budget.remaining_limits().deadline_unix_ms, 1000);
    budget.observe_charge_exact(&reservation.id, 41).unwrap();
    assert!(
        budget.prepare("root.nodes.implement", 4).is_err(),
        "Late Planner overrun did not fence new work"
    );
    assert!(
        budget.begin(&reservation.id, 4).is_err(),
        "Old settled reservation was restarted"
    );
}

#[test]
fn source_revision_retains_spend_reservations_late_usage_and_the_original_deadline() {
    let mut value = budget(5, 200, 1000);
    let author = spend(&mut value, "implement", 1, 20);
    let verify = spend(&mut value, "review.verify", 2, 10);
    value.invalidate_plan(3).unwrap();
    assert_eq!(value.begun_attempts(), 2);
    assert_eq!(value.committed_tokens(), 30);
    assert_eq!(value.remaining_limits().deadline_unix_ms, 1000);
    assert!(value.prepare("implement", 4).is_err());
    let allow = BTreeMap::from([
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
    ]);
    install(&mut value, allow.clone(), BTreeMap::new(), 4, false).unwrap();
    value.observe_charge_exact(&author, 25).unwrap();
    value.observe_charge_exact(&verify, 15).unwrap();
    assert_eq!(value.committed_tokens(), 40);
    let next = spend(&mut value, "implement", 5, 10);
    assert_ne!(next, author);
    assert_ne!(next, verify);
    assert_eq!(value.begun_attempts(), 3);
    spend(&mut value, "implement", 6, 10);
    assert!(value.prepare("implement", 7).is_err());
    spend(&mut value, "review.verify", 7, 10);
    assert_eq!(value.begun_attempts(), 5);
    value.invalidate_plan(8).unwrap();
    assert!(install(&mut value, allow, BTreeMap::new(), 9, false).is_err());
    assert_eq!(value.begun_attempts(), 5);
    assert_eq!(value.committed_tokens(), 70);
}
#[test]
fn source_revision_cannot_release_pending_work_overruns_or_expired_resources() {
    let mut value = budget(5, 200, 1000);
    let pending = value.prepare("implement", 1).unwrap();
    assert!(value.invalidate_plan(2).is_err());
    value.begin(&pending.id, 2).unwrap();
    assert!(value.invalidate_plan(3).is_err());
    value.settle_exact(&pending.id, 40).unwrap();
    value.invalidate_plan(4).unwrap();
    assert!(value.invalidate_plan(3).is_err());
    value.observe_charge_exact(&pending.id, 41).unwrap();
    assert!(value.breached());
    assert!(install(&mut value, BTreeMap::new(), BTreeMap::new(), 5, true).is_err());
    let mut value = budget(5, 200, 1000);
    spend(&mut value, "implement", 1, 10);
    value.invalidate_plan(1001).unwrap();
    assert!(install(&mut value, BTreeMap::new(), BTreeMap::new(), 1002, true).is_err());
    assert_eq!(value.begun_attempts(), 1);
    assert_eq!(value.committed_tokens(), 10);
}

#[test]
fn full_native_usage_is_charged_above_u64_totals_without_releasing_siblings() {
    let mut ledger = budget(4, 200, 1000);
    spend(&mut ledger, "implement", 1, 7);
    let active = ledger.prepare("implement", 2).unwrap();
    ledger.begin(&active.id, 2).unwrap();
    let sibling = ledger.prepare("review.verify", 2).unwrap();
    for amount in [u64::MAX, u64::MAX, 1] {
        ledger
            .observe_charge_exact(&active.id, u128::from(amount))
            .unwrap();
        assert_eq!(ledger.committed_tokens(), u128::from(u64::MAX) + 7);
        assert_eq!(ledger.reserved_tokens(), 30);
        assert!(ledger.breached());
        assert_eq!(ledger.remaining_limits().tokens, 0);
        assert!(ledger.begin(&sibling.id, 3).is_err());
    }
    ledger
        .settle_exact(&active.id, u128::from(u64::MAX))
        .unwrap();
    ledger
        .settle_exact(&active.id, u128::from(u64::MAX))
        .unwrap();
    ledger
        .observe_charge_exact(&active.id, u128::from(u64::MAX))
        .unwrap();
    assert_eq!(ledger.committed_tokens(), 18_446_744_073_709_551_622_u128);
    assert_eq!(ledger.reserved_tokens(), 30);
    assert!(ledger.release(&active.id).is_err());
    ledger.release(&sibling.id).unwrap();
    assert_eq!(ledger.reserved_tokens(), 0);
    assert_eq!(ledger.begun_attempts(), 2);
}

#[test]
fn exact_attempt_usage_survives_settlement_and_plan_invalidation() {
    use review_attempt::task_budget::TaskTokenScope;
    let mut ledger = budget(4, 200, 1000)
        .with_token_scopes(BTreeMap::from([(
            "round.one".into(),
            TaskTokenScope {
                tokens: 200,
                members: ["implement".into(), "review".into()].into(),
            },
        )]))
        .unwrap();
    spend(&mut ledger, "implement", 1, 7);
    let active = ledger.prepare("implement", 2).unwrap();
    ledger.begin(&active.id, 2).unwrap();
    let sibling = ledger.prepare("review.verify", 2).unwrap();
    let actual = u128::from(u64::MAX) + 7;
    ledger.observe_charge_exact(&active.id, actual).unwrap();
    ledger.observe_charge_exact(&active.id, 1).unwrap();
    assert_eq!(ledger.committed_tokens(), actual + 7);
    assert_eq!(ledger.reserved_tokens(), 30);
    assert_eq!(active.tokens, 40);
    assert_eq!(active.deadline_unix_ms, 102);
    assert!(ledger.begin(&sibling.id, 3).is_err());
    assert!(
        ledger
            .settle_exact(&active.id, u128::from(u64::MAX))
            .is_err()
    );
    ledger.settle_exact(&active.id, actual).unwrap();
    ledger.settle_exact(&active.id, actual).unwrap();
    ledger.release(&sibling.id).unwrap();
    ledger.invalidate_plan(4).unwrap();
    ledger.observe_charge_exact(&active.id, actual + 1).unwrap();
    ledger.observe_charge_exact(&active.id, actual).unwrap();
    assert_eq!(ledger.committed_tokens(), actual + 8);
    assert_eq!(ledger.scope_committed_tokens("round.one"), Some(actual + 8));
    assert_eq!(ledger.reserved_tokens(), 0);
    assert_eq!(ledger.begun_attempts(), 2);
    assert_eq!(ledger.remaining_limits().deadline_unix_ms, 1000);
    assert!(ledger.breached());
    assert!(install(&mut ledger, BTreeMap::new(), BTreeMap::new(), 5, true).is_err());
}

#[test]
fn late_exact_total_overflow_keeps_every_scope_and_fails_closed() {
    use review_attempt::task_budget::TaskTokenScope;
    let mut ledger = budget(4, 200, 1000)
        .with_token_scopes(BTreeMap::from([(
            "round.one".into(),
            TaskTokenScope {
                tokens: 200,
                members: ["implement".into()].into(),
            },
        )]))
        .unwrap();
    let first = spend(&mut ledger, "implement", 1, 0);
    let second = spend(&mut ledger, "implement", 2, 1);
    ledger.observe_charge_exact(&first, u128::MAX - 1).unwrap();
    assert!(ledger.observe_charge_exact(&second, 2).is_err());
    assert_eq!(ledger.committed_tokens(), u128::MAX);
    assert_eq!(ledger.scope_committed_tokens("round.one"), Some(u128::MAX));
    assert_eq!(ledger.reserved_tokens(), 0);
    assert!(ledger.breached());
    assert!(ledger.prepare("review.verify", 3).is_err());
}
