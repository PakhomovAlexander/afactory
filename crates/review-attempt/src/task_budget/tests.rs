//! The named refusals of dispatch and the recovery of abandoned Attempts.
use super::*;

fn budget(attempts: u32, tokens: u64) -> TaskBudget {
    TaskBudget::new(
        TaskLimitsV1 {
            tokens,
            max_attempts: attempts,
            deadline_unix_ms: 10_000,
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
                    max_attempts: 1,
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

/// Start `node` at `now` and settle it at `charge`, as an Attempt that ran.
fn spend(budget: &mut TaskBudget, node: &str, now: u64, charge: u128) -> String {
    let reservation = budget.prepare(node, now).unwrap();
    budget.begin(&reservation.id, now).unwrap();
    budget.settle_exact(&reservation.id, charge).unwrap();
    reservation.id
}

#[test]
fn every_dispatch_guard_names_itself_and_its_numbers() {
    let refused = |budget: &mut TaskBudget, id: &str, now: u64| budget.begin(id, now).unwrap_err();
    let prefix = "Task reservation for node implement is not dispatchable: ";

    let mut released = budget(3, 200);
    let reservation = released.prepare("implement", 1).unwrap();
    released.release(&reservation.id).unwrap();
    assert_eq!(
        refused(&mut released, &reservation.id, 2),
        format!("{prefix}it was released before it started")
    );

    let mut settled = budget(3, 200);
    let id = spend(&mut settled, "implement", 1, 1_500);
    assert_eq!(
        refused(&mut settled, &id, 2),
        format!("{prefix}it is already settled, charged 1,500 tokens")
    );

    let mut clock = budget(3, 200);
    let reservation = clock.prepare("implement", 5).unwrap();
    assert_eq!(
        refused(&mut clock, &reservation.id, 4),
        format!("{prefix}the policy clock moved backwards: now 4 ms < last recorded 5 ms")
    );

    let mut deadline = budget(3, 200);
    let reservation = deadline.prepare("implement", 1).unwrap();
    assert_eq!(
        refused(&mut deadline, &reservation.id, 101),
        format!("{prefix}its Attempt deadline passed: now 101 ms >= deadline 101 ms")
    );

    // A reported overrun of another Attempt breaches the Task budget for every later start.
    let mut breached = budget(3, 200);
    let waiting = breached.prepare("implement", 1).unwrap();
    let overrun = breached.prepare("review.verify", 1).unwrap();
    breached.begin(&overrun.id, 1).unwrap();
    assert!(breached.settle_exact(&overrun.id, 31).is_ok());
    assert!(breached.breached());
    assert_eq!(
        refused(&mut breached, &waiting.id, 2),
        format!(
            "{prefix}the Task token budget is breached: charged 31 of a 200 token limit, \
             reservation 40 > remaining 129"
        )
    );
}

#[test]
fn the_token_guard_states_the_reservation_against_what_remains() {
    let mut tight = budget(3, 60);
    assert_eq!(
        tight.prepare("implement", 1).unwrap_err(),
        "Task token limit protects still-required verification: reservation 40 + protected \
         verification 30 > remaining 60"
    );
}

#[test]
fn an_abandoned_attempt_is_recovered_within_its_allowance_and_not_beyond() {
    // Two Task Attempts: the implementer's one and the verifier's protected one. An abandoned
    // implementer Attempt uses neither.
    let mut task = budget(2, 200);
    let lost = spend(&mut task, "implement", 1, 7);
    task.recover(&lost).unwrap();
    task.recover(&lost).unwrap();
    assert_eq!(
        task.recovery("implement"),
        Some(NodeRecovery {
            chargeable: 0,
            abandoned: 1,
            allowance: DEFAULT_RECOVERY_ATTEMPTS,
        })
    );
    assert!(!task.recovery("implement").unwrap().exhausted());
    assert_eq!((task.begun_attempts(), task.chargeable_attempts()), (1, 0));
    // The observed 7 tokens stay charged; the rest of the 40-token reservation is back.
    assert_eq!(task.committed_tokens(), 7);
    assert_eq!(task.reserved_tokens(), 0);
    assert_eq!(task.remaining_limits().tokens, 193);

    // The re-dispatched Attempt is lost too: the allowance of one recovery is spent.
    let again = spend(&mut task, "implement", 2, 0);
    task.recover(&again).unwrap();
    assert!(task.recovery("implement").unwrap().exhausted());
    assert_eq!(
        task.prepare("implement", 3).unwrap_err(),
        "Task node implement exhausted its recovery allowance: 2 abandoned Attempts, recovery \
         allowance 1"
    );
    // The verifier still has its own Attempt, untouched by the implementer's losses.
    spend(&mut task, "review.verify", 3, 5);
    assert_eq!((task.begun_attempts(), task.chargeable_attempts()), (3, 1));
}

#[test]
fn a_declared_recovery_allowance_bounds_the_node_and_only_a_settled_attempt_recovers() {
    let mut task = budget(3, 400)
        .with_recovery_allowances(&BTreeMap::from([("implement".into(), 2)]))
        .unwrap();
    for now in 1..=2 {
        let lost = spend(&mut task, "implement", now, 0);
        task.recover(&lost).unwrap();
    }
    assert!(
        task.prepare("implement", 3).is_ok(),
        "two of two recoveries"
    );
    let pending = task.prepare("review.verify", 3).unwrap();
    assert_eq!(
        task.recover(&pending.id).unwrap_err(),
        "Only a settled started Task Attempt can be recovered"
    );
    assert!(
        budget(3, 400)
            .with_recovery_allowances(&BTreeMap::from([("absent".into(), 1)]))
            .unwrap_err()
            .contains("absent")
    );
    assert!(
        budget(3, 400)
            .with_recovery_allowances(&BTreeMap::from([(
                "implement".into(),
                MAX_RECOVERY_ATTEMPTS + 1
            )]))
            .is_err()
    );
}

#[test]
fn counts_are_grouped_by_thousands() {
    assert_eq!(group(0), "0");
    assert_eq!(group(999), "999");
    assert_eq!(group(1_500_000), "1,500,000");
    assert_eq!(group(900_000), "900,000");
}
