//! One Task's admission accounting, including capacity still owed to its verifiers.
//!
//! This is a deterministic projection, not storage or an execution capability. The common
//! Store publishes the corresponding transitions under its writer lease before dispatch.
//! Replaying those transitions reconstructs this same ledger; children never construct one.

use std::collections::BTreeMap;

use review_core::task::{TaskLimitsV1, VerificationReserveV1};
use serde::{Deserialize, Serialize};

use crate::{Budget, BudgetLedger, Reservation, Scope};

mod scopes;
pub use scopes::TaskTokenScope;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeAllowance {
    pub tokens_per_attempt: u64,
    pub wall_ms_per_attempt: u64,
    pub max_attempts: u32,
    /// Attempts protected from other nodes. Only trusted verifier slots have this credit.
    pub verification_attempts: u32,
}

#[derive(Debug, Clone)]
struct NodeAccount {
    allowance: NodeAllowance,
    begun: u32,
    prepared: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReservation {
    pub id: String,
    pub node: String,
    pub tokens: u64,
    pub deadline_unix_ms: u64,
}

#[derive(Debug, Clone)]
struct Held {
    public: TaskReservation,
    tokens: Reservation,
    begun: bool,
    settled: Option<u128>,
    released: bool,
}

#[derive(Debug, Clone)]
pub struct TaskBudget {
    limits: TaskLimitsV1,
    tokens: BudgetLedger,
    nodes: BTreeMap<String, NodeAccount>,
    reservations: BTreeMap<String, Held>,
    last_time: u64,
    breached: bool,
    call_limits: BTreeMap<String, u32>,
    token_scopes: BTreeMap<String, TaskTokenScope>,
    captured_token_scopes: BTreeMap<String, TaskTokenScope>,
    retired_attempts: u64,
    deferred_verification: bool,
}

fn add(a: u64, b: u64) -> Result<u64, String> {
    a.checked_add(b)
        .ok_or_else(|| "Task resource total overflow".into())
}

impl TaskBudget {
    pub fn new(
        limits: TaskLimitsV1,
        allowances: BTreeMap<String, NodeAllowance>,
    ) -> Result<Self, String> {
        limits.validate()?;
        let mut minimum = VerificationReserveV1 {
            tokens: 0,
            attempts: 0,
            wall_ms: 0,
        };
        for (name, allowance) in &allowances {
            if name.is_empty()
                || !name.split('.').all(review_core::task::is_name)
                || allowance.max_attempts == 0
                || allowance.verification_attempts > allowance.max_attempts
                || allowance.wall_ms_per_attempt == 0
                || allowance.tokens_per_attempt > limits.tokens
            {
                return Err(format!("Invalid Task allowance for {name}"));
            }
            let count = u64::from(allowance.verification_attempts);
            minimum.tokens = add(
                minimum.tokens,
                allowance
                    .tokens_per_attempt
                    .checked_mul(count)
                    .ok_or("Task verifier token reservation overflow")?,
            )?;
            minimum.wall_ms = add(
                minimum.wall_ms,
                allowance
                    .wall_ms_per_attempt
                    .checked_mul(count)
                    .ok_or("Task verifier wall reservation overflow")?,
            )?;
            minimum.attempts = minimum
                .attempts
                .checked_add(allowance.verification_attempts)
                .ok_or("Task verifier Attempt reservation overflow")?;
        }
        if minimum.tokens > limits.verification.tokens
            || minimum.attempts > limits.verification.attempts
            || minimum.wall_ms > limits.verification.wall_ms
        {
            return Err("Task cannot protect the compiled verifier allocation".into());
        }
        Ok(Self {
            tokens: BudgetLedger::default().with_limit(Scope::Run, Budget::of(limits.tokens)),
            limits,
            nodes: allowances
                .into_iter()
                .map(|(name, allowance)| {
                    (
                        name,
                        NodeAccount {
                            allowance,
                            begun: 0,
                            prepared: 0,
                        },
                    )
                })
                .collect(),
            reservations: BTreeMap::new(),
            last_time: 0,
            breached: false,
            call_limits: BTreeMap::new(),
            token_scopes: BTreeMap::new(),
            captured_token_scopes: BTreeMap::new(),
            retired_attempts: 0,
            deferred_verification: false,
        })
    }

    /// A fixed planning bootstrap has not installed the business verifier nodes yet. Its
    /// absence never releases the verification allocation owed by the business Task.
    pub fn with_deferred_verification(mut self) -> Result<Self, String> {
        if !self.reservations.is_empty()
            || self.deferred_verification
            || self
                .nodes
                .values()
                .any(|node| node.allowance.verification_attempts != 0)
        {
            return Err("Deferred verification must be captured before planning dispatch".into());
        }
        self.deferred_verification = true;
        Ok(self)
    }

    /// Capacity available to a newly compiled graph, without creating an execution ledger.
    /// Earlier spend and the original absolute deadline remain authoritative.
    pub fn remaining_limits(&self) -> TaskLimitsV1 {
        let mut limits = self.limits.clone();
        limits.tokens = self.tokens.remaining(&Scope::Run).unwrap_or(0);
        limits.max_attempts =
            u64::from(limits.max_attempts).saturating_sub(self.begun_attempts()) as u32;
        limits
    }

    /// Invalidate the active plan after an explicitly recorded business-source revision.
    /// The old ledger and reservation identities survive, including late usage. No resource
    /// check blocks recording updated intent; installing or dispatching new work remains bound.
    pub fn invalidate_plan(&mut self, now_unix_ms: u64) -> Result<(), String> {
        if now_unix_ms < self.last_time
            || self
                .reservations
                .values()
                .any(|held| !held.released && held.settled.is_none())
        {
            return Err(
                "Cannot revise a Task budget with pending work or a backwards clock".into(),
            );
        }
        self.retired_attempts = self.begun_attempts();
        self.nodes.clear();
        self.call_limits.clear();
        self.token_scopes.clear();
        self.deferred_verification = true;
        self.last_time = now_unix_ms;
        Ok(())
    }

    /// The single bootstrap-to-execution barrier keeps the token ledger, reservation IDs,
    /// late-usage authority and all begun Attempts. A caller must separately admit its exact
    /// compiled graph and generated origins through the common Store.
    pub fn enter_execution(
        &mut self,
        allowances: BTreeMap<String, NodeAllowance>,
        call_limits: BTreeMap<String, u32>,
        now_unix_ms: u64,
    ) -> Result<(), String> {
        self.install_graph(allowances, call_limits, now_unix_ms, false)
    }

    /// Install a graph only after invalidation or the fixed planning handoff. The Store owns
    /// those barriers and supplies whether this graph is the captured preparation Pipeline.
    pub fn install_graph(
        &mut self,
        allowances: BTreeMap<String, NodeAllowance>,
        call_limits: BTreeMap<String, u32>,
        now_unix_ms: u64,
        preparation: bool,
    ) -> Result<(), String> {
        self.install_graph_with_token_scopes(
            allowances,
            call_limits,
            BTreeMap::new(),
            now_unix_ms,
            preparation,
        )
    }

    pub fn install_graph_with_token_scopes(
        &mut self,
        allowances: BTreeMap<String, NodeAllowance>,
        call_limits: BTreeMap<String, u32>,
        token_scopes: BTreeMap<String, TaskTokenScope>,
        now_unix_ms: u64,
        preparation: bool,
    ) -> Result<(), String> {
        if !self.deferred_verification
            || self.breached
            || now_unix_ms < self.last_time
            || now_unix_ms >= self.limits.deadline_unix_ms
            || self
                .reservations
                .values()
                .any(|held| !held.released && held.settled.is_none())
        {
            return Err(
                "Planning cannot advance with pending work, expired authority or a spent barrier"
                    .into(),
            );
        }
        let remaining = self.remaining_limits();
        if remaining.tokens < self.limits.verification.tokens
            || remaining.max_attempts < self.limits.verification.attempts
            || self.limits.deadline_unix_ms - now_unix_ms < self.limits.verification.wall_ms
        {
            return Err("Planning spent capacity still owed to business verification".into());
        }
        let next = Self::new(remaining, allowances)?.with_call_limits(call_limits)?;
        let next = if preparation {
            next.with_deferred_verification()?
        } else {
            next
        };
        // Installing a new graph can fail on a retained scope's committed usage. Validate
        // the complete replacement before changing any node credit or active membership.
        let mut candidate = self.clone();
        candidate.retired_attempts = self.begun_attempts();
        candidate.nodes = next.nodes;
        candidate.call_limits = next.call_limits;
        candidate.deferred_verification = preparation;
        candidate.last_time = now_unix_ms;
        candidate.install_token_scopes(token_scopes)?;
        *self = candidate;
        Ok(())
    }

    /// Child Pipelines constrain the same ledger. They do not allocate a fresh allowance.
    pub fn with_call_limits(mut self, limits: BTreeMap<String, u32>) -> Result<Self, String> {
        if !self.reservations.is_empty() {
            return Err("Call limits must be captured before reservation".into());
        }
        for (scope, limit) in &limits {
            if *limit == 0 || !scope.split('.').all(review_core::task::is_name) {
                return Err("Invalid Task call Attempt limit".into());
            }
            let required: u64 = self
                .nodes
                .iter()
                .filter(|(node, _)| within(node, scope))
                .map(|(_, account)| u64::from(account.allowance.verification_attempts))
                .sum();
            if required > u64::from(*limit) {
                return Err(format!("Call {scope} cannot protect its verifier Attempts"));
            }
        }
        self.call_limits = limits;
        Ok(self)
    }

    /// Protect the declared allocation until its corresponding verification Attempts start.
    /// Any extra Task-kind reserve remains held while at least one verifier is still owed work.
    fn protected_after(&self, candidate: &str) -> Result<VerificationReserveV1, String> {
        if self.deferred_verification {
            return Ok(self.limits.verification.clone());
        }
        let mut consumed = VerificationReserveV1 {
            tokens: 0,
            attempts: 0,
            wall_ms: 0,
        };
        let mut remaining = 0u32;
        for (name, account) in &self.nodes {
            let requested = u32::from(name == candidate);
            let used = account
                .begun
                .checked_add(account.prepared)
                .and_then(|n| n.checked_add(requested))
                .ok_or("Task Attempt count overflow")?
                .min(account.allowance.verification_attempts);
            consumed.attempts = consumed
                .attempts
                .checked_add(used)
                .ok_or("Task Attempt count overflow")?;
            consumed.tokens = add(
                consumed.tokens,
                account
                    .allowance
                    .tokens_per_attempt
                    .checked_mul(u64::from(used))
                    .ok_or("Task token total overflow")?,
            )?;
            consumed.wall_ms = add(
                consumed.wall_ms,
                account
                    .allowance
                    .wall_ms_per_attempt
                    .checked_mul(u64::from(used))
                    .ok_or("Task wall total overflow")?,
            )?;
            remaining = remaining
                .checked_add(account.allowance.verification_attempts - used)
                .ok_or("Task Attempt count overflow")?;
        }
        if remaining == 0 {
            return Ok(VerificationReserveV1 {
                tokens: 0,
                attempts: 0,
                wall_ms: 0,
            });
        }
        Ok(VerificationReserveV1 {
            tokens: self
                .limits
                .verification
                .tokens
                .saturating_sub(consumed.tokens),
            attempts: self
                .limits
                .verification
                .attempts
                .saturating_sub(consumed.attempts),
            wall_ms: self
                .limits
                .verification
                .wall_ms
                .saturating_sub(consumed.wall_ms),
        })
    }

    pub fn prepare(&mut self, node: &str, now_unix_ms: u64) -> Result<TaskReservation, String> {
        if self.breached || now_unix_ms < self.last_time {
            return Err("Task budget breached or policy clock moved backwards".into());
        }
        let account = self
            .nodes
            .get(node)
            .ok_or_else(|| format!("Unknown Task node {node}"))?;
        let allowance = &account.allowance;
        if account.begun + account.prepared >= allowance.max_attempts {
            return Err(format!("Task node {node} exhausted its Attempt limit"));
        }
        let protected = self.protected_after(node)?;
        let used_attempts: u64 = self.retired_attempts
            + self
                .nodes
                .values()
                .map(|a| u64::from(a.begun) + u64::from(a.prepared))
                .sum::<u64>();
        if used_attempts + 1 + u64::from(protected.attempts) > u64::from(self.limits.max_attempts) {
            return Err("Task Attempt limit protects still-required verification".into());
        }
        for (scope, limit) in &self.call_limits {
            if !within(node, scope) {
                continue;
            }
            let mut occupied = 1u64;
            for (member, account) in self
                .nodes
                .iter()
                .filter(|(member, _)| within(member, scope))
            {
                let used = u64::from(account.begun) + u64::from(account.prepared);
                occupied = add(occupied, used)?;
                occupied = add(
                    occupied,
                    u64::from(account.allowance.verification_attempts)
                        .saturating_sub(used + u64::from(member == node)),
                )?;
            }
            if occupied > u64::from(*limit) {
                return Err(format!(
                    "Call {scope} Attempt limit protects still-required verification"
                ));
            }
        }
        let deadline = add(now_unix_ms, allowance.wall_ms_per_attempt)?;
        if now_unix_ms >= self.limits.deadline_unix_ms
            || add(deadline, protected.wall_ms)? > self.limits.deadline_unix_ms
        {
            return Err("Task deadline protects still-required verification".into());
        }
        if add(allowance.tokens_per_attempt, protected.tokens)?
            > self.tokens.remaining(&Scope::Run).unwrap_or(0)
        {
            return Err("Task token limit protects still-required verification".into());
        }
        let scopes = self.reservation_scopes(node)?;
        let tokens = self
            .tokens
            .reserve(&scopes, allowance.tokens_per_attempt)
            .map_err(|e| e.to_string())?;
        let public = TaskReservation {
            id: tokens.id.clone(),
            node: node.into(),
            tokens: tokens.amount,
            deadline_unix_ms: deadline,
        };
        self.nodes.get_mut(node).expect("validated node").prepared += 1;
        self.reservations.insert(
            public.id.clone(),
            Held {
                public: public.clone(),
                tokens,
                begun: false,
                settled: None,
                released: false,
            },
        );
        self.last_time = now_unix_ms;
        Ok(public)
    }

    pub fn begin(&mut self, id: &str, now_unix_ms: u64) -> Result<(), String> {
        let held = self
            .reservations
            .get_mut(id)
            .ok_or("Unknown Task reservation")?;
        if held.released
            || held.settled.is_some()
            || now_unix_ms < self.last_time
            || now_unix_ms >= held.public.deadline_unix_ms
            || self.breached
        {
            return Err("Task reservation is not dispatchable".into());
        }
        if !held.begun {
            let account = self
                .nodes
                .get_mut(&held.public.node)
                .expect("reservation node");
            account.prepared -= 1;
            account.begun += 1;
            held.begun = true;
        }
        self.last_time = now_unix_ms;
        Ok(())
    }

    /// Only a dispatch that provably never began can return its Attempt and token credit.
    pub fn release(&mut self, id: &str) -> Result<(), String> {
        let held = self
            .reservations
            .get_mut(id)
            .ok_or("Unknown Task reservation")?;
        if held.begun || held.settled.is_some() {
            return Err("A started Task Attempt cannot release its charge".into());
        }
        if !held.released {
            self.tokens.release(&held.tokens);
            self.nodes
                .get_mut(&held.public.node)
                .expect("reservation node")
                .prepared -= 1;
            held.released = true;
        }
        Ok(())
    }

    /// Failures and fenced work use the same settlement as successful work. A reported
    /// overrun stops new dispatch even when the wider Task limit would otherwise have room.
    pub fn settle(&mut self, id: &str, actual: u64) -> Result<(), String> {
        self.settle_exact(id, u128::from(actual))
    }

    pub fn settle_exact(&mut self, id: &str, actual: u128) -> Result<(), String> {
        let held = self
            .reservations
            .get_mut(id)
            .ok_or("Unknown Task reservation")?;
        if !held.begun || held.released {
            return Err("Task settlement has no started Attempt".into());
        }
        if let Some(old) = held.settled {
            return if old == actual {
                Ok(())
            } else {
                Err("Conflicting Task settlement".into())
            };
        }
        // The shared token ledger retains wide aggregate charges without narrowing usage.
        let observed = self.tokens.observed_charge_exact(&held.tokens);
        if actual < observed {
            return Err("Task settlement cannot refund already observed usage".into());
        }
        if let Err(error) = self.tokens.charge_exact(&held.tokens, actual) {
            self.breached = true;
            return Err(error);
        }
        self.breached |= actual > u128::from(held.tokens.amount);
        held.settled = Some(actual);
        Ok(())
    }

    pub fn committed_tokens(&self) -> u128 {
        self.tokens.committed(&Scope::Run)
    }

    /// A Provider observation commits known usage even while its Attempt is running. The
    /// unspent reservation stays held until settlement; an overrun immediately stops dispatch.
    /// After settlement, observations can only increase the charge, including abandoned work.
    pub fn observe_charge(&mut self, id: &str, actual: u64) -> Result<(), String> {
        self.observe_charge_exact(id, u128::from(actual))
    }

    pub fn observe_charge_exact(&mut self, id: &str, actual: u128) -> Result<(), String> {
        let held = self
            .reservations
            .get_mut(id)
            .ok_or("Unknown Task reservation")?;
        if !held.begun || held.released {
            return Err("Usage has no started Attempt".into());
        }
        if let Some(previous) = held.settled {
            if actual > previous {
                let mut updated = self.tokens.clone();
                for scope in held.tokens.scopes() {
                    let Some(total) = self.tokens.committed(scope).checked_add(actual - previous)
                    else {
                        self.breached = true;
                        return Err("Observed budget total overflow".into());
                    };
                    updated = updated.with_exact_committed(scope.clone(), total);
                }
                self.tokens = updated;
                held.settled = Some(actual);
            }
        } else if let Err(error) = self.tokens.observe_charge_exact(&held.tokens, actual) {
            self.breached = true;
            return Err(error);
        }
        self.breached |= actual > u128::from(held.tokens.amount);
        Ok(())
    }
    pub fn reserved_tokens(&self) -> u128 {
        self.tokens.reserved(&Scope::Run)
    }
    pub fn begun_attempts(&self) -> u64 {
        self.retired_attempts + self.nodes.values().map(|a| u64::from(a.begun)).sum::<u64>()
    }
    pub fn breached(&self) -> bool {
        self.breached
    }
}

fn within(node: &str, scope: &str) -> bool {
    node.strip_prefix(scope)
        .is_some_and(|tail| tail.starts_with('.'))
}
