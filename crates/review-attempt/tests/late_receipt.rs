//! `fixtures/adversarial/late-receipt.md`, made executable.
//!
//! The scenario from the case: attempt A1 times out while its process is still alive, the kernel
//! fences it and starts A2, A2 completes normally — and *then* A1 delivers. Its finding is a
//! plausible one from a real reviewer, which is exactly why nothing about it looks wrong.
//!
//! Three things must hold, and the case names all three: A1's result is quarantined and cannot
//! reach anything downstream; its cost is still charged; and replay with A1's delivery moved to
//! any position produces the same outcome.

use review_attempt::{
    AttemptLedger, AttemptState, Budget, BudgetLedger, Receipt, Scope, Selection,
};

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

/// Play a sequence and return what a downstream node would see, plus what it all cost.
fn play(steps: &[Step]) -> (Vec<(String, String)>, u64, usize) {
    let mut attempts = AttemptLedger::default();
    let mut budget = BudgetLedger::default().with_limit(Scope::Run, Budget::of(1000));
    let mut a1 = None;
    let mut a2 = None;

    for step in steps {
        match step {
            Step::DispatchA1 => {
                let reservation = budget
                    .reserve(&[Scope::Node("deep".into()), Scope::Run], 100)
                    .expect("first dispatch fits");
                a1 = Some((attempts.dispatch("deep"), reservation));
            }
            Step::FenceA1 => attempts.fence("deep"),
            Step::DispatchA2 => {
                let reservation = budget
                    .reserve(&[Scope::Node("deep".into()), Scope::Run], 100)
                    .expect("the retry fits");
                a2 = Some((attempts.dispatch("deep"), reservation));
            }
            Step::DeliverA2 => {
                let (id, reservation) = a2.as_ref().expect("A2 was dispatched");
                attempts.admit(&Receipt {
                    attempt: id.clone(),
                    output: "artifact:A2".into(),
                    cost: 100,
                });
                budget.charge(reservation, 100);
            }
            Step::DeliverA1 => {
                let (id, reservation) = a1.as_ref().expect("A1 was dispatched");
                attempts.admit(&Receipt {
                    attempt: id.clone(),
                    output: "artifact:A1".into(),
                    cost: 100,
                });
                // Charged whether or not anyone reads it: the tokens were spent.
                budget.charge(reservation, 100);
            }
        }
    }

    (
        attempts.selected_outputs(),
        budget.committed(&Scope::Run),
        attempts.quarantined().len(),
    )
}

#[test]
fn a_late_result_is_quarantined_charged_and_invisible_downstream() {
    let mut attempts = AttemptLedger::default();
    let mut budget = BudgetLedger::default().with_limit(Scope::Run, Budget::of(1000));

    // A1 dispatched and reserved.
    let reservation_a1 = budget.reserve(&[Scope::Run], 100).unwrap();
    let a1 = attempts.dispatch("deep");

    // It times out. The process behind it is still alive; the kernel stops waiting.
    attempts.fence("deep");
    assert_eq!(attempts.attempt(&a1).unwrap().state, AttemptState::Fenced);

    // A2 runs and answers.
    let reservation_a2 = budget.reserve(&[Scope::Run], 100).unwrap();
    let a2 = attempts.dispatch("deep");
    assert_eq!(
        attempts.admit(&Receipt {
            attempt: a2.clone(),
            output: "artifact:A2".into(),
            cost: 90,
        }),
        Selection::Selected
    );
    budget.charge(&reservation_a2, 90);

    // ...and then A1 delivers, with a finding that looks entirely reasonable.
    let selection = attempts.admit(&Receipt {
        attempt: a1.clone(),
        output: "artifact:A1-plausible-but-fenced".into(),
        cost: 100,
    });
    budget.charge(&reservation_a1, 100);

    assert_eq!(selection, Selection::Quarantined);
    assert_eq!(
        attempts.attempt(&a1).unwrap().state,
        AttemptState::Quarantined
    );

    // Invisible downstream: a consumer sees A2's artifact and only A2's.
    assert_eq!(
        attempts.selected_outputs(),
        vec![("deep".to_string(), "artifact:A2".to_string())]
    );

    // Charged anyway. A fenced attempt is not a free retry.
    assert_eq!(budget.committed(&Scope::Run), 190);
    assert_eq!(attempts.total_charged(), 190);

    // And it is *recorded*, not discarded — an operator can see that a fenced attempt delivered.
    assert_eq!(attempts.quarantined().len(), 1);
    assert_eq!(attempts.quarantined()[0].id, a1);
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

    let (selected, spent, quarantined) = &outcomes[0];
    assert_eq!(
        selected,
        &vec![("deep".to_string(), "artifact:A2".to_string())]
    );
    assert_eq!(*spent, 200, "both attempts charged");
    assert_eq!(*quarantined, 1);
}

/// The reason a retry is dispatched at all is that the first one is no longer wanted — so
/// dispatching one fences the other, without the caller having to remember.
#[test]
fn a_retry_fences_its_predecessor_even_without_an_explicit_timeout() {
    let mut attempts = AttemptLedger::default();
    let a1 = attempts.dispatch("deep");
    let a2 = attempts.dispatch("deep");

    // A1 delivers first, having never been explicitly fenced.
    assert_eq!(
        attempts.admit(&Receipt {
            attempt: a1,
            output: "artifact:A1".into(),
            cost: 10,
        }),
        Selection::Quarantined,
        "a superseded attempt cannot win by finishing first"
    );
    assert_eq!(
        attempts.admit(&Receipt {
            attempt: a2,
            output: "artifact:A2".into(),
            cost: 10,
        }),
        Selection::Selected
    );
    assert_eq!(
        attempts.selected_outputs(),
        vec![("deep".to_string(), "artifact:A2".to_string())]
    );
}

/// Budget exhaustion must stop the next dispatch, and a fenced attempt's charge is what makes
/// the cap bite. Otherwise a node could retry forever at no recorded cost.
#[test]
fn fenced_attempts_consume_the_cap_that_bounds_retries() {
    let mut attempts = AttemptLedger::default();
    let mut budget =
        BudgetLedger::default().with_limit(Scope::Node("deep".into()), Budget::of(250));

    let mut dispatched = 0;
    loop {
        let Ok(reservation) = budget.reserve(&[Scope::Node("deep".into())], 100) else {
            break;
        };
        let id = attempts.dispatch("deep");
        dispatched += 1;
        // Every attempt times out and is charged in full.
        attempts.fence("deep");
        attempts.admit(&Receipt {
            attempt: id,
            output: "never selected".into(),
            cost: 100,
        });
        budget.charge(&reservation, 100);
    }

    assert_eq!(dispatched, 2, "a cap of 250 admits two 100-unit attempts");
    assert!(
        attempts.selected_outputs().is_empty(),
        "none of them landed"
    );
    assert_eq!(attempts.quarantined().len(), 2);
    assert_eq!(budget.committed(&Scope::Node("deep".into())), 200);
}
