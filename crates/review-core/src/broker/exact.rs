//! Exact per-operation connector usage. Reservations and authority retain their frozen bounds.
use super::{BrokerFailureReasonV1, BrokerOperationOutcomeV1, BrokerOperationReceiptV1};
use crate::task::usage::DecimalU64;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerOperationReceiptV2 {
    pub handle_id: String,
    pub node: String,
    pub attempt_id: String,
    pub lease_epoch: u64,
    pub operation: String,
    pub destination: String,
    pub method: String,
    pub ordinal: u32,
    pub outcome: BrokerOperationOutcomeV1,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::task::present_option"
    )]
    pub failure_reason: Option<BrokerFailureReasonV1>,
    pub request_digest: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::task::present_option"
    )]
    pub response_digest: Option<String>,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub reserved_usage: u64,
    pub charged_usage: DecimalU64,
}

impl BrokerOperationReceiptV2 {
    pub fn validate(&self) -> Result<(), String> {
        self.clone()
            .normalized()
            .validate_with_charge_limit(u64::MAX)
    }

    /// Convert only when the exact receipt is representable by the historical contract.
    pub fn try_into_legacy(self) -> Result<BrokerOperationReceiptV1, String> {
        let value = self.normalized();
        value.validate()?;
        Ok(value)
    }

    fn normalized(self) -> BrokerOperationReceiptV1 {
        BrokerOperationReceiptV1 {
            handle_id: self.handle_id,
            node: self.node,
            attempt_id: self.attempt_id,
            lease_epoch: self.lease_epoch,
            operation: self.operation,
            destination: self.destination,
            method: self.method,
            ordinal: self.ordinal,
            outcome: self.outcome,
            failure_reason: self.failure_reason,
            request_digest: self.request_digest,
            response_digest: self.response_digest,
            request_bytes: self.request_bytes,
            response_bytes: self.response_bytes,
            reserved_usage: self.reserved_usage,
            charged_usage: self.charged_usage.get(),
        }
    }
}

impl From<BrokerOperationReceiptV1> for BrokerOperationReceiptV2 {
    fn from(value: BrokerOperationReceiptV1) -> Self {
        Self {
            handle_id: value.handle_id,
            node: value.node,
            attempt_id: value.attempt_id,
            lease_epoch: value.lease_epoch,
            operation: value.operation,
            destination: value.destination,
            method: value.method,
            ordinal: value.ordinal,
            outcome: value.outcome,
            failure_reason: value.failure_reason,
            request_digest: value.request_digest,
            response_digest: value.response_digest,
            request_bytes: value.request_bytes,
            response_bytes: value.response_bytes,
            reserved_usage: value.reserved_usage,
            charged_usage: value.charged_usage.into(),
        }
    }
}
