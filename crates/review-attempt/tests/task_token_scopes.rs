use review_attempt::task_budget::{NodeAllowance, TaskBudget, TaskTokenScope};
use review_core::task::{TaskLimitsV1, VerificationReserveV1};
use std::collections::{BTreeMap, BTreeSet};

fn nodes() -> BTreeMap<String, NodeAllowance> {
    ["root.review.a", "root.review.b", "root.reviewx.a"]
        .into_iter()
        .map(|name| {
            (
                name.into(),
                NodeAllowance {
                    tokens_per_attempt: 40,
                    wall_ms_per_attempt: 10,
                    max_attempts: 4,
                    verification_attempts: 0,
                },
            )
        })
        .collect()
}
fn scope(tokens: u64, members: &[&str]) -> TaskTokenScope {
    TaskTokenScope {
        tokens,
        members: members.iter().map(|n| (*n).into()).collect::<BTreeSet<_>>(),
    }
}
fn budget(scopes: BTreeMap<String, TaskTokenScope>) -> TaskBudget {
    TaskBudget::new(
        TaskLimitsV1 {
            tokens: 1000,
            max_attempts: 20,
            deadline_unix_ms: 1000,
            verification: VerificationReserveV1 {
                tokens: 0,
                attempts: 0,
                wall_ms: 0,
            },
        },
        nodes(),
    )
    .unwrap()
    .with_token_scopes(scopes)
    .unwrap()
}
fn install(
    budget: &mut TaskBudget,
    allowances: BTreeMap<String, NodeAllowance>,
    call_limits: BTreeMap<String, u32>,
    token_scopes: BTreeMap<String, TaskTokenScope>,
    now_unix_ms: u64,
    preparation: bool,
) -> Result<(), String> {
    budget.install_graph_with_owned_templates(
        allowances,
        call_limits,
        token_scopes,
        BTreeMap::new(),
        now_unix_ms,
        preparation,
    )
}
fn spent(budget: &mut TaskBudget, node: &str, now: u64, tokens: u64) -> String {
    let reservation = budget.prepare(node, now).unwrap();
    budget.begin(&reservation.id, now).unwrap();
    budget
        .settle_exact(&reservation.id, u128::from(tokens))
        .unwrap();
    reservation.id
}

#[test]
fn static_node_cap_aggregates_retries_and_failed_reservation_is_atomic() {
    let mut b = budget(BTreeMap::from([(
        "round.r1.node_a".into(),
        scope(50, &["root.review.a"]),
    )]));
    let unused = b.prepare("root.review.a", 1).unwrap();
    assert_eq!(b.scope_reserved_tokens("round.r1.node_a"), Some(40));
    b.release(&unused.id).unwrap();
    spent(&mut b, "root.review.a", 2, 15);
    assert!(
        b.prepare("root.review.a", 3)
            .unwrap_err()
            .contains("round.r1.node_a")
    );
    assert_eq!(b.begun_attempts(), 1);
    assert_eq!(b.reserved_tokens(), 0);
    assert_eq!(b.scope_committed_tokens("round.r1.node_a"), Some(15));
    spent(&mut b, "root.review.b", 3, 20);
    assert_eq!(b.committed_tokens(), 35);
}

#[test]
fn parallel_children_share_one_pool_and_running_usage_retains_the_unspent_reservation() {
    let mut b = budget(BTreeMap::from([(
        "round.r1.children".into(),
        scope(90, &["root.review"]),
    )]));
    let a = b.prepare("root.review.a", 1).unwrap();
    let other = b.prepare("root.review.b", 1).unwrap();
    assert!(b.prepare("root.review.a", 1).is_err());
    assert_eq!(b.reserved_tokens(), 80);
    b.begin(&a.id, 1).unwrap();
    b.settle_exact(&a.id, 7).unwrap();
    let retry = b.prepare("root.review.a", 2).unwrap();
    b.begin(&other.id, 2).unwrap();
    b.observe_charge_exact(&other.id, 30).unwrap();
    assert_eq!(b.committed_tokens(), 37);
    assert_eq!(b.scope_committed_tokens("round.r1.children"), Some(37));
    assert_eq!(b.scope_reserved_tokens("round.r1.children"), Some(50));
    assert!(b.prepare("root.review.b", 2).is_err());
    b.release(&retry.id).unwrap();
    b.settle_exact(&other.id, 30).unwrap();
    assert_eq!(b.scope_reserved_tokens("round.r1.children"), Some(0));
    assert_eq!(b.committed_tokens(), 37);
}

#[test]
fn token_scope_prefixes_match_address_segments_and_overlaps_never_double_charge() {
    let mut b = budget(BTreeMap::from([
        (
            "round.r1.pool".into(),
            scope(90, &["root.review", "root.review.a"]),
        ),
        ("round.r1.node".into(), scope(50, &["root.review.a"])),
    ]));
    spent(&mut b, "root.reviewx.a", 1, 30);
    assert_eq!(b.scope_committed_tokens("round.r1.pool"), Some(0));
    spent(&mut b, "root.review.a", 2, 15);
    assert_eq!(b.committed_tokens(), 45);
    assert_eq!(b.scope_committed_tokens("round.r1.pool"), Some(15));
    assert_eq!(b.scope_committed_tokens("round.r1.node"), Some(15));
    assert!(b.prepare("root.review.a", 3).is_err());
}

#[test]
fn shared_pool_protects_its_verifier_before_admitting_other_work() {
    let mut allowances = nodes();
    let verifier = allowances.get_mut("root.review.b").unwrap();
    verifier.tokens_per_attempt = 30;
    verifier.verification_attempts = 1;
    let mut b = TaskBudget::new(
        TaskLimitsV1 {
            tokens: 1000,
            max_attempts: 20,
            deadline_unix_ms: 1000,
            verification: VerificationReserveV1 {
                tokens: 30,
                attempts: 1,
                wall_ms: 10,
            },
        },
        allowances,
    )
    .unwrap()
    .with_token_scopes(BTreeMap::from([(
        "pool".into(),
        scope(60, &["root.review"]),
    )]))
    .unwrap();
    assert!(
        b.prepare("root.review.a", 1)
            .unwrap_err()
            .contains("still-required verification")
    );
    spent(&mut b, "root.review.b", 1, 1);
    spent(&mut b, "root.review.a", 2, 4);
    assert_eq!(b.scope_committed_tokens("pool"), Some(5));
}

#[test]
fn graph_replacement_retains_retired_scope_usage_and_the_original_task_allowance() {
    let first = BTreeMap::from([("round.r1".into(), scope(50, &["root.review"]))]);
    let second = BTreeMap::from([("round.r2".into(), scope(50, &["root.review"]))]);
    let mut b = budget(first.clone());
    let old = spent(&mut b, "root.review.a", 1, 10);
    b.invalidate_plan(2).unwrap();
    install(&mut b, nodes(), BTreeMap::new(), second, 2, false).unwrap();
    spent(&mut b, "root.review.a", 3, 5);
    b.observe_charge_exact(&old, 25).unwrap();
    b.observe_charge_exact(&old, 25).unwrap();
    b.observe_charge_exact(&old, 12).unwrap();
    assert_eq!(b.committed_tokens(), 30);
    assert_eq!(b.begun_attempts(), 2);
    assert_eq!(b.scope_committed_tokens("round.r1"), Some(25));
    assert_eq!(b.scope_committed_tokens("round.r2"), Some(5));
    assert_eq!(b.remaining_limits().deadline_unix_ms, 1000);
    b.invalidate_plan(4).unwrap();
    let mut changed = first.clone();
    changed.get_mut("round.r1").unwrap().tokens = 90;
    assert!(
        install(&mut b, nodes(), BTreeMap::new(), changed, 4, false)
            .unwrap_err()
            .contains("captured authority")
    );
    install(&mut b, nodes(), BTreeMap::new(), first, 4, false).unwrap();
    assert!(
        b.prepare("root.review.a", 5).is_err(),
        "the earlier scope is not replenished by reinstallation"
    );
    assert_eq!(b.committed_tokens(), 30);
}

#[test]
fn late_overrun_charges_every_original_scope_and_stops_already_prepared_work() {
    let mut b = budget(BTreeMap::from([(
        "pool".into(),
        scope(90, &["root.review"]),
    )]));
    let settled = spent(&mut b, "root.review.a", 1, 2);
    let pending = b.prepare("root.review.b", 2).unwrap();
    b.observe_charge_exact(&settled, 60).unwrap();
    assert_eq!(b.committed_tokens(), 60);
    assert_eq!(b.scope_committed_tokens("pool"), Some(60));
    assert_eq!(b.scope_reserved_tokens("pool"), Some(40));
    assert!(b.begin(&pending.id, 2).is_err());
    b.release(&pending.id).unwrap();
    assert_eq!(b.scope_reserved_tokens("pool"), Some(0));
}

#[test]
fn a_scope_ceiling_is_not_compared_to_the_tasks_remaining_token_count() {
    let scopes = BTreeMap::from([("pool".into(), scope(1000, &["root.review"]))]);
    let mut b = budget(scopes.clone());
    spent(&mut b, "root.review.a", 1, 20);
    b.invalidate_plan(2).unwrap();
    install(&mut b, nodes(), BTreeMap::new(), scopes, 2, false).unwrap();
    spent(&mut b, "root.review.b", 3, 10);
    assert_eq!(b.committed_tokens(), 30);
    assert_eq!(b.scope_committed_tokens("pool"), Some(30));
    assert_eq!(b.remaining_limits().tokens, 970);
}
