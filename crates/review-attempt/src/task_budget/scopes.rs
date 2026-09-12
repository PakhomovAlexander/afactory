//! Named aggregate caps share the Task's one reservation and charge. Scope identity is
//! separate from node membership so a later Review Round may reuse node addresses while
//! retaining old scope charges, including observations received after graph replacement.

use super::{Budget, Scope, TaskBudget, add, within};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTokenScope {
    pub tokens: u64,
    /// Exact qualified nodes or prefixes including their dot-separated descendants.
    pub members: BTreeSet<String>,
}

impl TaskTokenScope {
    pub fn contains(&self, node: &str) -> bool {
        self.members
            .iter()
            .any(|member| member == node || within(node, member))
    }
}

fn qualified(name: &str) -> bool {
    !name.is_empty() && name.split('.').all(review_core::task::is_name)
}

impl TaskBudget {
    pub fn with_token_scopes(
        mut self,
        scopes: BTreeMap<String, TaskTokenScope>,
    ) -> Result<Self, String> {
        if !self.reservations.is_empty() {
            return Err("Task token scopes must be captured before reservation".into());
        }
        self.install_token_scopes(scopes)?;
        Ok(self)
    }

    pub(super) fn install_token_scopes(
        &mut self,
        scopes: BTreeMap<String, TaskTokenScope>,
    ) -> Result<(), String> {
        let mut tokens = self.tokens.clone();
        for (name, scope) in &scopes {
            if !qualified(name)
                || scope.tokens > review_core::json::SAFE_INTEGER_MAX as u64
                || scope.members.is_empty()
                || scope.members.iter().any(|member| {
                    !qualified(member)
                        || !self
                            .nodes
                            .keys()
                            .any(|node| node == member || within(node, member))
                })
            {
                return Err(format!("Invalid Task token scope {name}"));
            }
            if self
                .captured_token_scopes
                .get(name)
                .is_some_and(|old| old != scope)
            {
                return Err(format!(
                    "Task token scope {name} differs from its captured authority"
                ));
            }
            let account = Scope::FanOut(name.clone());
            let required = self.scope_verification_after(scope, None)?;
            if add(
                add(tokens.committed(&account), tokens.reserved(&account))?,
                required,
            )? > scope.tokens
            {
                return Err(format!(
                    "Task token scope {name} cannot retain required verification"
                ));
            }
            tokens = tokens.with_limit(account, Budget::of(scope.tokens));
        }
        self.tokens = tokens;
        self.captured_token_scopes.extend(scopes.clone());
        self.token_scopes = scopes;
        Ok(())
    }

    fn scope_verification_after(
        &self,
        scope: &TaskTokenScope,
        candidate: Option<&str>,
    ) -> Result<u64, String> {
        let mut required = 0;
        for (node, account) in self.nodes.iter().filter(|(node, _)| scope.contains(node)) {
            let used = u64::from(account.begun)
                + u64::from(account.prepared)
                + u64::from(candidate == Some(node));
            let owed = u64::from(account.allowance.verification_attempts).saturating_sub(used);
            required = add(
                required,
                account
                    .allowance
                    .tokens_per_attempt
                    .checked_mul(owed)
                    .ok_or("Task scope verifier token overflow")?,
            )?;
        }
        Ok(required)
    }

    pub(super) fn reservation_scopes(&self, node: &str) -> Result<Vec<Scope>, String> {
        let mut scopes = Vec::new();
        for (name, scope) in self
            .token_scopes
            .iter()
            .filter(|(_, scope)| scope.contains(node))
        {
            let account = Scope::FanOut(name.clone());
            let protected = self.scope_verification_after(scope, Some(node))?;
            if add(self.nodes[node].allowance.tokens_per_attempt, protected)?
                > self.tokens.remaining(&account).unwrap_or(0)
            {
                return Err(format!(
                    "Task token scope {name} exhausted or protects still-required verification"
                ));
            }
            scopes.push(account);
        }
        scopes.push(Scope::Run);
        Ok(scopes)
    }

    pub fn scope_committed_tokens(&self, name: &str) -> Option<u64> {
        self.captured_token_scopes
            .contains_key(name)
            .then(|| self.tokens.committed(&Scope::FanOut(name.into())))
    }

    pub fn scope_reserved_tokens(&self, name: &str) -> Option<u64> {
        self.captured_token_scopes
            .contains_key(name)
            .then(|| self.tokens.reserved(&Scope::FanOut(name.into())))
    }
}
