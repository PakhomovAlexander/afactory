//! Budgets: reserve before spending, and charge what was spent even when it was wasted.
//!
//! Two rules, and the second is the one that gets skipped in a hurry:
//!
//! 1. **Reserve before dispatch.** A dispatch that cannot reserve does not happen. Accounting
//!    after the fact would let a retry storm or a wide scatter overrun a cap by the width of one
//!    attempt — and "one attempt" on a frontier model at maximum reasoning is not a rounding
//!    error.
//! 2. **A fenced attempt still charges.** It consumed the tokens whether or not anyone read its
//!    answer. Forgiving it would make retries free, which is precisely the behaviour a cap
//!    exists to bound.
//!
//! Scopes nest: an attempt's spend counts against its node, its binding's fan-out, and the run.
//! The tightest binding limit is the one that refuses, and the error says which — a cap that
//! refuses without naming itself is one nobody can raise correctly.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Where a limit applies. Ordered from tightest to widest for error reporting.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scope {
    Attempt(String),
    Node(String),
    /// A group of nodes sharing one limit — a scatter's shards, or every binding of one reviewer.
    FanOut(String),
    Run,
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scope::Attempt(id) => write!(f, "attempt {id}"),
            Scope::Node(id) => write!(f, "node {id}"),
            Scope::FanOut(id) => write!(f, "fan-out {id}"),
            Scope::Run => write!(f, "run"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub limit: u64,
}

impl Budget {
    pub fn of(limit: u64) -> Budget {
        Budget { limit }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetError {
    /// The scope whose limit refused. Named so an operator knows which number to change.
    pub scope: Scope,
    pub limit: u64,
    pub committed: u64,
    pub reserved: u64,
    pub requested: u64,
}

impl std::fmt::Display for BudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} budget exhausted: limit {}, already committed {}, reserved {}, requested {}",
            self.scope, self.limit, self.committed, self.reserved, self.requested
        )
    }
}

impl std::error::Error for BudgetError {}

/// A granted reservation. Holding one is what entitles a dispatch to happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub id: String,
    pub amount: u64,
    scopes: Vec<Scope>,
}

#[derive(Debug, Clone, Default)]
struct Account {
    limit: Option<u64>,
    committed: u64,
    reserved: u64,
}

#[derive(Debug, Clone, Default)]
pub struct BudgetLedger {
    accounts: BTreeMap<Scope, Account>,
    outstanding: BTreeMap<String, Reservation>,
    next: u64,
}

impl BudgetLedger {
    pub fn with_limit(mut self, scope: Scope, budget: Budget) -> Self {
        self.accounts.entry(scope).or_default().limit = Some(budget.limit);
        self
    }

    /// Reserve against every scope an attempt belongs to.
    ///
    /// All or nothing: if any scope refuses, nothing is reserved anywhere. A partial reservation
    /// would leave a scope holding capacity for a dispatch that never happened, and the next
    /// dispatch would be refused for a spend nobody made.
    pub fn reserve(&mut self, scopes: &[Scope], amount: u64) -> Result<Reservation, BudgetError> {
        for scope in scopes {
            let account = self.accounts.entry(scope.clone()).or_default();
            if let Some(limit) = account.limit
                && account.committed + account.reserved + amount > limit
            {
                return Err(BudgetError {
                    scope: scope.clone(),
                    limit,
                    committed: account.committed,
                    reserved: account.reserved,
                    requested: amount,
                });
            }
        }
        for scope in scopes {
            self.accounts.entry(scope.clone()).or_default().reserved += amount;
        }

        self.next += 1;
        let reservation = Reservation {
            id: format!("reservation:{}", self.next),
            amount,
            scopes: scopes.to_vec(),
        };
        self.outstanding
            .insert(reservation.id.clone(), reservation.clone());
        Ok(reservation)
    }

    /// Settle a reservation with what was actually spent.
    ///
    /// `actual` may exceed the reservation — a model does not stop at an estimate — and the
    /// overrun is committed rather than refused. Refusing here would mean discarding work
    /// already paid for; the cap's job is to stop the *next* dispatch, which it now will.
    pub fn charge(&mut self, reservation: &Reservation, actual: u64) {
        let Some(held) = self.outstanding.remove(&reservation.id) else {
            return;
        };
        for scope in &held.scopes {
            let account = self.accounts.entry(scope.clone()).or_default();
            account.reserved = account.reserved.saturating_sub(held.amount);
            account.committed += actual;
        }
    }

    /// Release a reservation that was never spent — a dispatch refused before it started.
    pub fn release(&mut self, reservation: &Reservation) {
        let Some(held) = self.outstanding.remove(&reservation.id) else {
            return;
        };
        for scope in &held.scopes {
            let account = self.accounts.entry(scope.clone()).or_default();
            account.reserved = account.reserved.saturating_sub(held.amount);
        }
    }

    pub fn committed(&self, scope: &Scope) -> u64 {
        self.accounts.get(scope).map(|a| a.committed).unwrap_or(0)
    }

    pub fn reserved(&self, scope: &Scope) -> u64 {
        self.accounts.get(scope).map(|a| a.reserved).unwrap_or(0)
    }

    pub fn remaining(&self, scope: &Scope) -> Option<u64> {
        let account = self.accounts.get(scope)?;
        let limit = account.limit?;
        Some(limit.saturating_sub(account.committed + account.reserved))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_ledger(limit: u64) -> BudgetLedger {
        BudgetLedger::default().with_limit(Scope::Run, Budget::of(limit))
    }

    #[test]
    fn a_reservation_that_would_exceed_the_cap_is_refused_before_it_spends() {
        let mut ledger = run_ledger(100);
        let first = ledger.reserve(&[Scope::Run], 60).unwrap();
        let error = ledger.reserve(&[Scope::Run], 60).unwrap_err();

        assert_eq!(error.scope, Scope::Run);
        assert_eq!(error.reserved, 60);
        assert_eq!(ledger.remaining(&Scope::Run), Some(40));

        ledger.charge(&first, 60);
        assert_eq!(ledger.committed(&Scope::Run), 60);
        assert_eq!(ledger.remaining(&Scope::Run), Some(40));
    }

    /// The rule that makes a cap mean something under retry: every attempt charges, including
    /// the ones whose answers were thrown away.
    #[test]
    fn retries_cannot_overrun_the_cap() {
        let mut ledger = run_ledger(100);
        let mut spent = 0;
        let mut attempts = 0;
        while let Ok(reservation) = ledger.reserve(&[Scope::Run], 30) {
            ledger.charge(&reservation, 30);
            spent += 30;
            attempts += 1;
        }
        assert_eq!(attempts, 3, "three attempts fit in a cap of 100");
        assert_eq!(spent, 90);
        assert!(ledger.remaining(&Scope::Run).unwrap() < 30);
    }

    /// A wide scatter is bounded by its fan-out scope, not only by the run.
    #[test]
    fn a_fan_out_limit_bounds_a_scatter() {
        let mut ledger = BudgetLedger::default()
            .with_limit(Scope::Run, Budget::of(1000))
            .with_limit(Scope::FanOut("architecture".into()), Budget::of(50));

        let scopes = [Scope::FanOut("architecture".into()), Scope::Run];
        let first = ledger.reserve(&scopes, 25).unwrap();
        let second = ledger.reserve(&scopes, 25).unwrap();
        let refused = ledger.reserve(&scopes, 25).unwrap_err();

        assert_eq!(refused.scope, Scope::FanOut("architecture".into()));
        assert_eq!(
            ledger.remaining(&Scope::Run),
            Some(950),
            "the run still has room; the fan-out is what refused"
        );
        ledger.charge(&first, 25);
        ledger.charge(&second, 25);
    }

    /// A refused reservation must leave no trace in any scope, or the next attempt is refused
    /// for a spend nobody made.
    #[test]
    fn a_refused_reservation_is_all_or_nothing() {
        let mut ledger = BudgetLedger::default()
            .with_limit(Scope::Run, Budget::of(1000))
            .with_limit(Scope::Node("deep".into()), Budget::of(10));

        let scopes = [Scope::Node("deep".into()), Scope::Run];
        assert!(ledger.reserve(&scopes, 50).is_err());
        assert_eq!(
            ledger.reserved(&Scope::Run),
            0,
            "the run must not be holding a reservation the node refused"
        );
        assert!(ledger.reserve(&[Scope::Run], 50).is_ok());
    }

    /// An overrun commits rather than being refused: the work is already paid for. What it must
    /// do is stop the *next* dispatch.
    #[test]
    fn an_overrun_commits_and_then_closes_the_gate() {
        let mut ledger = run_ledger(100);
        let reservation = ledger.reserve(&[Scope::Run], 40).unwrap();
        ledger.charge(&reservation, 120);

        assert_eq!(ledger.committed(&Scope::Run), 120);
        assert_eq!(ledger.remaining(&Scope::Run), Some(0));
        assert!(ledger.reserve(&[Scope::Run], 1).is_err());
    }

    #[test]
    fn releasing_an_unspent_reservation_returns_the_capacity() {
        let mut ledger = run_ledger(100);
        let reservation = ledger.reserve(&[Scope::Run], 80).unwrap();
        assert!(ledger.reserve(&[Scope::Run], 80).is_err());
        ledger.release(&reservation);
        assert!(
            ledger.reserve(&[Scope::Run], 80).is_ok(),
            "a dispatch that never happened must not hold capacity"
        );
    }

    #[test]
    fn an_unlimited_scope_never_refuses() {
        let mut ledger = BudgetLedger::default();
        for _ in 0..100 {
            let reservation = ledger.reserve(&[Scope::Run], u64::MAX / 200).unwrap();
            ledger.charge(&reservation, u64::MAX / 200);
        }
        assert_eq!(ledger.remaining(&Scope::Run), None);
    }
}
