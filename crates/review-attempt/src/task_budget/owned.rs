//! Captured owned-child capacity. Templates are scope anchors, never reservable parents.
use super::{NodeAccount, NodeAllowance, TaskBudget, within};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedNodeAllowance {
    pub allowance: NodeAllowance,
    pub max_children: u32,
}

impl TaskBudget {
    pub fn with_owned_templates(
        mut self,
        templates: BTreeMap<String, OwnedNodeAllowance>,
    ) -> Result<Self, String> {
        if !self.reservations.is_empty() || !self.owned_templates.is_empty() {
            return Err("Owned Task templates must be captured before reservation".into());
        }
        for (parent, template) in &templates {
            let allowance = &template.allowance;
            if parent.is_empty()
                || !parent.split('.').all(review_core::task::is_name)
                || template.max_children == 0
                || allowance.max_attempts == 0
                || allowance.verification_attempts != 0
                || allowance.wall_ms_per_attempt == 0
                || allowance.tokens_per_attempt > self.limits.tokens
                || self
                    .nodes
                    .keys()
                    .any(|node| node == parent || within(node, parent))
                || templates
                    .keys()
                    .any(|other| other != parent && within(other, parent))
            {
                return Err(format!(
                    "Invalid non-reservable Task child template {parent}"
                ));
            }
        }
        self.owned_templates = templates;
        Ok(self)
    }

    /// Pure registration consumes no budget and remains possible after exhaustion/expiry.
    /// The Store proves the complete source set and current parent authority before calling.
    pub fn register_owned_children(
        &mut self,
        parent: &str,
        children: &[String],
    ) -> Result<(), String> {
        let template = self
            .owned_templates
            .get(parent)
            .ok_or("Unknown Task child owner")?;
        if let Some(previous) = self.owned_children.get(parent) {
            return if previous == children {
                Ok(())
            } else {
                Err("Task child registration changed its captured set".into())
            };
        }
        let mut unique = BTreeSet::new();
        if children.is_empty()
            || children.len() > template.max_children as usize
            || children.iter().any(|child| {
                !within(child, parent)
                    || child.split('.').count() != parent.split('.').count() + 1
                    || !child.split('.').all(review_core::task::is_name)
                    || !unique.insert(child)
                    || self.nodes.contains_key(child)
                    || self.owned_templates.contains_key(child)
            })
        {
            return Err("Task children differ from their bounded owner namespace".into());
        }
        for child in children {
            self.nodes.insert(
                child.clone(),
                NodeAccount {
                    allowance: template.allowance.clone(),
                    begun: 0,
                    prepared: 0,
                },
            );
        }
        self.owned_children.insert(parent.into(), children.to_vec());
        Ok(())
    }

    /// Install an exact, separately approved experimental allocation into this Task's existing
    /// ledger. The Store validates the signed closure and remaining aggregate allowance first;
    /// this projection ensures the registered nodes can only spend their declared sub-allocations.
    pub fn register_experimental_children(
        &mut self,
        parent: &str,
        children: &BTreeMap<String, NodeAllowance>,
        max_children: u32,
    ) -> Result<(), String> {
        let names = children.keys().cloned().collect::<Vec<_>>();
        if let Some(previous) = self.owned_children.get(parent) {
            return if previous == &names {
                Ok(())
            } else {
                Err("Experimental child registration changed its approved closure".into())
            };
        }
        let mut unique = BTreeSet::new();
        if children.is_empty()
            || children.len() > max_children as usize
            || children.iter().any(|(child, allowance)| {
                !within(child, parent)
                    || !child.split('.').all(review_core::task::is_name)
                    || !unique.insert(child)
                    || self.nodes.contains_key(child)
                    || self.owned_templates.contains_key(child)
                    || allowance.max_attempts == 0
                    || allowance.verification_attempts > allowance.max_attempts
                    || allowance.wall_ms_per_attempt == 0
                    || allowance.tokens_per_attempt > self.limits.tokens
            })
        {
            return Err(
                "Experimental children differ from their approved bounded namespace".into(),
            );
        }
        let mut verifier_tokens = 0u64;
        let mut verifier_attempts = 0u32;
        let mut verifier_wall_ms = 0u64;
        for allowance in self
            .nodes
            .values()
            .map(|account| &account.allowance)
            .chain(children.values())
        {
            let count = u64::from(allowance.verification_attempts);
            verifier_attempts = verifier_attempts
                .checked_add(allowance.verification_attempts)
                .ok_or("Experimental verifier Attempt reservation overflow")?;
            verifier_tokens = verifier_tokens
                .checked_add(
                    allowance
                        .tokens_per_attempt
                        .checked_mul(count)
                        .ok_or("Experimental verifier token reservation overflow")?,
                )
                .ok_or("Experimental verifier token reservation overflow")?;
            verifier_wall_ms = verifier_wall_ms
                .checked_add(
                    allowance
                        .wall_ms_per_attempt
                        .checked_mul(count)
                        .ok_or("Experimental verifier wall reservation overflow")?,
                )
                .ok_or("Experimental verifier wall reservation overflow")?;
        }
        if verifier_attempts > self.limits.verification.attempts
            || verifier_tokens > self.limits.verification.tokens
            || verifier_wall_ms > self.limits.verification.wall_ms
        {
            return Err(
                "Experimental closure exceeds the Task's protected verifier allocation".into(),
            );
        }
        for (child, allowance) in children {
            self.nodes.insert(
                child.clone(),
                NodeAccount {
                    allowance: allowance.clone(),
                    begun: 0,
                    prepared: 0,
                },
            );
        }
        self.owned_children.insert(parent.into(), names);
        Ok(())
    }
}
