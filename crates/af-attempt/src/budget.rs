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
//! Scopes nest: an attempt's spend counts against every fan-out it belongs to and the run. The
//! tightest binding limit is the one that refuses, and the error says which — a cap that
//! refuses without naming itself is one nobody can raise correctly.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Where a limit applies. Ordered from tightest to widest for error reporting.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BudgetScope {
    /// A group of nodes sharing one limit, such as a Task token scope over a Round's reviewers.
    FanOut(String),
    Run,
}

impl std::fmt::Display for BudgetScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FanOut(id) => write!(f, "fan-out {id}"),
            Self::Run => write!(f, "run"),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetRefusal {
    Tokens,
    ReservationIdentities,
    AccountingOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetError {
    pub reason: BudgetRefusal,
    /// The scope whose limit refused. Named so an operator knows which number to change.
    pub scope: BudgetScope,
    pub limit: u64,
    pub committed: u128,
    pub reserved: u128,
    pub requested: u64,
}

impl std::fmt::Display for BudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.reason == BudgetRefusal::ReservationIdentities {
            return write!(f, "budget reservation identity space exhausted");
        }
        if self.reason == BudgetRefusal::AccountingOverflow {
            return write!(f, "budget accounting overflow; admission is closed");
        }
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
    scopes: Vec<BudgetScope>,
}

impl Reservation {
    pub(crate) fn scopes(&self) -> &[BudgetScope] {
        &self.scopes
    }
}

#[derive(Debug, Clone, Default)]
struct Account {
    limit: Option<u64>,
    committed: u128,
    reserved: u128,
}

#[derive(Debug, Clone)]
struct Outstanding {
    reservation: Reservation,
    observed: u128,
}

#[derive(Debug, Clone, Default)]
pub struct BudgetLedger {
    accounts: BTreeMap<BudgetScope, Account>,
    outstanding: BTreeMap<String, Outstanding>,
    next: u64,
    overflowed: bool,
}

impl BudgetLedger {
    pub fn with_limit(mut self, scope: BudgetScope, budget: Budget) -> Self {
        self.accounts.entry(scope).or_default().limit = Some(budget.limit);
        self
    }

    pub(crate) fn with_exact_committed(mut self, scope: BudgetScope, committed: u128) -> Self {
        self.accounts.entry(scope).or_default().committed = committed;
        self
    }

    /// Reserve against every scope an attempt belongs to.
    ///
    /// All or nothing: if any scope refuses, nothing is reserved anywhere. A partial reservation
    /// would leave a scope holding capacity for a dispatch that never happened, and the next
    /// dispatch would be refused for a spend nobody made.
    pub fn reserve(
        &mut self,
        scopes: &[BudgetScope],
        amount: u64,
    ) -> Result<Reservation, BudgetError> {
        if self.overflowed {
            return Err(BudgetError {
                reason: BudgetRefusal::AccountingOverflow,
                scope: BudgetScope::Run,
                limit: u64::MAX,
                committed: self.committed(&BudgetScope::Run),
                reserved: self.reserved(&BudgetScope::Run),
                requested: amount,
            });
        }
        let next = self.next.checked_add(1).ok_or_else(|| BudgetError {
            reason: BudgetRefusal::ReservationIdentities,
            scope: BudgetScope::Run,
            limit: u64::MAX,
            committed: u128::from(self.next),
            reserved: 0,
            requested: 1,
        })?;
        let scopes: Vec<_> = scopes
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        for scope in &scopes {
            let account = self.accounts.get(scope).cloned().unwrap_or_default();
            let total = account
                .committed
                .checked_add(account.reserved)
                .and_then(|used| used.checked_add(u128::from(amount)))
                .ok_or_else(|| BudgetError {
                    reason: BudgetRefusal::AccountingOverflow,
                    scope: scope.clone(),
                    limit: account.limit.unwrap_or(u64::MAX),
                    committed: account.committed,
                    reserved: account.reserved,
                    requested: amount,
                })?;
            if let Some(limit) = account.limit
                && total > u128::from(limit)
            {
                return Err(BudgetError {
                    reason: BudgetRefusal::Tokens,
                    scope: scope.clone(),
                    limit,
                    committed: account.committed,
                    reserved: account.reserved,
                    requested: amount,
                });
            }
        }
        for scope in &scopes {
            self.accounts.entry(scope.clone()).or_default().reserved += u128::from(amount);
        }

        self.next = next;
        let reservation = Reservation {
            id: format!("reservation:{}", self.next),
            amount,
            scopes,
        };
        self.outstanding.insert(
            reservation.id.clone(),
            Outstanding {
                reservation: reservation.clone(),
                observed: 0,
            },
        );
        Ok(reservation)
    }

    /// Move known spend out of an outstanding reservation without ending its Attempt.
    /// Observations are cumulative floors, so duplicated or older receipts cannot charge twice.
    /// An unrepresentable aggregate leaves every scope unchanged and closes admission; the
    /// caller must retain the rejected paid receipt as evidence.
    pub fn observe_charge_exact(
        &mut self,
        reservation: &Reservation,
        actual: u128,
    ) -> Result<(), String> {
        let held = self
            .outstanding
            .get_mut(&reservation.id)
            .ok_or("Usage has no outstanding reservation")?;
        if &held.reservation != reservation {
            return Err("Usage differs from its reserved authority".into());
        }
        let delta = actual.saturating_sub(held.observed);
        let consumed = delta.min(u128::from(held.reservation.amount).saturating_sub(held.observed));
        // Check every scope before changing any of them.
        for scope in &held.reservation.scopes {
            let account = &self.accounts[scope];
            if account.committed.checked_add(delta).is_none() {
                self.overflowed = true;
                return Err("Observed budget total overflow".into());
            }
        }
        for scope in &held.reservation.scopes {
            let account = self.accounts.get_mut(scope).expect("reserved scope");
            account.committed += delta;
            account.reserved -= consumed;
        }
        held.observed = held.observed.max(actual);
        Ok(())
    }

    pub fn observed_charge_exact(&self, reservation: &Reservation) -> u128 {
        self.outstanding
            .get(&reservation.id)
            .map_or(0, |held| held.observed)
    }

    /// Settle a reservation with what was actually spent.
    ///
    /// `actual` may exceed the reservation — a model does not stop at an estimate — and the
    /// overrun is committed rather than refused. Refusing here would mean discarding work
    /// already paid for; the cap's job is to stop the *next* dispatch, which it now will.
    pub fn charge_exact(&mut self, reservation: &Reservation, actual: u128) -> Result<(), String> {
        if !self.outstanding.contains_key(&reservation.id) {
            return Ok(());
        }
        self.observe_charge_exact(reservation, actual)?;
        let Some(held) = self.outstanding.remove(&reservation.id) else {
            return Ok(());
        };
        for scope in &held.reservation.scopes {
            let account = self.accounts.entry(scope.clone()).or_default();
            account.reserved -= u128::from(held.reservation.amount).saturating_sub(held.observed);
        }
        Ok(())
    }

    /// Release a reservation that was never spent — a dispatch refused before it started.
    pub fn release(&mut self, reservation: &Reservation) {
        let Some(held) = self.outstanding.remove(&reservation.id) else {
            return;
        };
        for scope in &held.reservation.scopes {
            let account = self.accounts.entry(scope.clone()).or_default();
            account.reserved -= u128::from(held.reservation.amount).saturating_sub(held.observed);
        }
    }

    pub fn committed(&self, scope: &BudgetScope) -> u128 {
        self.accounts.get(scope).map(|a| a.committed).unwrap_or(0)
    }

    pub fn reserved(&self, scope: &BudgetScope) -> u128 {
        self.accounts.get(scope).map(|a| a.reserved).unwrap_or(0)
    }

    pub fn remaining(&self, scope: &BudgetScope) -> Option<u64> {
        let account = self.accounts.get(scope)?;
        let limit = account.limit?;
        if self.overflowed {
            return Some(0);
        }
        Some(
            account
                .committed
                .checked_add(account.reserved)
                .map_or(0, |used| u128::from(limit).saturating_sub(used) as u64),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_attempt_usage_retains_prior_spend() {
        let node = BudgetScope::FanOut("reviewer".into());
        let mut ledger = run_ledger(100).with_exact_committed(BudgetScope::Run, 7);
        let reservation = ledger
            .reserve(&[node.clone(), BudgetScope::Run], 40)
            .unwrap();
        let sibling = ledger.reserve(&[BudgetScope::Run], 30).unwrap();
        let actual = u128::from(u64::MAX) + 7;
        for observed in [actual, actual, 1] {
            ledger.observe_charge_exact(&reservation, observed).unwrap();
            assert_eq!(ledger.observed_charge_exact(&reservation), actual);
            assert_eq!(ledger.committed(&node), actual);
            assert_eq!(ledger.committed(&BudgetScope::Run), actual + 7);
            assert_eq!(ledger.reserved(&BudgetScope::Run), 30);
            assert!(ledger.reserve(&[BudgetScope::Run], 1).is_err());
        }
        ledger.charge_exact(&reservation, actual).unwrap();
        ledger.charge_exact(&reservation, actual).unwrap();
        ledger.release(&sibling);
        assert_eq!(ledger.committed(&BudgetScope::Run), actual + 7);
        assert_eq!(ledger.reserved(&BudgetScope::Run), 0);
    }

    #[test]
    fn an_unrepresentable_charge_is_atomic_and_closes_admission() {
        let node = BudgetScope::FanOut("reviewer".into());
        let mut ledger = BudgetLedger::default()
            .with_limit(node.clone(), Budget::of(100))
            .with_exact_committed(BudgetScope::Run, u128::MAX - 10);
        let reservation = ledger
            .reserve(&[node.clone(), BudgetScope::Run], 7)
            .unwrap();
        assert!(ledger.charge_exact(&reservation, 11).is_err());
        assert_eq!(ledger.committed(&node), 0);
        assert_eq!(ledger.committed(&BudgetScope::Run), u128::MAX - 10);
        assert_eq!(ledger.reserved(&node), 7);
        assert_eq!(ledger.reserved(&BudgetScope::Run), 7);
        assert_eq!(ledger.observed_charge_exact(&reservation), 0);
        assert_eq!(ledger.remaining(&node), Some(0));
        assert_eq!(
            ledger.reserve(&[node], 1).unwrap_err().reason,
            BudgetRefusal::AccountingOverflow
        );
    }

    fn run_ledger(limit: u64) -> BudgetLedger {
        BudgetLedger::default().with_limit(BudgetScope::Run, Budget::of(limit))
    }

    #[test]
    fn partial_usage_checks_every_scope_before_mutating_and_never_refunds_spend() {
        let node = BudgetScope::FanOut("reviewer".into());
        let mut ledger = run_ledger(100).with_limit(node.clone(), Budget::of(60));
        let first = ledger
            .reserve(&[node.clone(), BudgetScope::Run], 40)
            .unwrap();
        let other = ledger.reserve(&[BudgetScope::Run], 30).unwrap();
        // A forged reservation must not partially charge the narrower scope.
        let mut forged = first.clone();
        forged.amount += 1;
        assert!(
            ledger
                .observe_charge_exact(&forged, u128::from(u64::MAX))
                .is_err()
        );
        assert_eq!(ledger.committed(&node), 0);
        assert_eq!(ledger.reserved(&BudgetScope::Run), 70);
        ledger.observe_charge_exact(&first, 20).unwrap();
        ledger.observe_charge_exact(&first, 20).unwrap();
        ledger.observe_charge_exact(&first, 4).unwrap();
        assert_eq!(ledger.committed(&node), 20);
        assert_eq!(ledger.reserved(&node), 20);
        assert_eq!(ledger.committed(&BudgetScope::Run), 20);
        assert_eq!(ledger.reserved(&BudgetScope::Run), 50);
        ledger.charge_exact(&first, 28).unwrap();
        ledger.charge_exact(&first, 28).unwrap();
        assert_eq!(ledger.committed(&BudgetScope::Run), 28);
        assert_eq!(ledger.reserved(&BudgetScope::Run), 30);
        ledger.observe_charge_exact(&other, 3).unwrap();
        ledger.release(&other);
        assert_eq!(ledger.committed(&BudgetScope::Run), 31);
        assert_eq!(ledger.reserved(&BudgetScope::Run), 0);
    }

    #[test]
    fn a_reservation_that_would_exceed_the_cap_is_refused_before_it_spends() {
        let mut ledger = run_ledger(100);
        let first = ledger.reserve(&[BudgetScope::Run], 60).unwrap();
        let error = ledger.reserve(&[BudgetScope::Run], 60).unwrap_err();

        assert_eq!(error.scope, BudgetScope::Run);
        assert_eq!(error.reserved, 60);
        assert_eq!(ledger.remaining(&BudgetScope::Run), Some(40));

        ledger.charge_exact(&first, 60).unwrap();
        assert_eq!(ledger.committed(&BudgetScope::Run), 60);
        assert_eq!(ledger.remaining(&BudgetScope::Run), Some(40));
    }

    /// The rule that makes a cap mean something under retry: every attempt charges, including
    /// the ones whose answers were thrown away.
    #[test]
    fn retries_cannot_overrun_the_cap() {
        let mut ledger = run_ledger(100);
        let mut spent = 0;
        let mut attempts = 0;
        while let Ok(reservation) = ledger.reserve(&[BudgetScope::Run], 30) {
            ledger.charge_exact(&reservation, 30).unwrap();
            spent += 30;
            attempts += 1;
        }
        assert_eq!(attempts, 3, "three attempts fit in a cap of 100");
        assert_eq!(spent, 90);
        assert!(ledger.remaining(&BudgetScope::Run).unwrap() < 30);
    }

    /// A wide scatter is bounded by its fan-out scope, not only by the run.
    #[test]
    fn a_fan_out_limit_bounds_a_scatter() {
        let mut ledger = BudgetLedger::default()
            .with_limit(BudgetScope::Run, Budget::of(1000))
            .with_limit(BudgetScope::FanOut("architecture".into()), Budget::of(50));

        let scopes = [BudgetScope::FanOut("architecture".into()), BudgetScope::Run];
        let first = ledger.reserve(&scopes, 25).unwrap();
        let second = ledger.reserve(&scopes, 25).unwrap();
        let refused = ledger.reserve(&scopes, 25).unwrap_err();

        assert_eq!(refused.scope, BudgetScope::FanOut("architecture".into()));
        assert_eq!(
            ledger.remaining(&BudgetScope::Run),
            Some(950),
            "the run still has room; the fan-out is what refused"
        );
        ledger.charge_exact(&first, 25).unwrap();
        ledger.charge_exact(&second, 25).unwrap();
    }

    /// A refused reservation must leave no trace in any scope, or the next attempt is refused
    /// for a spend nobody made.
    #[test]
    fn a_refused_reservation_is_all_or_nothing() {
        let mut ledger = BudgetLedger::default()
            .with_limit(BudgetScope::Run, Budget::of(1000))
            .with_limit(BudgetScope::FanOut("deep".into()), Budget::of(10));

        let scopes = [BudgetScope::FanOut("deep".into()), BudgetScope::Run];
        assert!(ledger.reserve(&scopes, 50).is_err());
        assert_eq!(
            ledger.reserved(&BudgetScope::Run),
            0,
            "the run must not be holding a reservation the fan-out refused"
        );
        assert!(ledger.reserve(&[BudgetScope::Run], 50).is_ok());
    }

    /// An overrun commits rather than being refused: the work is already paid for. What it must
    /// do is stop the *next* dispatch.
    #[test]
    fn an_overrun_commits_and_then_closes_the_gate() {
        let mut ledger = run_ledger(100);
        let reservation = ledger.reserve(&[BudgetScope::Run], 40).unwrap();
        ledger.charge_exact(&reservation, 120).unwrap();

        assert_eq!(ledger.committed(&BudgetScope::Run), 120);
        assert_eq!(ledger.remaining(&BudgetScope::Run), Some(0));
        assert!(ledger.reserve(&[BudgetScope::Run], 1).is_err());
    }

    #[test]
    fn releasing_an_unspent_reservation_returns_the_capacity() {
        let mut ledger = run_ledger(100);
        let reservation = ledger.reserve(&[BudgetScope::Run], 80).unwrap();
        assert!(ledger.reserve(&[BudgetScope::Run], 80).is_err());
        ledger.release(&reservation);
        assert!(
            ledger.reserve(&[BudgetScope::Run], 80).is_ok(),
            "a dispatch that never happened must not hold capacity"
        );
    }

    #[test]
    fn an_unlimited_scope_never_refuses() {
        let mut ledger = BudgetLedger::default();
        for _ in 0..100 {
            let reservation = ledger.reserve(&[BudgetScope::Run], u64::MAX / 200).unwrap();
            ledger
                .charge_exact(&reservation, u128::from(u64::MAX / 200))
                .unwrap();
        }
        assert_eq!(ledger.remaining(&BudgetScope::Run), None);
    }
    #[test]
    fn wide_spend_preserves_each_scope_and_reservation_identity_exhaustion_is_atomic() {
        let node = BudgetScope::FanOut("reviewer".into());
        let mut ledger = run_ledger(100)
            .with_limit(node.clone(), Budget::of(60))
            .with_exact_committed(BudgetScope::Run, 7);
        let first = ledger
            .reserve(&[node.clone(), BudgetScope::Run, node.clone()], 40)
            .unwrap();
        let sibling = ledger.reserve(&[BudgetScope::Run], 30).unwrap();
        ledger
            .observe_charge_exact(&first, u128::from(u64::MAX))
            .unwrap();
        assert_eq!(ledger.committed(&node), u128::from(u64::MAX));
        assert_eq!(
            ledger.committed(&BudgetScope::Run),
            u128::from(u64::MAX) + 7
        );
        assert_eq!(ledger.reserved(&BudgetScope::Run), 30);
        assert_eq!(ledger.remaining(&BudgetScope::Run), Some(0));
        assert_eq!(
            ledger.reserve(&[BudgetScope::Run], 1).unwrap_err().reason,
            BudgetRefusal::Tokens
        );
        ledger.charge_exact(&first, u128::from(u64::MAX)).unwrap();
        ledger.release(&sibling);
        assert_eq!(
            ledger.committed(&BudgetScope::Run),
            u128::from(u64::MAX) + 7
        );
        assert_eq!(ledger.reserved(&BudgetScope::Run), 0);

        let mut exhausted = BudgetLedger {
            next: u64::MAX,
            ..Default::default()
        };
        let error = exhausted.reserve(&[BudgetScope::Run], 1).unwrap_err();
        assert_eq!(error.reason, BudgetRefusal::ReservationIdentities);
        assert_eq!(exhausted.next, u64::MAX);
        assert!(exhausted.accounts.is_empty());
        assert!(exhausted.outstanding.is_empty());
    }
}
