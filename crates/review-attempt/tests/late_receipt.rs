//! `fixtures/adversarial/late-receipt.md`, made executable.
//!
//! The scenario from the case: attempt A1 times out while its process is still alive, the kernel
//! fences it and starts A2, A2 completes normally — and *then* A1 delivers. Its finding is a
//! plausible one from a real reviewer, which is exactly why nothing about it looks wrong.
//!
//! Three things must hold, and the case names all three: A1's result is quarantined and can
//! never be selected; its cost is still charged; and replay with A1's delivery moved to any
//! position produces the same outcome.

use std::collections::BTreeMap;

use review_attempt::{
    AttemptId, AttemptLedger, AttemptState, Budget, BudgetLedger, BudgetScope, ExactReceipt,
    Selection,
};

fn ledger() -> AttemptLedger {
    AttemptLedger::scoped("round", BTreeMap::new())
}

fn deliver(attempts: &mut AttemptLedger, attempt: &AttemptId, cost: u128) -> Selection {
    attempts
        .admit_exact(&ExactReceipt {
            attempt: attempt.clone(),
            cost,
        })
        .unwrap()
}

/// One delivery in a run's event order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    DispatchA1,
    FenceA1,
    DispatchA2,
    DeliverA2,
    /// The late one. Moved around by the replay test.
    DeliverA1,
}

/// What one Attempt ended as, and what it was charged.
type Outcome = (AttemptState, u128);

/// Play a sequence and return how each Attempt ended, plus what the run spent.
fn play(steps: &[Step]) -> (Outcome, Outcome, u128) {
    let mut attempts = ledger();
    let mut budget = BudgetLedger::default().with_limit(BudgetScope::Run, Budget::of(1000));
    let scopes = [BudgetScope::FanOut("deep".into()), BudgetScope::Run];
    let mut a1 = None;
    let mut a2 = None;

    for step in steps {
        match step {
            Step::DispatchA1 => {
                let reservation = budget.reserve(&scopes, 100).expect("first dispatch fits");
                a1 = Some((attempts.dispatch("deep"), reservation));
            }
            Step::FenceA1 => attempts.fence("deep"),
            Step::DispatchA2 => {
                let reservation = budget.reserve(&scopes, 100).expect("the retry fits");
                a2 = Some((attempts.dispatch("deep"), reservation));
            }
            Step::DeliverA2 => {
                let (id, reservation) = a2.as_ref().expect("A2 was dispatched");
                deliver(&mut attempts, id, 100);
                budget.charge_exact(reservation, 100).unwrap();
            }
            Step::DeliverA1 => {
                let (id, reservation) = a1.as_ref().expect("A1 was dispatched");
                deliver(&mut attempts, id, 100);
                // Charged whether or not anyone reads it: the tokens were spent.
                budget.charge_exact(reservation, 100).unwrap();
            }
        }
    }

    let outcome = |slot: &Option<(AttemptId, _)>| {
        let (id, _) = slot.as_ref().expect("dispatched");
        let attempt = attempts.attempt(id).expect("recorded");
        (attempt.state, attempt.charged)
    };
    (
        outcome(&a1),
        outcome(&a2),
        budget.committed(&BudgetScope::Run),
    )
}

#[test]
fn a_late_result_is_quarantined_charged_and_never_selected() {
    let mut attempts = ledger();
    let mut budget = BudgetLedger::default().with_limit(BudgetScope::Run, Budget::of(1000));

    // A1 dispatched and reserved.
    let reservation_a1 = budget.reserve(&[BudgetScope::Run], 100).unwrap();
    let a1 = attempts.dispatch("deep");

    // It times out. The process behind it is still alive; the kernel stops waiting.
    attempts.fence("deep");
    assert_eq!(attempts.attempt(&a1).unwrap().state, AttemptState::Fenced);

    // A2 runs and answers.
    let reservation_a2 = budget.reserve(&[BudgetScope::Run], 100).unwrap();
    let a2 = attempts.dispatch("deep");
    assert_eq!(deliver(&mut attempts, &a2, 90), Selection::Selected);
    budget.charge_exact(&reservation_a2, 90).unwrap();

    // ...and then A1 delivers, with a finding that looks entirely reasonable.
    let selection = deliver(&mut attempts, &a1, 100);
    budget.charge_exact(&reservation_a1, 100).unwrap();

    // Never selected: only A2's delivery may feed a consumer.
    assert_eq!(selection, Selection::Quarantined);
    assert_eq!(attempts.attempt(&a2).unwrap().state, AttemptState::Selected);

    // Charged anyway. A fenced attempt is not a free retry.
    assert_eq!(budget.committed(&BudgetScope::Run), 190);
    assert_eq!(attempts.attempt(&a1).unwrap().charged, 100);
    assert_eq!(attempts.attempt(&a2).unwrap().charged, 90);

    // And it is *recorded*, not discarded — an operator can see that a fenced attempt delivered.
    assert_eq!(
        attempts.attempt(&a1).unwrap().state,
        AttemptState::Quarantined
    );
}

/// The replay property from the case: A1's delivery may land anywhere, and the run is the same.
#[test]
fn the_late_delivery_may_arrive_at_any_point_without_changing_the_run() {
    use Step::*;

    let orderings = [
        // Immediately after being fenced, before the retry is even dispatched.
        vec![DispatchA1, FenceA1, DeliverA1, DispatchA2, DeliverA2],
        // While the retry is running.
        vec![DispatchA1, FenceA1, DispatchA2, DeliverA1, DeliverA2],
        // After the retry answered — the case's own ordering.
        vec![DispatchA1, FenceA1, DispatchA2, DeliverA2, DeliverA1],
    ];

    let outcomes: Vec<_> = orderings.iter().map(|steps| play(steps)).collect();
    for (index, outcome) in outcomes.iter().enumerate().skip(1) {
        assert_eq!(
            outcome, &outcomes[0],
            "ordering {index} produced a different run"
        );
    }

    let (a1, a2, spent) = outcomes[0];
    assert_eq!(a1, (AttemptState::Quarantined, 100));
    assert_eq!(a2, (AttemptState::Selected, 100));
    assert_eq!(spent, 200, "both attempts charged");
}

/// The reason a retry is dispatched at all is that the first one is no longer wanted — so
/// dispatching one fences the other, without the caller having to remember.
#[test]
fn a_retry_fences_its_predecessor_even_without_an_explicit_timeout() {
    let mut attempts = ledger();
    let a1 = attempts.dispatch("deep");
    let a2 = attempts.dispatch("deep");

    // A1 delivers first, having never been explicitly fenced.
    assert_eq!(
        deliver(&mut attempts, &a1, 10),
        Selection::Quarantined,
        "a superseded attempt cannot win by finishing first"
    );
    assert_eq!(deliver(&mut attempts, &a2, 10), Selection::Selected);
    assert_eq!(
        attempts.attempt(&a1).unwrap().state,
        AttemptState::Quarantined
    );
}

/// Budget exhaustion must stop the next dispatch, and a fenced attempt's charge is what makes
/// the cap bite. Otherwise a node could retry forever at no recorded cost.
#[test]
fn fenced_attempts_consume_the_cap_that_bounds_retries() {
    let mut attempts = ledger();
    let deep = BudgetScope::FanOut("deep".into());
    let mut budget = BudgetLedger::default().with_limit(deep.clone(), Budget::of(250));

    let mut dispatched = Vec::new();
    while let Ok(reservation) = budget.reserve(std::slice::from_ref(&deep), 100) {
        let id = attempts.dispatch("deep");
        // Every attempt times out and is charged in full.
        attempts.fence("deep");
        deliver(&mut attempts, &id, 100);
        budget.charge_exact(&reservation, 100).unwrap();
        dispatched.push(id);
    }

    assert_eq!(
        dispatched.len(),
        2,
        "a cap of 250 admits two 100-unit attempts"
    );
    assert!(
        dispatched
            .iter()
            .all(|id| attempts.attempt(id).unwrap().state == AttemptState::Quarantined),
        "none of them landed"
    );
    assert_eq!(budget.committed(&deep), 200);
}

#[test]
fn late_native_usage_does_not_overflow_the_total_of_distinct_attempts() {
    let mut attempts = ledger();
    let first = attempts.dispatch("review");
    attempts.charge_exact(&first, 7).unwrap();
    attempts.fence("review");
    let second = attempts.dispatch("review");
    attempts
        .charge_exact(&second, u128::from(u64::MAX))
        .unwrap();
    attempts
        .charge_exact(&second, u128::from(u64::MAX))
        .unwrap();
    assert_eq!(attempts.attempt(&first).unwrap().charged, 7);
    assert_eq!(
        attempts.attempt(&second).unwrap().charged,
        u128::from(u64::MAX)
    );
}
