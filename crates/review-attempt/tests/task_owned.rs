use review_attempt::task_budget::{NodeAllowance, OwnedNodeAllowance, TaskBudget, TaskTokenScope};
use review_core::task::{TaskLimitsV1, VerificationReserveV1};
use std::collections::{BTreeMap, BTreeSet};

const OWNER: &str = "root.nodes.scatter";

fn templates() -> BTreeMap<String, OwnedNodeAllowance> {
    BTreeMap::from([(
        OWNER.into(),
        OwnedNodeAllowance {
            allowance: NodeAllowance {
                tokens_per_attempt: 10,
                wall_ms_per_attempt: 100,
                max_attempts: 2,
                verification_attempts: 0,
            },
            max_children: 3,
        },
    )])
}

fn scopes(round: u32) -> BTreeMap<String, TaskTokenScope> {
    BTreeMap::from([(
        format!("review.round{round}.fanout"),
        TaskTokenScope {
            tokens: 20,
            members: BTreeSet::from([OWNER.into()]),
        },
    )])
}

fn budget() -> TaskBudget {
    TaskBudget::new(
        TaskLimitsV1 {
            tokens: 100,
            max_attempts: 8,
            deadline_unix_ms: 10_000,
            verification: VerificationReserveV1 {
                tokens: 0,
                attempts: 0,
                wall_ms: 0,
            },
        },
        BTreeMap::new(),
    )
    .unwrap()
    .with_call_limits(BTreeMap::from([("root".into(), 2)]))
    .unwrap()
    .with_owned_templates(templates())
    .unwrap()
    .with_token_scopes(scopes(1))
    .unwrap()
}

fn children() -> Vec<String> {
    (1..=3).map(|n| format!("{OWNER}.slice{n}")).collect()
}

#[test]
fn nonreservable_owner_registers_one_bounded_set_without_minting_credit() {
    let mut budget = budget();
    assert!(budget.prepare(OWNER, 1).is_err());
    assert!(budget.prepare(&children()[0], 1).is_err());
    let mut invalid = children();
    invalid[2] = "root.other.slice3".into();
    assert!(budget.register_owned_children(OWNER, &invalid).is_err());
    assert!(
        budget.prepare(&children()[0], 1).is_err(),
        "registration is atomic"
    );
    budget.register_owned_children(OWNER, &children()).unwrap();
    assert_eq!(budget.begun_attempts(), 0);
    assert_eq!(budget.reserved_tokens(), 0);
    assert_eq!(budget.remaining_limits().tokens, 100);
    let first = budget.prepare(&children()[0], 1).unwrap();
    budget.begin(&first.id, 2).unwrap();
    budget.settle_exact(&first.id, 3).unwrap();
    budget.register_owned_children(OWNER, &children()).unwrap();
    assert_eq!(
        budget.begun_attempts(),
        1,
        "duplicate cannot reset child accounts"
    );
    assert!(
        budget
            .register_owned_children(OWNER, &children()[..2])
            .is_err()
    );
    let second = budget.prepare(&children()[1], 3).unwrap();
    budget.begin(&second.id, 3).unwrap();
    budget.settle_exact(&second.id, 3).unwrap();
    assert!(
        budget
            .prepare(&children()[2], 4)
            .unwrap_err()
            .contains("Call root")
    );
    assert_eq!(
        budget.scope_committed_tokens("review.round1.fanout"),
        Some(6)
    );
}

#[test]
fn owned_overrun_stops_prepared_sibling_and_exact_usage_remains_payable() {
    let mut budget = budget();
    budget.register_owned_children(OWNER, &children()).unwrap();
    let first = budget.prepare(&children()[0], 1).unwrap();
    let second = budget.prepare(&children()[1], 1).unwrap();
    budget.begin(&first.id, 2).unwrap();
    let exact = u128::from(u64::MAX) + 7;
    budget.observe_charge_exact(&first.id, exact).unwrap();
    assert!(budget.begin(&second.id, 3).is_err());
    budget.release(&second.id).unwrap();
    assert!(budget.settle_exact(&first.id, 7).is_err());
    budget.settle_exact(&first.id, exact).unwrap();
    budget.observe_charge_exact(&first.id, exact + 1).unwrap();
    assert_eq!(budget.committed_tokens(), exact + 1);
    assert_eq!(
        budget.scope_committed_tokens("review.round1.fanout"),
        Some(exact + 1)
    );
    assert_eq!(budget.begun_attempts(), 1);
    assert_eq!(budget.reserved_tokens(), 0);
    assert!(budget.breached());
}

#[test]
fn handoff_reuses_node_names_but_keeps_original_reservation_scopes() {
    let mut budget = budget();
    budget.register_owned_children(OWNER, &children()).unwrap();
    let old = budget.prepare(&children()[0], 1).unwrap();
    budget.begin(&old.id, 2).unwrap();
    budget.settle_exact(&old.id, 2).unwrap();
    budget.invalidate_plan(3).unwrap();
    budget
        .install_graph_with_owned_templates(
            BTreeMap::new(),
            BTreeMap::from([("root".into(), 2)]),
            scopes(2),
            templates(),
            4,
            false,
        )
        .unwrap();
    assert!(budget.prepare(&children()[0], 5).is_err());
    budget.register_owned_children(OWNER, &children()).unwrap();
    let new = budget.prepare(&children()[0], 5).unwrap();
    assert_ne!(old.id, new.id);
    budget.observe_charge_exact(&old.id, 7).unwrap();
    assert_eq!(
        budget.scope_committed_tokens("review.round1.fanout"),
        Some(7)
    );
    assert_eq!(
        budget.scope_committed_tokens("review.round2.fanout"),
        Some(0)
    );
    assert_eq!(
        budget.scope_reserved_tokens("review.round2.fanout"),
        Some(10)
    );
    budget.begin(&new.id, 6).unwrap();
    budget.settle_exact(&new.id, 3).unwrap();
    assert_eq!(budget.committed_tokens(), 10);
    assert_eq!(budget.begun_attempts(), 2);
    assert_eq!(budget.remaining_limits().deadline_unix_ms, 10_000);
}

#[test]
fn registration_after_deadline_retains_missing_items_but_cannot_dispatch() {
    let mut budget = budget();
    budget.register_owned_children(OWNER, &children()).unwrap();
    assert!(budget.prepare(&children()[0], 10_000).is_err());
    assert_eq!(budget.begun_attempts(), 0);
    assert_eq!(budget.committed_tokens(), 0);
    assert_eq!(budget.reserved_tokens(), 0);
}
