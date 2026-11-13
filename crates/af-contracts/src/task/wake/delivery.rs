//! Local owner authority is separate from notification data. Never accept these from a Task.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeHostV1 {
    Pi,
    Codex,
    Claude,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeInstallationV1 {
    pub executable: String,
    pub executable_digest: String,
    /// Pi extension bytes, or null for Codex. Protected owner configuration, never a prompt.
    #[serde(deserialize_with = "Option::deserialize")]
    pub extension: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub extension_digest: Option<String>,
    /// Owner-selected Codex Unix socket, never a URL supplied by a Task.
    #[serde(deserialize_with = "Option::deserialize")]
    pub socket: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub socket_device: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub socket_inode: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeRouteV1 {
    pub schema: String,
    pub route_id: String,
    pub installation_id: String,
    pub capability_digest: String,
    pub task: WakeTaskV1,
    pub host: WakeHostV1,
    pub session: String,
    pub mode: String,
    pub expires_unix_ms: u64,
    pub installation: WakeInstallationV1,
}
impl WakeRouteV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.task.validate()?;
        let i = &self.installation;
        let path =
            |s: &str| s.starts_with('/') && s.len() <= 4096 && !s.chars().any(char::is_control);
        require(
            self.schema == "af/task-wake-route@1"
                && [
                    &self.route_id,
                    &self.installation_id,
                    &self.capability_digest,
                    &i.executable_digest,
                ]
                .into_iter()
                .all(|v| is_digest(v))
                && is_name(&self.session)
                && self.mode == "follow_up"
                && positive(self.expires_unix_ms)
                && path(&i.executable)
                && match self.host {
                    WakeHostV1::Pi => {
                        i.extension.as_deref().is_some_and(path)
                            && i.extension_digest.as_deref().is_some_and(is_digest)
                            && i.socket.is_none()
                            && i.socket_device.is_none()
                            && i.socket_inode.is_none()
                    }
                    WakeHostV1::Codex => {
                        i.extension.is_none()
                            && i.extension_digest.is_none()
                            && i.socket.as_deref().is_some_and(path)
                            && i.socket_device.is_some()
                            && i.socket_inode.is_some()
                    }
                    WakeHostV1::Claude => false,
                },
            "Invalid or unsupported local wake route",
        )
    }
}

/// The secret lives only in an owner-private grant file. The Store retains its digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeGrantV1 {
    pub schema: String,
    pub route_id: String,
    pub capability: String,
}
impl WakeGrantV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af/task-wake-grant@1"
                && is_digest(&self.route_id)
                && is_digest(&self.capability),
            "Invalid wake grant",
        )
    }
}

/// A receiver response contains only fixed metadata. Unknown admission is sticky.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeDeliveryV1 {
    pub schema: String,
    pub envelope: WakeEnvelopeV1,
    pub route_id: String,
    pub attempts: u64,
    pub epoch: u64,
    pub claim_until_unix_ms: u64,
    pub next_attempt_unix_ms: u64,
    pub state: WakeDeliveryStateV1,
    #[serde(deserialize_with = "Option::deserialize")]
    pub reason: Option<WakeDeliveryReasonV1>,
    pub receipts: Vec<WakeReceiptV1>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeDeliveryReasonV1 {
    RouteUnavailable,
    TransportRetryExhausted,
    NativePreflightUnavailable,
    NativePreflightExhausted,
    Expired,
    NativeAdmissionUncertain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeDeliveryStateV1 {
    Pending,
    InboxAccepted,
    AdmissionUnknown,
    Blocked,
    Expired,
}
impl WakeDeliveryV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.envelope.validate()?;
        require(
            self.schema == "af/task-wake-delivery@1"
                && is_digest(&self.route_id)
                && self.attempts <= 8
                && safe_number(self.epoch)
                && safe_number(self.claim_until_unix_ms)
                && safe_number(self.next_attempt_unix_ms)
                && self.receipts.len() <= 3,
            "Invalid bounded wake delivery",
        )?;
        for receipt in &self.receipts {
            receipt.validate()?;
            require(
                receipt.delivery_id == self.envelope.delivery_id
                    && receipt.payload_digest == self.envelope.payload_digest,
                "Foreign wake receipt",
            )?;
        }
        let stages: Vec<_> = self.receipts.iter().map(|r| &r.stage).collect();
        let inbox = matches!(
            stages.first(),
            Some(WakeReceiptStageV1::InboxAccepted { .. })
        );
        let uncertain = match stages.as_slice() {
            [
                WakeReceiptStageV1::InboxAccepted { .. },
                WakeReceiptStageV1::NativeAdmissionIntent { intent_id: a },
                WakeReceiptStageV1::AdmissionUnknown { intent_id: b },
            ] => a == b,
            _ => false,
        };
        require(
            match self.state {
                WakeDeliveryStateV1::Pending => stages.is_empty(),
                WakeDeliveryStateV1::Blocked => stages.is_empty() || (stages.len() == 1 && inbox),
                WakeDeliveryStateV1::InboxAccepted => stages.len() == 1 && inbox,
                WakeDeliveryStateV1::AdmissionUnknown => uncertain,
                WakeDeliveryStateV1::Expired => stages.is_empty() || (stages.len() == 1 && inbox),
            },
            "Wake receipt stages disagree with receiver state",
        )?;
        Ok(())
    }
}
wake_contract!(WakeRouteV1, WakeGrantV1, WakeDeliveryV1);
