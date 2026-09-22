//! Broker evidence belongs to an existing common Task Attempt. These payloads describe
//! captured authority; only the Store can bind a handle or authorize a connector operation.
//! Late paid receipts retain usage without granting dispatch or output publication.

use super::{is_name, require, safe_number};
use crate::{BrokerLeaseV1, BrokerOperationPolicyV1, BrokerOperationReceiptV2, is_digest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const TASK_BROKER_BINDING_V1: &str = "af/TaskBrokerBinding@1";
pub const TASK_BROKER_OPERATION_V1: &str = "af/TaskBrokerOperation@1";

/// The business Worker slot whose captured authority one Broker handle serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskBrokerTargetV1 {
    Worker {
        slot: String,
        invocation_policy_id: String,
    },
}

impl TaskBrokerTargetV1 {
    pub fn policy_id(&self) -> &str {
        let Self::Worker {
            invocation_policy_id,
            ..
        } = self;
        invocation_policy_id
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(self.policy_id()),
            "Task Broker target needs an exact captured policy",
        )?;
        let Self::Worker { slot, .. } = self;
        require(
            slot.split('.').all(is_name),
            "Task Broker Worker needs a qualified slot",
        )
    }
}

/// The exact captured binding and started Attempt behind one opaque Broker handle.
/// Worker identity comes from its exact plan slot. The target installs no package binding,
/// enlarges no reservation, and creates no other Attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBrokerBindingV1 {
    pub task_id: String,
    pub task_revision_id: String,
    pub plan_id: String,
    pub invocation_id: String,
    pub context_id: String,
    pub attempt_id: String,
    pub reservation_id: String,
    pub writer: String,
    pub writer_epoch: u64,
    /// Qualified common Task node, distinct from the original Review node in `lease`.
    pub node: String,
    pub target: TaskBrokerTargetV1,
    pub lease: BrokerLeaseV1,
    pub handle_id: String,
    pub operations: Vec<BrokerOperationPolicyV1>,
}

impl TaskBrokerBindingV1 {
    pub fn artifact_refs(&self) -> Vec<&str> {
        vec![
            &self.task_revision_id,
            &self.plan_id,
            &self.invocation_id,
            &self.context_id,
            self.target.policy_id(),
        ]
    }

    pub fn validate(&self) -> Result<(), String> {
        self.target.validate()?;
        require(
            self.artifact_refs().into_iter().all(is_digest),
            "Task Broker binding needs exact captured artifact identities",
        )?;
        require(
            is_name(&self.task_id)
                && is_name(&self.writer)
                && self.node.split('.').all(is_name)
                && self.writer_epoch > 0
                && safe_number(self.writer_epoch)
                && self.handle_id.len() == 26
                && self
                    .handle_id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
            "Task Broker binding has invalid Task, node, slot, writer or handle identity",
        )?;
        // Keep the existing TaskExecutionRecord reservation syntax. The Store compares
        // it to the actual original reservation; this string grants no allowance.
        require(
            self.reservation_id
                .strip_prefix("reservation:")
                .is_some_and(|number| {
                    !number.is_empty()
                        && number.len() <= 20
                        && number.bytes().all(|b| b.is_ascii_digit())
                }),
            "Task Broker binding has an invalid reservation identity",
        )?;
        self.lease.validate()?;
        require(
            self.attempt_id == self.lease.attempt_id && self.writer_epoch == self.lease.lease_epoch,
            "Task Broker lease differs from its original Attempt or writer epoch",
        )?;
        require(
            !self.operations.is_empty(),
            "Task Broker binding needs captured operations",
        )?;
        let mut names = BTreeSet::new();
        for operation in &self.operations {
            operation.validate()?;
            require(
                names.insert(&operation.name),
                "Task Broker binding has duplicate operation names",
            )?;
        }
        crate::broker_authority_usage(&self.operations)?;
        Ok(())
    }
}

/// One operation receipt under an already captured Task Broker binding. Receipt digests
/// describe connector bytes and need not name CAS objects; only the binding is a CAS ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBrokerOperationV1 {
    pub binding_id: String,
    pub receipt: BrokerOperationReceiptV2,
}

impl TaskBrokerOperationV1 {
    pub fn artifact_refs(&self) -> Vec<&str> {
        vec![&self.binding_id]
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.binding_id),
            "Task Broker operation needs its exact binding",
        )?;
        self.receipt.validate()
    }
}

/// Additive Task-log evidence. `record_id` names a typed binding or operation artifact.
/// Store admission checks binding currentness; recording late paid work grants no authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBrokerTransitionV1 {
    pub now_unix_ms: u64,
    pub record_id: String,
}

impl TaskBrokerTransitionV1 {
    pub fn artifact_refs(&self) -> Vec<&str> {
        vec![&self.record_id]
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.now_unix_ms > 0 && safe_number(self.now_unix_ms) && is_digest(&self.record_id),
            "Task Broker transition needs bounded policy time and an exact record",
        )
    }
}
