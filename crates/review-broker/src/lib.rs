//! A trusted, revocable capability broker for M6.3.
//!
//! The Worker receives only an opaque [`BrokerHandle`]. Project authority fixes the symbolic
//! destination, method, byte bounds, call count, and usage budget. Credential bytes cross only
//! the trusted [`Connector`] boundary, and every response is withheld until a credential-free
//! receipt is durable.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use review_core::{
    BrokerFailureReasonV1, BrokerLeaseV1, BrokerOperationOutcomeV1, BrokerOperationPolicyV1,
    BrokerOperationReceiptV1,
};
use sha2::{Digest, Sha256};

const JSON_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, PartialEq, Eq)]
pub struct BrokerHandle(String);

impl BrokerHandle {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for BrokerHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("BrokerHandle")
            .field(&self.0)
            .finish()
    }
}

/// Credential material held only by the trusted broker/connector side of the boundary.
pub struct Credential {
    bytes: Vec<u8>,
    forbidden_response_fragments: Vec<Vec<u8>>,
}

impl Credential {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, BrokerError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(BrokerError::InvalidPolicy("credential is empty"));
        }
        let forbidden_response_fragments = credential_representations(&bytes)?;
        Ok(Self {
            bytes,
            forbidden_response_fragments,
        })
    }

    fn expose(&self) -> &[u8] {
        &self.bytes
    }

    fn appears_in(&self, bytes: &[u8]) -> bool {
        self.forbidden_response_fragments.iter().any(|fragment| {
            bytes
                .windows(fragment.len())
                .any(|window| window == fragment)
        }) || encoded_hex_appears(bytes, &self.bytes)
            || percent_encoded_appears(bytes, &self.bytes)
    }
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Credential([redacted])")
    }
}

pub struct ConnectorCall<'a> {
    pub destination: &'a str,
    pub method: &'a str,
    pub request: &'a [u8],
    credential: &'a [u8],
}

impl ConnectorCall<'_> {
    /// Only trusted connector implementations receive this type.
    pub fn credential(&self) -> &[u8] {
        self.credential
    }
}

/// Connector-specific decoded output. Raw authenticated wire bytes must be consumed inside the
/// trusted connector; this value is the credential-free application response proposed for the
/// Worker. The Broker still rejects common raw and encoded credential representations.
pub struct ConnectorReply {
    decoded_response: Vec<u8>,
    charged_usage: u64,
}

impl ConnectorReply {
    pub fn credential_free(decoded_response: impl Into<Vec<u8>>, charged_usage: u64) -> Self {
        Self {
            decoded_response: decoded_response.into(),
            charged_usage,
        }
    }

    fn into_parts(self) -> (Vec<u8>, u64) {
        (self.decoded_response, self.charged_usage)
    }
}

/// Intentionally detail-free: raw provider failures may contain credentials or machine paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectorError;

pub trait Connector: Send + Sync {
    fn execute(&self, call: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityError;

/// Durable authority is checked both before a connector call and after it returns. The second
/// check prevents a response racing with a fence from reaching the Worker.
pub trait LeaseAuthority: Send + Sync {
    fn ensure_current(
        &self,
        lease: &BrokerLeaseV1,
        handle: &BrokerHandle,
    ) -> Result<(), AuthorityError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptError {
    AuthorityRevoked,
    Unavailable,
}

/// The sink must make the receipt durable before returning. A sink failure withholds the
/// response; successful external work never becomes unreceipted Worker input.
pub trait ReceiptSink: Send + Sync {
    fn record(&self, receipt: &BrokerOperationReceiptV1) -> Result<(), ReceiptError>;
}

pub struct BrokerResponse {
    pub body: Vec<u8>,
    pub receipt: BrokerOperationReceiptV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerError {
    InvalidPolicy(&'static str),
    OperationNotAllowed,
    Revoked,
    RequestTooLarge,
    QuotaExceeded,
    ConnectorFailed,
    CredentialExposure,
    ResponseTooLarge,
    UsageOverrun,
    ReceiptFailed,
}

impl std::fmt::Display for BrokerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPolicy(reason) => write!(formatter, "invalid broker policy: {reason}"),
            Self::OperationNotAllowed => formatter.write_str("broker operation is not allowed"),
            Self::Revoked => formatter.write_str("broker handle is revoked"),
            Self::RequestTooLarge => formatter.write_str("broker request exceeds its byte limit"),
            Self::QuotaExceeded => formatter.write_str("broker operation quota is exhausted"),
            Self::ConnectorFailed => formatter.write_str("broker connector failed"),
            Self::CredentialExposure => {
                formatter.write_str("broker connector response exposed credential material")
            }
            Self::ResponseTooLarge => formatter.write_str("broker response exceeds its byte limit"),
            Self::UsageOverrun => formatter.write_str("broker connector exceeded reserved usage"),
            Self::ReceiptFailed => formatter.write_str("broker receipt could not be made durable"),
        }
    }
}

impl std::error::Error for BrokerError {}

pub trait BrokerClient {
    fn handle(&self) -> &BrokerHandle;

    fn call(
        &self,
        operation: &str,
        request: &[u8],
        reserved_usage: u64,
    ) -> Result<BrokerResponse, BrokerError>;
}

#[derive(Default)]
struct OperationState {
    calls: u32,
    in_flight_usage: u64,
    charged_usage: u64,
    observed_usage: u64,
}

struct State {
    revoked: bool,
    next_ordinal: u32,
    operations: BTreeMap<String, OperationState>,
}

struct ReceiptDetails<'a> {
    outcome: BrokerOperationOutcomeV1,
    failure_reason: Option<BrokerFailureReasonV1>,
    response: Option<&'a [u8]>,
    charged_usage: u64,
}

pub struct Broker<'a> {
    handle: BrokerHandle,
    lease: BrokerLeaseV1,
    policies: BTreeMap<String, BrokerOperationPolicyV1>,
    credential: Credential,
    authority: &'a dyn LeaseAuthority,
    connector: &'a dyn Connector,
    receipts: &'a dyn ReceiptSink,
    /// Completion receipts are globally ordinal, so authorized calls complete in that same
    /// order. Replay is therefore independent of connector timing.
    call_gate: Mutex<()>,
    state: Mutex<State>,
}

impl<'a> Broker<'a> {
    pub fn issue(
        lease: BrokerLeaseV1,
        policies: Vec<BrokerOperationPolicyV1>,
        credential: Credential,
        authority: &'a dyn LeaseAuthority,
        connector: &'a dyn Connector,
        receipts: &'a dyn ReceiptSink,
    ) -> Result<Self, BrokerError> {
        lease
            .validate()
            .map_err(|_| BrokerError::InvalidPolicy("lease is invalid"))?;
        if policies.is_empty() {
            return Err(BrokerError::InvalidPolicy("no operations are allowed"));
        }
        review_core::broker_authority_usage(&policies)
            .map_err(|_| BrokerError::InvalidPolicy("aggregate usage is too large"))?;
        let mut by_name = BTreeMap::new();
        for policy in policies {
            policy
                .validate()
                .map_err(|_| BrokerError::InvalidPolicy("operation is invalid"))?;
            if by_name.insert(policy.name.clone(), policy).is_some() {
                return Err(BrokerError::InvalidPolicy("operation names are duplicated"));
            }
        }
        let handle = BrokerHandle(handle_id(&lease, by_name.values()));
        Ok(Self {
            handle,
            lease,
            credential,
            authority,
            connector,
            receipts,
            call_gate: Mutex::new(()),
            state: Mutex::new(State {
                revoked: false,
                next_ordinal: 1,
                operations: by_name
                    .keys()
                    .cloned()
                    .map(|name| (name, OperationState::default()))
                    .collect(),
            }),
            policies: by_name,
        })
    }

    pub fn revoke(&self) {
        self.revoke_state();
        let _call = self.call_gate.lock().expect("broker call gate");
    }

    /// Machine-side observability for revocation coordination. Workers receive only
    /// [`BrokerClient`], which deliberately does not expose this state.
    pub fn is_revoked(&self) -> bool {
        self.state.lock().expect("broker state").revoked
    }

    fn revoke_state(&self) {
        self.state.lock().expect("broker state").revoked = true;
    }

    /// Actual normalized connector usage observed by this Attempt, including post-call failures.
    pub fn charged_usage(&self) -> u64 {
        self.state
            .lock()
            .expect("broker state")
            .operations
            .values()
            .map(|operation| operation.observed_usage)
            .fold(0, u64::saturating_add)
    }

    fn next_ordinal(&self) -> Result<u32, BrokerError> {
        let mut state = self.state.lock().expect("broker state");
        let ordinal = state.next_ordinal;
        state.next_ordinal = ordinal.checked_add(1).ok_or(BrokerError::QuotaExceeded)?;
        Ok(ordinal)
    }

    fn receipt(
        &self,
        policy: &BrokerOperationPolicyV1,
        ordinal: u32,
        request: &[u8],
        reserved_usage: u64,
        details: ReceiptDetails<'_>,
    ) -> BrokerOperationReceiptV1 {
        BrokerOperationReceiptV1 {
            handle_id: self.handle.0.clone(),
            node: self.lease.node_id.clone(),
            attempt_id: self.lease.attempt_id.clone(),
            lease_epoch: self.lease.lease_epoch,
            operation: policy.name.clone(),
            destination: policy.destination.clone(),
            method: policy.method.clone(),
            ordinal,
            outcome: details.outcome,
            failure_reason: details.failure_reason,
            request_digest: digest(request),
            response_digest: details.response.map(digest),
            request_bytes: request.len() as u64,
            response_bytes: details.response.map_or(0, |body| body.len() as u64),
            reserved_usage,
            charged_usage: details.charged_usage,
        }
    }

    fn record(
        &self,
        receipt: BrokerOperationReceiptV1,
        error: BrokerError,
    ) -> Result<BrokerResponse, BrokerError> {
        self.persist(&receipt)?;
        Err(error)
    }

    fn persist(&self, receipt: &BrokerOperationReceiptV1) -> Result<(), BrokerError> {
        if receipt.validate().is_err() {
            self.revoke_state();
            return Err(BrokerError::ReceiptFailed);
        }
        match self.receipts.record(receipt) {
            Ok(()) => Ok(()),
            Err(ReceiptError::AuthorityRevoked) => {
                let mut revoked = receipt.clone();
                revoked.outcome = BrokerOperationOutcomeV1::Revoked;
                revoked.failure_reason = Some(BrokerFailureReasonV1::AuthorityRevoked);
                if revoked.validate().is_err() || self.receipts.record(&revoked).is_err() {
                    self.revoke_state();
                    return Err(BrokerError::ReceiptFailed);
                }
                self.revoke_state();
                Err(BrokerError::Revoked)
            }
            Err(ReceiptError::Unavailable) => {
                self.revoke_state();
                Err(BrokerError::ReceiptFailed)
            }
        }
    }
}

impl BrokerClient for Broker<'_> {
    fn handle(&self) -> &BrokerHandle {
        &self.handle
    }

    fn call(
        &self,
        operation: &str,
        request: &[u8],
        reserved_usage: u64,
    ) -> Result<BrokerResponse, BrokerError> {
        let policy = self
            .policies
            .get(operation)
            .ok_or(BrokerError::OperationNotAllowed)?;
        let _call = self.call_gate.lock().expect("broker call gate");
        if self.is_revoked() {
            return Err(BrokerError::Revoked);
        }
        let ordinal = self.next_ordinal()?;
        if self
            .authority
            .ensure_current(&self.lease, &self.handle)
            .is_err()
        {
            self.revoke_state();
            let receipt = self.receipt(
                policy,
                ordinal,
                request,
                reserved_usage,
                ReceiptDetails {
                    outcome: BrokerOperationOutcomeV1::Revoked,
                    failure_reason: Some(BrokerFailureReasonV1::AuthorityRevoked),
                    response: None,
                    charged_usage: 0,
                },
            );
            return self.record(receipt, BrokerError::Revoked);
        }
        if request.len() as u64 > policy.max_request_bytes {
            self.revoke_state();
            let receipt = self.receipt(
                policy,
                ordinal,
                request,
                reserved_usage,
                ReceiptDetails {
                    outcome: BrokerOperationOutcomeV1::Refused,
                    failure_reason: Some(BrokerFailureReasonV1::RequestTooLarge),
                    response: None,
                    charged_usage: 0,
                },
            );
            return self.record(receipt, BrokerError::RequestTooLarge);
        }
        let reservation_admitted = {
            let mut state = self.state.lock().expect("broker state");
            let operation = state
                .operations
                .get_mut(operation)
                .expect("policy and state have identical operations");
            let projected = operation
                .charged_usage
                .checked_add(operation.in_flight_usage)
                .and_then(|value| value.checked_add(reserved_usage));
            if operation.calls >= policy.max_calls
                || reserved_usage == 0
                || projected.is_none_or(|value| value > policy.max_usage)
            {
                false
            } else {
                operation.calls += 1;
                operation.in_flight_usage += reserved_usage;
                true
            }
        };
        if !reservation_admitted {
            self.revoke_state();
            let receipt = self.receipt(
                policy,
                ordinal,
                request,
                reserved_usage,
                ReceiptDetails {
                    outcome: BrokerOperationOutcomeV1::Refused,
                    failure_reason: Some(BrokerFailureReasonV1::QuotaExceeded),
                    response: None,
                    charged_usage: 0,
                },
            );
            return self.record(receipt, BrokerError::QuotaExceeded);
        }

        let invoked = catch_unwind(AssertUnwindSafe(|| {
            self.connector.execute(ConnectorCall {
                destination: &policy.destination,
                method: &policy.method,
                request,
                credential: self.credential.expose(),
            })
        }));
        let (response, charged_usage, connector_failed) = match invoked {
            Ok(Ok(reply)) => {
                let (response, charged_usage) = reply.into_parts();
                (Some(response), charged_usage.min(JSON_SAFE_INTEGER), false)
            }
            Ok(Err(_)) | Err(_) => (None, reserved_usage, true),
        };
        {
            let mut state = self.state.lock().expect("broker state");
            let operation = state
                .operations
                .get_mut(operation)
                .expect("policy and state have identical operations");
            operation.in_flight_usage -= reserved_usage;
            operation.charged_usage = operation
                .charged_usage
                .saturating_add(charged_usage.min(reserved_usage));
            operation.observed_usage = operation.observed_usage.saturating_add(charged_usage);
        }

        if connector_failed {
            let receipt = self.receipt(
                policy,
                ordinal,
                request,
                reserved_usage,
                ReceiptDetails {
                    outcome: BrokerOperationOutcomeV1::Failed,
                    failure_reason: Some(BrokerFailureReasonV1::ConnectorFailed),
                    response: None,
                    charged_usage,
                },
            );
            return self.record(receipt, BrokerError::ConnectorFailed);
        }
        let response = response.expect("successful connector returned a body");
        let credential_exposed = self.credential.appears_in(&response);
        if charged_usage > reserved_usage {
            self.revoke_state();
            let receipt = self.receipt(
                policy,
                ordinal,
                request,
                reserved_usage,
                ReceiptDetails {
                    outcome: BrokerOperationOutcomeV1::Failed,
                    failure_reason: Some(BrokerFailureReasonV1::UsageOverrun),
                    response: (!credential_exposed).then_some(response.as_slice()),
                    charged_usage,
                },
            );
            return self.record(receipt, BrokerError::UsageOverrun);
        }
        if credential_exposed {
            self.revoke_state();
            let receipt = self.receipt(
                policy,
                ordinal,
                request,
                reserved_usage,
                ReceiptDetails {
                    outcome: BrokerOperationOutcomeV1::Failed,
                    failure_reason: Some(BrokerFailureReasonV1::CredentialExposure),
                    response: None,
                    charged_usage,
                },
            );
            return self.record(receipt, BrokerError::CredentialExposure);
        }
        if self.is_revoked()
            || self
                .authority
                .ensure_current(&self.lease, &self.handle)
                .is_err()
        {
            self.revoke_state();
            let receipt = self.receipt(
                policy,
                ordinal,
                request,
                reserved_usage,
                ReceiptDetails {
                    outcome: BrokerOperationOutcomeV1::Revoked,
                    failure_reason: Some(BrokerFailureReasonV1::AuthorityRevoked),
                    response: Some(&response),
                    charged_usage,
                },
            );
            return self.record(receipt, BrokerError::Revoked);
        }
        if response.len() as u64 > policy.max_response_bytes {
            let receipt = self.receipt(
                policy,
                ordinal,
                request,
                reserved_usage,
                ReceiptDetails {
                    outcome: BrokerOperationOutcomeV1::Failed,
                    failure_reason: Some(BrokerFailureReasonV1::ResponseTooLarge),
                    response: Some(&response),
                    charged_usage,
                },
            );
            return self.record(receipt, BrokerError::ResponseTooLarge);
        }
        let receipt = self.receipt(
            policy,
            ordinal,
            request,
            reserved_usage,
            ReceiptDetails {
                outcome: BrokerOperationOutcomeV1::Succeeded,
                failure_reason: None,
                response: Some(&response),
                charged_usage,
            },
        );
        self.persist(&receipt)?;
        Ok(BrokerResponse {
            body: response,
            receipt,
        })
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn credential_representations(bytes: &[u8]) -> Result<Vec<Vec<u8>>, BrokerError> {
    let mut representations = vec![
        bytes.to_vec(),
        STANDARD.encode(bytes).into_bytes(),
        STANDARD_NO_PAD.encode(bytes).into_bytes(),
        URL_SAFE.encode(bytes).into_bytes(),
        URL_SAFE_NO_PAD.encode(bytes).into_bytes(),
    ];
    representations.sort();
    representations.dedup();
    Ok(representations)
}

fn encoded_hex_appears(response: &[u8], credential: &[u8]) -> bool {
    let Some(encoded_len) = credential.len().checked_mul(2) else {
        return false;
    };
    response.windows(encoded_len).any(|window| {
        window
            .chunks_exact(2)
            .zip(credential)
            .all(|(encoded, expected)| {
                hex_value(encoded[0]) == Some(expected >> 4)
                    && hex_value(encoded[1]) == Some(expected & 0x0f)
            })
    })
}

fn percent_encoded_appears(response: &[u8], credential: &[u8]) -> bool {
    // Decode the whole candidate stream so escaped and literal bytes may be interleaved. Keep
    // malformed escapes verbatim and continue scanning: one bad `%` must not hide a later valid
    // representation. Repeating until stable also catches a percent-escaped `%` without imposing
    // an arbitrary nesting limit; every successful pass strictly shortens the buffer.
    let mut decoded = response.to_vec();
    loop {
        let mut next = Vec::with_capacity(decoded.len());
        let mut changed = false;
        let mut index = 0;
        while index < decoded.len() {
            if decoded[index] == b'%' && index + 2 < decoded.len() {
                if let (Some(high), Some(low)) =
                    (hex_value(decoded[index + 1]), hex_value(decoded[index + 2]))
                {
                    next.push((high << 4) | low);
                    index += 3;
                    changed = true;
                    continue;
                }
            }
            next.push(decoded[index]);
            index += 1;
        }
        if next
            .windows(credential.len())
            .any(|window| window == credential)
        {
            return true;
        }
        if !changed {
            return false;
        }
        decoded = next;
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn handle_id<'a>(
    lease: &BrokerLeaseV1,
    policies: impl Iterator<Item = &'a BrokerOperationPolicyV1>,
) -> String {
    let mut hasher = Sha256::new();
    for value in [
        lease.campaign_id.as_bytes(),
        lease.round_event_id.as_bytes(),
        lease.node_id.as_bytes(),
        lease.attempt_id.as_bytes(),
        &lease.lease_epoch.to_be_bytes(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value);
    }
    for policy in policies {
        for value in [
            policy.name.as_bytes(),
            policy.destination.as_bytes(),
            policy.method.as_bytes(),
            &policy.max_request_bytes.to_be_bytes(),
            &policy.max_response_bytes.to_be_bytes(),
            &policy.max_calls.to_be_bytes(),
            &policy.max_usage.to_be_bytes(),
        ] {
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value);
        }
    }
    format!("{:x}", hasher.finalize())[..26].to_string()
}
