//! Attempts, fencing, and budgets.
//!
//! An attempt is one execution of one node. It can time out, be cancelled, be retried — and the
//! process behind it does not necessarily stop when the kernel stops waiting. That is the whole
//! problem: a reviewer whose attempt was abandoned five minutes ago can still deliver a
//! perfectly plausible finding, and nothing about that finding looks wrong.
//!
//! So an abandoned attempt is **fenced**: its epoch is revoked, and any result arriving under it
//! is *quarantined* — recorded as an immutable fact, charged to the budget it spent, and unable
//! to reach the FindingSet, convergence, or the report. Recorded rather than discarded, because
//! "a fenced attempt delivered late" is a thing an operator needs to be able to see; charged
//! rather than forgiven, because a fenced attempt is not a free retry.
//!
//! Budgets are reservations, not accounting after the fact. A dispatch that cannot reserve does
//! not happen, so a retry storm or a wide scatter cannot overrun a cap by the width of one
//! attempt — the check is before the spend, not after it.

pub mod budget;
pub mod fencing;

pub use budget::{Budget, BudgetError, BudgetLedger, Reservation, Scope};
pub use fencing::{Attempt, AttemptId, AttemptLedger, AttemptState, Epoch, Receipt, Selection};
