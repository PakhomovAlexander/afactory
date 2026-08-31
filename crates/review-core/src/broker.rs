//! Durable M6.3 contracts for reviewer Execution Bindings and broker operation receipts.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

const JSON_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn opaque_id(value: &str) -> bool {
    value.len() == 26
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn symbolic_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'-' | b'_'))
        })
}

/// How one reviewer obtains external capability. Formats before pipeline v4 have no such claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerCredentialModeV1 {
    CredentialFree,
    Brokered,
    TrustedUnsafe,
}

/// The exact durable Attempt epoch one opaque handle authorizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerLeaseV1 {
    pub campaign_id: String,
    pub round_event_id: String,
    pub node_id: String,
    pub attempt_id: String,
    pub lease_epoch: u64,
}

impl BrokerLeaseV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.campaign_id.trim().is_empty()
            || !opaque_id(&self.round_event_id)
            || self.node_id.trim().is_empty()
            || !opaque_id(&self.attempt_id)
            || self.lease_epoch == 0
            || self.lease_epoch > JSON_SAFE_INTEGER
        {
            return Err("Broker lease has invalid Campaign, Round, node, Attempt, or epoch".into());
        }
        Ok(())
    }
}

/// One project-authorized external operation. `destination` and `method` are symbolic connector
/// routes, never a Worker-supplied URL or command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerOperationPolicyV1 {
    pub name: String,
    pub destination: String,
    pub method: String,
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
    pub max_calls: u32,
    pub max_usage: u64,
}

impl BrokerOperationPolicyV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !symbolic_name(&self.name)
            || !symbolic_name(&self.destination)
            || !symbolic_name(&self.method)
            || self.max_request_bytes == 0
            || self.max_response_bytes == 0
            || self.max_calls == 0
            || self.max_usage == 0
            || [
                self.max_request_bytes,
                self.max_response_bytes,
                self.max_usage,
            ]
            .into_iter()
            .any(|value| value > JSON_SAFE_INTEGER)
            || self.max_usage == JSON_SAFE_INTEGER
        {
            return Err("Broker operation policy has invalid names or bounds".into());
        }
        Ok(())
    }
}

/// Maximum charge reserved by one brokered Attempt across all of its named operations.
pub fn broker_authority_usage(operations: &[BrokerOperationPolicyV1]) -> Result<u64, String> {
    operations.iter().try_fold(0_u64, |total, operation| {
        let total = total
            .checked_add(operation.max_usage)
            .ok_or_else(|| "Broker authority usage overflow".to_string())?;
        if total > JSON_SAFE_INTEGER {
            return Err("Broker authority usage exceeds the durable numeric domain".into());
        }
        Ok(total)
    })
}

/// Durable evidence that one reviewer Attempt was admitted under its exact execution policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerExecutionBindingV1 {
    pub node: String,
    pub attempt_id: String,
    pub lease_epoch: u64,
    pub credential_mode: BrokerCredentialModeV1,
    pub auto_apply: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<BrokerOperationPolicyV1>,
    pub admitted: bool,
}

impl ReviewerExecutionBindingV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.node.trim().is_empty()
            || !opaque_id(&self.attempt_id)
            || self.lease_epoch == 0
            || self.lease_epoch > JSON_SAFE_INTEGER
            || self
                .broker_handle
                .as_deref()
                .is_some_and(|handle| !opaque_id(handle))
        {
            return Err("Reviewer Execution Binding has invalid identity or epoch".into());
        }
        let mut names = BTreeSet::new();
        for policy in &self.operations {
            policy.validate()?;
            if !names.insert(policy.name.as_str()) {
                return Err("Reviewer Execution Binding has duplicate Broker operations".into());
            }
        }
        broker_authority_usage(&self.operations)?;
        match self.credential_mode {
            BrokerCredentialModeV1::Brokered
                if self.broker_handle.is_some() && !self.operations.is_empty() => {}
            BrokerCredentialModeV1::CredentialFree | BrokerCredentialModeV1::TrustedUnsafe
                if self.broker_handle.is_none() && self.operations.is_empty() => {}
            _ => {
                return Err("Reviewer Execution Binding contradicts its credential mode".into());
            }
        }
        if self.auto_apply && self.credential_mode == BrokerCredentialModeV1::TrustedUnsafe {
            return Err("trusted_unsafe reviewer bindings cannot authorize auto_apply".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerOperationOutcomeV1 {
    Succeeded,
    Refused,
    Failed,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerFailureReasonV1 {
    AuthorityRevoked,
    RequestTooLarge,
    QuotaExceeded,
    ConnectorFailed,
    CredentialExposure,
    ResponseTooLarge,
    UsageOverrun,
}

/// Machine-path- and credential-free receipt for one named broker operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerOperationReceiptV1 {
    pub handle_id: String,
    pub node: String,
    pub attempt_id: String,
    pub lease_epoch: u64,
    pub operation: String,
    pub destination: String,
    pub method: String,
    pub ordinal: u32,
    pub outcome: BrokerOperationOutcomeV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<BrokerFailureReasonV1>,
    pub request_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_digest: Option<String>,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub reserved_usage: u64,
    pub charged_usage: u64,
}

impl BrokerOperationReceiptV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !opaque_id(&self.handle_id)
            || self.node.trim().is_empty()
            || !opaque_id(&self.attempt_id)
            || self.lease_epoch == 0
            || self.lease_epoch > JSON_SAFE_INTEGER
            || !symbolic_name(&self.operation)
            || !symbolic_name(&self.destination)
            || !symbolic_name(&self.method)
            || self.ordinal == 0
            || !crate::is_digest(&self.request_digest)
            || self
                .response_digest
                .as_deref()
                .is_some_and(|digest| !crate::is_digest(digest))
            || [
                self.request_bytes,
                self.response_bytes,
                self.reserved_usage,
                self.charged_usage,
            ]
            .into_iter()
            .any(|value| value > JSON_SAFE_INTEGER)
        {
            return Err("Broker operation receipt has invalid identity, digest, or bounds".into());
        }
        let response_shape_is_valid = self.response_digest.is_some() || self.response_bytes == 0;
        let shape_is_valid = response_shape_is_valid
            && match self.outcome {
                BrokerOperationOutcomeV1::Succeeded => {
                    self.failure_reason.is_none()
                        && self.response_digest.is_some()
                        && self.charged_usage <= self.reserved_usage
                }
                BrokerOperationOutcomeV1::Refused => {
                    matches!(
                        self.failure_reason,
                        Some(
                            BrokerFailureReasonV1::RequestTooLarge
                                | BrokerFailureReasonV1::QuotaExceeded
                        )
                    ) && self.response_digest.is_none()
                        && self.response_bytes == 0
                        && self.charged_usage == 0
                }
                BrokerOperationOutcomeV1::Failed => match self.failure_reason {
                    Some(BrokerFailureReasonV1::ConnectorFailed) => {
                        self.response_digest.is_none()
                            && self.response_bytes == 0
                            && self.charged_usage == self.reserved_usage
                    }
                    Some(BrokerFailureReasonV1::CredentialExposure) => {
                        self.response_digest.is_none()
                            && self.response_bytes == 0
                            && self.charged_usage <= self.reserved_usage
                    }
                    Some(BrokerFailureReasonV1::ResponseTooLarge) => {
                        self.response_digest.is_some() && self.charged_usage <= self.reserved_usage
                    }
                    Some(BrokerFailureReasonV1::UsageOverrun) => {
                        self.charged_usage > self.reserved_usage
                    }
                    _ => false,
                },
                BrokerOperationOutcomeV1::Revoked => {
                    self.failure_reason == Some(BrokerFailureReasonV1::AuthorityRevoked)
                        && (self.response_digest.is_some()
                            || (self.response_bytes == 0
                                && (self.charged_usage == 0 || self.reserved_usage > 0)))
                }
            };
        if !shape_is_valid {
            return Err("Broker operation receipt contradicts its outcome".into());
        }
        Ok(())
    }
}
