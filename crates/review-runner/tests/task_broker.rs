//! Task transport accepts only an explicitly supported opaque Broker capability. These tests
//! use an in-process connector; no Provider, subprocess, or Task scheduler is involved.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use review_broker::{
    AuthorityError, BrokerHandle, Connector, ConnectorCall, ConnectorError, ConnectorReply,
    Credential, ExactBroker, ExactBrokerClient, ExactReceiptSink, LeaseAuthority, ReceiptError,
};
use review_core::task::execution::TaskInvocationV1;
use review_core::task::feedback::TaskFeedbackCodeV1;
use review_core::{
    BrokerCredentialModeV1, BrokerLeaseV1, BrokerOperationPolicyV1, BrokerOperationReceiptV2,
};
use review_runner::TokenUsage;
use review_runner::task::{
    ModelWorkerReturn, WorkerContract, WorkerModelAdapter, invoke_model, invoke_model_controlled,
    invoke_model_with_broker,
};
use review_store::Cas;
use serde_json::json;

const VALID_REPLY: &[u8] =
    br#"{"schema":"af.worker-reply/1","outputs":{"document":[{"text":"Captured answer"}]}}"#;
const RAW: &[u8] = b"raw adapter evidence";
const TIMEOUT: Duration = Duration::from_millis(1234);

struct Fixture {
    directory: tempfile::TempDir,
    cas: Cas,
    contract: WorkerContract,
    context_id: String,
    input: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let contract = WorkerContract::capture(
            &cas,
            json!({"type":"object","additionalProperties":false}),
            BTreeMap::from([(
                "document".into(),
                json!({"type":"object","additionalProperties":false,"required":["text"],
                    "properties":{"text":{"type":"string"}}}),
            )]),
        )
        .unwrap();
        let invocation = TaskInvocationV1 {
            plan_id: cas.put(b"exact captured plan").unwrap(),
            node: "root.worker".into(),
            inputs: BTreeMap::new(),
        };
        let context_id = contract
            .prepare(&cas, &invocation, &[], "Only declared context")
            .unwrap();
        let (_, input) = contract.read_context(&cas, &context_id).unwrap();
        Self {
            directory,
            cas,
            contract,
            context_id,
            input,
        }
    }
}

struct NativeModel<'a> {
    fixture: &'a Fixture,
    calls: AtomicUsize,
    failed: bool,
}

impl WorkerModelAdapter for NativeModel<'_> {
    fn provider_kind(&self) -> &'static str {
        "fixture"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some(("fixture-1".into(), "high".into()))
    }
    fn invoke(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        writable: bool,
    ) -> ModelWorkerReturn {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(std::ptr::eq(cas, &self.fixture.cas));
        assert_eq!(workdir, self.fixture.directory.path());
        assert_eq!(input, self.fixture.input);
        assert_eq!(timeout, TIMEOUT);
        assert!(writable);
        ModelWorkerReturn {
            usage_observation: None,
            message: if self.failed {
                Err("reported Provider failure".into())
            } else {
                Ok(VALID_REPLY.to_vec())
            },
            usage: Some(TokenUsage::charge_only(u64::MAX).into()),
            raw_artifact_ids: vec![cas.put(RAW).unwrap()],
        }
    }
}

struct LocalBroker {
    response: Vec<u8>,
    calls: AtomicUsize,
    receipts: Mutex<Vec<BrokerOperationReceiptV2>>,
}

impl LocalBroker {
    fn new(response: &[u8]) -> Self {
        Self {
            response: response.to_vec(),
            calls: AtomicUsize::new(0),
            receipts: Mutex::new(vec![]),
        }
    }
    fn issue(&self) -> ExactBroker<'_> {
        ExactBroker::issue(
            BrokerLeaseV1 {
                campaign_id: "fixture-task".into(),
                round_event_id: "a".repeat(26),
                node_id: "reviewer".into(),
                attempt_id: "b".repeat(26),
                lease_epoch: 1,
            },
            vec![BrokerOperationPolicyV1 {
                name: "inference".into(),
                destination: "fixture.test".into(),
                method: "respond".into(),
                max_request_bytes: 4096,
                max_response_bytes: 4096,
                max_calls: 1,
                max_usage: 10,
            }],
            Credential::new(b"fixture-secret-material".to_vec()).unwrap(),
            self,
            self,
            self,
        )
        .unwrap()
    }
}

impl LeaseAuthority for LocalBroker {
    fn ensure_current(&self, _: &BrokerLeaseV1, _: &BrokerHandle) -> Result<(), AuthorityError> {
        Ok(())
    }
}
impl Connector for LocalBroker {
    fn execute(&self, call: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(call.destination, "fixture.test");
        assert_eq!(call.method, "respond");
        assert_eq!(call.credential(), b"fixture-secret-material");
        let request: serde_json::Value = serde_json::from_slice(call.request).unwrap();
        assert_eq!(request["instructions"], "Only declared context");
        Ok(ConnectorReply::credential_free(self.response.clone(), 7))
    }
}
impl ExactReceiptSink for LocalBroker {
    fn record(&self, receipt: &BrokerOperationReceiptV2) -> Result<(), ReceiptError> {
        self.receipts.lock().unwrap().push(receipt.clone());
        Ok(())
    }
}

#[test]
fn absent_broker_forwards_exact_invocation_and_retains_success_or_failed_evidence() {
    let fixture = Fixture::new();
    for failed in [false, true] {
        let model = NativeModel {
            fixture: &fixture,
            calls: AtomicUsize::new(0),
            failed,
        };
        assert_eq!(
            model.credential_mode(),
            BrokerCredentialModeV1::TrustedUnsafe
        );
        for legacy_entry in 0..3 {
            let result = if legacy_entry == 0 {
                invoke_model(
                    &fixture.cas,
                    fixture.directory.path(),
                    &model,
                    &fixture.contract,
                    &fixture.context_id,
                    TIMEOUT,
                    true,
                )
            } else if legacy_entry == 1 {
                invoke_model_with_broker(
                    &fixture.cas,
                    fixture.directory.path(),
                    &model,
                    &fixture.contract,
                    &fixture.context_id,
                    TIMEOUT,
                    true,
                    None,
                )
            } else {
                invoke_model_controlled(
                    &fixture.cas,
                    fixture.directory.path(),
                    &model,
                    &fixture.contract,
                    &fixture.context_id,
                    TIMEOUT,
                    true,
                    None,
                    None,
                )
            };
            assert_eq!(result.reply.is_err(), failed);
            assert_eq!(
                result.feedback_code,
                failed.then_some(TaskFeedbackCodeV1::ProviderFailure)
            );
            assert_eq!(
                result.usage.unwrap().chargeable_tokens.get(),
                u128::from(u64::MAX)
            );
            assert_eq!(result.raw_artifact_ids.len(), 1);
            assert_eq!(fixture.cas.get(&result.raw_artifact_ids[0]).unwrap(), RAW);
        }
        assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    }
}

#[test]
fn unsupported_control_is_refused_without_invoking_adapter_or_broker() {
    let fixture = Fixture::new();
    let model = NativeModel {
        fixture: &fixture,
        calls: AtomicUsize::new(0),
        failed: false,
    };
    let local = LocalBroker::new(VALID_REPLY);
    let broker = local.issue();
    for cancelled in [false, true] {
        let flag = std::sync::atomic::AtomicBool::new(cancelled);
        let result = invoke_model_controlled(
            &fixture.cas,
            fixture.directory.path(),
            &model,
            &fixture.contract,
            &fixture.context_id,
            TIMEOUT,
            true,
            Some(&broker),
            Some(&flag),
        );
        assert!(
            result
                .reply
                .unwrap_err()
                .contains("does not support controlled invocation")
        );
        assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 0);
        assert!(result.raw_artifact_ids.is_empty());
    }
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(local.calls.load(Ordering::SeqCst), 0);
    assert_eq!(broker.charged_usage(), 0);
}

#[test]
fn unsupported_broker_is_refused_before_adapter_or_connector_invocation() {
    let fixture = Fixture::new();
    let model = NativeModel {
        fixture: &fixture,
        calls: AtomicUsize::new(0),
        failed: false,
    };
    let local = LocalBroker::new(VALID_REPLY);
    let broker = local.issue();
    let result = invoke_model_with_broker(
        &fixture.cas,
        fixture.directory.path(),
        &model,
        &fixture.contract,
        &fixture.context_id,
        TIMEOUT,
        true,
        Some(&broker),
    );
    assert!(
        result
            .reply
            .unwrap_err()
            .contains("does not consume Broker Handles")
    );
    assert_eq!(
        result.feedback_code,
        Some(TaskFeedbackCodeV1::ProviderFailure)
    );
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 0);
    assert!(result.raw_artifact_ids.is_empty());
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(local.calls.load(Ordering::SeqCst), 0);
    assert!(local.receipts.lock().unwrap().is_empty());
    assert_eq!(broker.charged_usage(), 0);
}

struct BrokeredModel {
    calls: AtomicUsize,
    failed: bool,
}

impl WorkerModelAdapter for BrokeredModel {
    fn credential_mode(&self) -> BrokerCredentialModeV1 {
        BrokerCredentialModeV1::Brokered
    }
    fn provider_kind(&self) -> &'static str {
        "fixture"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some(("fixture-1".into(), "high".into()))
    }
    fn invoke(&self, _: &Cas, _: &Path, _: Vec<u8>, _: Duration, _: bool) -> ModelWorkerReturn {
        panic!("Brokered adapter must receive its opaque client")
    }
    fn invoke_with_broker(
        &self,
        cas: &Cas,
        _: &Path,
        input: Vec<u8>,
        timeout: Duration,
        writable: bool,
        broker: Option<&dyn ExactBrokerClient>,
    ) -> ModelWorkerReturn {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(timeout, TIMEOUT);
        assert!(writable);
        let response = broker
            .expect("bound Broker")
            .call("inference", &input, 10)
            .unwrap();
        let raw_artifact_ids = vec![cas.put(&response.body).unwrap()];
        ModelWorkerReturn {
            usage_observation: None,
            message: if self.failed {
                Err("framing failed after paid operation".into())
            } else {
                Ok(response.body)
            },
            // Adapter counters remain separate from the execution owner's exact Broker total.
            usage: Some(TokenUsage::charge_only(3).into()),
            raw_artifact_ids,
        }
    }
}

#[test]
fn brokered_transport_shares_context_and_output_checks_and_retains_failed_usage() {
    let fixture = Fixture::new();
    for (reply, failed, expected_feedback) in [
        (VALID_REPLY, false, None),
        (
            br#"{"schema":"af.worker-reply/1","outputs":{"document":[{"text":42}]}}"#.as_slice(),
            false,
            Some(TaskFeedbackCodeV1::InvalidOutputContract),
        ),
        (VALID_REPLY, true, Some(TaskFeedbackCodeV1::ProviderFailure)),
    ] {
        let local = LocalBroker::new(reply);
        let broker = local.issue();
        let model = BrokeredModel {
            calls: AtomicUsize::new(0),
            failed,
        };
        assert_eq!(model.credential_mode(), BrokerCredentialModeV1::Brokered);
        let wrong_context = fixture.cas.put(b"not a captured context").unwrap();
        let rejected = invoke_model_with_broker(
            &fixture.cas,
            fixture.directory.path(),
            &model,
            &fixture.contract,
            &wrong_context,
            TIMEOUT,
            true,
            Some(&broker),
        );
        assert!(rejected.reply.is_err());
        assert_eq!(
            rejected.feedback_code,
            Some(TaskFeedbackCodeV1::ContextRejected)
        );
        assert_eq!(rejected.usage.unwrap().chargeable_tokens.get(), 0);
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        assert_eq!(local.calls.load(Ordering::SeqCst), 0);
        let result = invoke_model_with_broker(
            &fixture.cas,
            fixture.directory.path(),
            &model,
            &fixture.contract,
            &fixture.context_id,
            TIMEOUT,
            true,
            Some(&broker),
        );
        assert_eq!(result.reply.is_err(), expected_feedback.is_some());
        assert_eq!(result.feedback_code, expected_feedback);
        assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 3);
        assert_eq!(result.raw_artifact_ids.len(), 1);
        assert_eq!(fixture.cas.get(&result.raw_artifact_ids[0]).unwrap(), reply);
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        assert_eq!(local.calls.load(Ordering::SeqCst), 1);
        assert_eq!(local.receipts.lock().unwrap().len(), 1);
        assert_eq!(broker.charged_usage(), 7);
    }
}
