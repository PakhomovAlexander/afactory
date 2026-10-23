//! One Task's admission accounting, including capacity still owed to its verifiers.
//!
//! This is a deterministic projection, not storage or an execution capability. The common
//! Store publishes the corresponding transitions under its writer lease before dispatch.
//! Replaying those transitions reconstructs this same ledger; children never construct one.

use std::collections::BTreeMap;

use review_core::task::{TaskLimitsV1, VerificationReserveV1};
use serde::{Deserialize, Serialize};

use crate::{Budget, BudgetLedger, BudgetScope, Reservation};

mod scopes;
pub use scopes::TaskTokenScope;
mod owned;
pub use owned::OwnedNodeAllowance;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeAllowance {
    pub tokens_per_attempt: u64,
    pub wall_ms_per_attempt: u64,
    pub max_attempts: u32,
    /// Attempts protected from other nodes. Only trusted verifier slots have this credit.
    pub verification_attempts: u32,
}

/// One writer loss or host interruption per node is dispatched again unless its slot declares
/// otherwise (ADR-0200).
pub use review_core::task::pipeline::{DEFAULT_RECOVERY_ATTEMPTS, MAX_RECOVERY_ATTEMPTS};

#[derive(Debug, Clone)]
struct NodeAccount {
    allowance: NodeAllowance,
    begun: u32,
    prepared: u32,
    /// Begun Attempts settled as abandoned: their writer was lost or their host cancelled them.
    /// They are counted here instead of against any Attempt limit, and bounded by
    /// `recovery_allowance` instead.
    recovered: u32,
    recovery_allowance: u32,
}

impl NodeAccount {
    fn new(allowance: NodeAllowance) -> Self {
        Self {
            allowance,
            begun: 0,
            prepared: 0,
            recovered: 0,
            recovery_allowance: DEFAULT_RECOVERY_ATTEMPTS,
        }
    }

    /// Begun Attempts that ran to an answer, a failure included: what the Attempt limits count.
    fn chargeable(&self) -> u32 {
        self.begun - self.recovered
    }
}

/// One node's recovery state: its abandoned Attempts against the allowance that bounds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeRecovery {
    pub chargeable: u32,
    pub abandoned: u32,
    pub allowance: u32,
}

impl NodeRecovery {
    /// Whether another abandoned Attempt has used up the allowance, so the node cannot be
    /// dispatched again.
    pub fn exhausted(&self) -> bool {
        self.abandoned > self.allowance
    }
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
    recovered: bool,
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
    /// Chargeable Attempts of graphs this ledger no longer runs.
    retired_attempts: u64,
    /// Abandoned Attempts of graphs this ledger no longer runs.
    retired_recovered: u64,
    deferred_verification: bool,
    owned_templates: BTreeMap<String, OwnedNodeAllowance>,
    owned_children: BTreeMap<String, Vec<String>>,
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
            let exceeded = [
                (minimum.tokens > limits.verification.tokens, "tokens"),
                (minimum.attempts > limits.verification.attempts, "attempts"),
                (minimum.wall_ms > limits.verification.wall_ms, "wall_ms"),
            ]
            .into_iter()
            .filter_map(|(exceeds, resource)| exceeds.then_some(resource))
            .collect::<Vec<_>>()
            .join(", ");
            // These products were checked while computing the minimum above. Only protected
            // Attempts contribute; optional retries and ordinary Workers are not reserved here.
            let contributions = allowances
                .iter()
                .filter(|(_, allowance)| allowance.verification_attempts > 0)
                .map(|(name, allowance)| {
                    let count = u64::from(allowance.verification_attempts);
                    format!(
                        "{name}: tokens={}, attempts={}, wall_ms={}",
                        allowance.tokens_per_attempt * count,
                        count,
                        allowance.wall_ms_per_attempt * count,
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!(
                "Task cannot protect the compiled verifier allocation: \
                 required tokens={}, attempts={}, wall_ms={}; \
                 available verification reserve tokens={}, attempts={}, wall_ms={}; \
                 exceeded: {exceeded}; protected node contributions: [{contributions}]. \
                 These are summed declared allowances, not estimated runtime. \
                 Review the Task verification reserve and pipeline allowances before retrying.",
                minimum.tokens,
                minimum.attempts,
                minimum.wall_ms,
                limits.verification.tokens,
                limits.verification.attempts,
                limits.verification.wall_ms,
            ));
        }
        Ok(Self {
            tokens: BudgetLedger::default().with_limit(BudgetScope::Run, Budget::of(limits.tokens)),
            limits,
            nodes: allowances
                .into_iter()
                .map(|(name, allowance)| (name, NodeAccount::new(allowance)))
                .collect(),
            reservations: BTreeMap::new(),
            last_time: 0,
            breached: false,
            call_limits: BTreeMap::new(),
            token_scopes: BTreeMap::new(),
            captured_token_scopes: BTreeMap::new(),
            retired_attempts: 0,
            retired_recovered: 0,
            deferred_verification: false,
            owned_templates: BTreeMap::new(),
            owned_children: BTreeMap::new(),
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
        limits.tokens = self.tokens.remaining(&BudgetScope::Run).unwrap_or(0);
        limits.max_attempts =
            u64::from(limits.max_attempts).saturating_sub(self.chargeable_attempts()) as u32;
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
        self.retire();
        self.nodes.clear();
        self.call_limits.clear();
        self.token_scopes.clear();
        self.owned_templates.clear();
        self.owned_children.clear();
        self.deferred_verification = true;
        self.last_time = now_unix_ms;
        Ok(())
    }

    /// Install a graph only after invalidation or the fixed planning handoff. The Store owns
    /// those barriers and supplies whether this graph is the captured preparation Pipeline.
    /// The bootstrap-to-execution barrier keeps the token ledger, reservation IDs, late-usage
    /// authority and all begun Attempts; a caller must separately admit its exact compiled
    /// graph and generated origins through the common Store.
    pub fn install_graph_with_owned_templates(
        &mut self,
        allowances: BTreeMap<String, NodeAllowance>,
        call_limits: BTreeMap<String, u32>,
        token_scopes: BTreeMap<String, TaskTokenScope>,
        owned_templates: BTreeMap<String, OwnedNodeAllowance>,
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
        let next = Self::new(remaining, allowances)?
            .with_call_limits(call_limits)?
            .with_owned_templates(owned_templates)?;
        let next = if preparation {
            next.with_deferred_verification()?
        } else {
            next
        };
        // Installing a new graph can fail on a retained scope's committed usage. Validate
        // the complete replacement before changing any node credit or active membership.
        let mut candidate = self.clone();
        candidate.retire();
        candidate.nodes = next.nodes;
        candidate.call_limits = next.call_limits;
        candidate.deferred_verification = preparation;
        candidate.owned_templates = next.owned_templates;
        candidate.owned_children.clear();
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
                .chargeable()
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

    /// One more Attempt for `node` than its compiled allowance, for a gate retry
    /// (`TaskGateRetried@1`): its earlier Attempts stay counted and charged, and the Task's own
    /// Attempt, token and wall limits and its verification reserve bound the new one as they
    /// bound every Attempt, so a Task without room for it refuses it at [`Self::prepare`].
    pub fn allow_retry(&mut self, node: &str) -> Result<(), String> {
        let account = self
            .nodes
            .get_mut(node)
            .ok_or_else(|| format!("Unknown Task node {node}"))?;
        account.allowance.max_attempts = account
            .allowance
            .max_attempts
            .checked_add(1)
            .ok_or("Task Attempt count overflow")?;
        Ok(())
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
        if account.recovered > account.recovery_allowance {
            return Err(format!(
                "Task node {node} exhausted its recovery allowance: {} abandoned Attempts, \
                 recovery allowance {}",
                account.recovered, account.recovery_allowance
            ));
        }
        if account.chargeable() + account.prepared >= allowance.max_attempts {
            return Err(format!("Task node {node} exhausted its Attempt limit"));
        }
        let protected = self.protected_after(node)?;
        let used_attempts: u64 = self.retired_attempts
            + self
                .nodes
                .values()
                .map(|a| u64::from(a.chargeable()) + u64::from(a.prepared))
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
                let used = u64::from(account.chargeable()) + u64::from(account.prepared);
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
        let remaining = self.tokens.remaining(&BudgetScope::Run).unwrap_or(0);
        if add(allowance.tokens_per_attempt, protected.tokens)? > remaining {
            return Err(format!(
                "Task token limit protects still-required verification: reservation {} + \
                 protected verification {} > remaining {}",
                group(u128::from(allowance.tokens_per_attempt)),
                group(u128::from(protected.tokens)),
                group(u128::from(remaining)),
            ));
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
                recovered: false,
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
        let node = &held.public.node;
        // Each guard names itself and its numbers, so a refusal says which one fired.
        let refusal = if held.released {
            Some("it was released before it started".to_string())
        } else if let Some(charged) = held.settled {
            Some(format!(
                "it is already settled, charged {} tokens",
                group(charged)
            ))
        } else if now_unix_ms < self.last_time {
            Some(format!(
                "the policy clock moved backwards: now {now_unix_ms} ms < last recorded {} ms",
                self.last_time
            ))
        } else if now_unix_ms >= held.public.deadline_unix_ms {
            Some(format!(
                "its Attempt deadline passed: now {now_unix_ms} ms >= deadline {} ms",
                held.public.deadline_unix_ms
            ))
        } else if self.breached {
            Some(format!(
                "the Task token budget is breached: charged {} of a {} token limit, reservation \
                 {} > remaining {}",
                group(self.tokens.committed(&BudgetScope::Run)),
                group(u128::from(self.limits.tokens)),
                group(u128::from(held.public.tokens)),
                group(u128::from(
                    self.tokens.remaining(&BudgetScope::Run).unwrap_or(0)
                )),
            ))
        } else {
            None
        };
        if let Some(reason) = refusal {
            return Err(format!(
                "Task reservation for node {node} is not dispatchable: {reason}"
            ));
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

    /// Record that a settled Attempt was abandoned: its writer was lost or its host cancelled
    /// it before it answered. It stops counting against its node's, its calls' and the Task's
    /// Attempt limits and counts against the node's recovery allowance instead; its charge is
    /// whatever its settlement recorded. Idempotent for the same reservation.
    pub fn recover(&mut self, id: &str) -> Result<(), String> {
        let held = self
            .reservations
            .get_mut(id)
            .ok_or("Unknown Task reservation")?;
        if !held.begun || held.released || held.settled.is_none() {
            return Err("Only a settled started Task Attempt can be recovered".into());
        }
        if !held.recovered {
            self.nodes
                .get_mut(&held.public.node)
                .ok_or("A recovered Task Attempt's node is not in the current graph")?
                .recovered += 1;
            held.recovered = true;
        }
        Ok(())
    }

    /// Install the recovery allowance each node's slot declares; every other node keeps
    /// [`DEFAULT_RECOVERY_ATTEMPTS`].
    pub fn with_recovery_allowances(
        mut self,
        allowances: &BTreeMap<String, u32>,
    ) -> Result<Self, String> {
        self.install_recovery_allowances(allowances)?;
        Ok(self)
    }

    pub fn install_recovery_allowances(
        &mut self,
        allowances: &BTreeMap<String, u32>,
    ) -> Result<(), String> {
        for (node, allowance) in allowances {
            if *allowance > MAX_RECOVERY_ATTEMPTS {
                return Err(format!(
                    "Task node {node} declares {allowance} recovery Attempts; at most \
                     {MAX_RECOVERY_ATTEMPTS} are allowed"
                ));
            }
            self.nodes
                .get_mut(node)
                .ok_or_else(|| format!("Unknown Task node {node}"))?
                .recovery_allowance = *allowance;
        }
        Ok(())
    }

    /// The recovery state of one node of the current graph.
    pub fn recovery(&self, node: &str) -> Option<NodeRecovery> {
        self.nodes.get(node).map(|account| NodeRecovery {
            chargeable: account.chargeable(),
            abandoned: account.recovered,
            allowance: account.recovery_allowance,
        })
    }

    pub fn committed_tokens(&self) -> u128 {
        self.tokens.committed(&BudgetScope::Run)
    }

    /// A Provider observation commits known usage even while its Attempt is running. The
    /// unspent reservation stays held until settlement; an overrun immediately stops dispatch.
    /// After settlement, observations can only increase the charge, including abandoned work.
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
        self.tokens.reserved(&BudgetScope::Run)
    }
    /// Every Attempt that began, abandoned ones included.
    pub fn begun_attempts(&self) -> u64 {
        self.retired_attempts
            + self.retired_recovered
            + self.nodes.values().map(|a| u64::from(a.begun)).sum::<u64>()
    }

    /// The begun Attempts the Attempt limits count: every one except those settled as abandoned.
    pub fn chargeable_attempts(&self) -> u64 {
        self.retired_attempts
            + self
                .nodes
                .values()
                .map(|a| u64::from(a.chargeable()))
                .sum::<u64>()
    }

    /// Move the current graph's Attempts into the retired counts before it is replaced.
    fn retire(&mut self) {
        self.retired_attempts = self.chargeable_attempts();
        self.retired_recovered += self
            .nodes
            .values()
            .map(|a| u64::from(a.recovered))
            .sum::<u64>();
    }
    pub fn breached(&self) -> bool {
        self.breached
    }
}

/// A token count with thousands separators, as af prints counts to people: `1,500,000`.
pub fn group(value: u128) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

fn within(node: &str, scope: &str) -> bool {
    node.strip_prefix(scope)
        .is_some_and(|tail| tail.starts_with('.'))
}
