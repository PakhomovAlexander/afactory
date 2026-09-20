//! M6.3 composition: project authority, machine-local capability material, and durable receipts.

mod support;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use review_broker::{
    BrokerClient, BrokerError, Connector, ConnectorCall, ConnectorError, ConnectorReply,
};
use review_config::{ConfigError, Definition};
use review_core::{
    BrokerCredentialModeV1, BrokerFailureReasonV1, BrokerOperationOutcomeV1,
    BrokerOperationPolicyV1, BrokerOperationReceiptV1, EventType, LegacyStageOutput,
    ReviewerExecutionBindingV1,
    event::{AttemptDispatchedPayloadV1, AttemptFencedPayloadV1},
};
use review_pipeline::{BrokerProvider, Kernel};
use review_runner::{
    ContextManifest, ReceiptedReviewerReturn, ReviewerAdapter, ReviewerInputs, ReviewerReturn,
    RunnerError, TokenUsage,
};
use review_source_git::Manifest;
use review_store::{Cas, EventStore, NewEvent};

const BROKERED_PIPELINE: &str = r#"
version = 4

[subject]
kind = "whole-tree"

[gate]
provider = "trusted_local"
required_isolation = "none"
mode = "ephemeral-write"

[[checks]]
name = "admission"
program = "/bin/sh"
args = [{ value = "-c" }, { value = "exit 0" }]

[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]

[[nodes]]
id = "reviewer"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }
execution = { credential_mode = "brokered", operations = [{ name = "model_inference", destination = "provider.test", method = "responses.create", max_request_bytes = 1024, max_response_bytes = 1024, max_calls = 1, max_usage = 100 }] }

[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["findings"]

[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "reviewer", port = "gate" }

[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "ledger", port = "reports" }
"#;

const CREDENTIAL: &[u8] = b"machine-local-secret";

fn clean_output() -> LegacyStageOutput {
    serde_json::from_str(
        r#"{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]}"#,
    )
    .unwrap()
}

struct TestConnector {
    calls: Arc<AtomicUsize>,
}

impl Connector for TestConnector {
    fn execute(&self, call: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(call.destination, "provider.test");
        assert_eq!(call.method, "responses.create");
        assert_eq!(call.request, b"bounded model request");
        assert_eq!(call.credential(), CREDENTIAL);
        Ok(ConnectorReply::credential_free(b"model response", 7))
    }
}

struct BrokeredReviewer {
    handle: Arc<Mutex<Option<String>>>,
    reported_cost: u64,
}

impl ReviewerAdapter for BrokeredReviewer {
    fn credential_mode(&self) -> BrokerCredentialModeV1 {
        BrokerCredentialModeV1::Brokered
    }

    fn invoke(
        &self,
        _cas: &Cas,
        _root: &Path,
        _inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        Err(RunnerError::Refused(
            "brokered reviewer must receive a Broker Handle".into(),
        ))
    }

    fn invoke_with_broker(
        &self,
        cas: &Cas,
        _root: &Path,
        inputs: &ReviewerInputs,
        broker: Option<&dyn BrokerClient>,
    ) -> Result<ReceiptedReviewerReturn, RunnerError> {
        assert!(
            !serde_json::to_string(inputs)
                .unwrap()
                .contains("machine-local-secret")
        );
        let broker = broker.expect("admitted Broker Handle");
        *self.handle.lock().unwrap() = Some(broker.handle().as_str().to_string());
        let response = broker
            .call("model_inference", b"bounded model request", 10)
            .map_err(|error| RunnerError::Refused(error.to_string()))?;
        assert_eq!(response.body, b"model response");
        assert_eq!(response.receipt.charged_usage, 7);
        assert!(matches!(
            broker.call("model_inference", b"second request", 10),
            Err(BrokerError::QuotaExceeded)
        ));
        assert!(matches!(
            broker.call("arbitrary_egress", b"request", 10),
            Err(BrokerError::OperationNotAllowed)
        ));
        Ok(ReceiptedReviewerReturn {
            returned: ReviewerReturn {
                output: clean_output(),
                proposal: Ok(None),
                notes: Ok(None),
                cost_tokens: self.reported_cost,
                raw_artifact: cas.put(b"redacted model answer").unwrap(),
            },
            usage: TokenUsage::charge_only(self.reported_cost),
            context_manifest: ContextManifest::default(),
        })
    }
}

#[test]
fn brokered_reviewer_gets_only_a_handle_and_leaves_durable_secret_free_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let loaded = Definition::from_toml(BROKERED_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let manifest = Manifest::new(vec![]).unwrap();
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &manifest,
        BROKERED_PIPELINE,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = Arc::new(Mutex::new(None));
    let kernel = Kernel::from_loaded(&cas, &mut store, "run", manifest, &loaded, authority)
        .unwrap()
        .with_checks(loaded.checks().to_vec())
        .with_adapter(
            "reviewer",
            Box::new(BrokeredReviewer {
                handle: handle.clone(),
                reported_cost: 7,
            }),
        )
        .with_broker_provider(
            "reviewer",
            BrokerProvider::new(
                CREDENTIAL,
                Arc::new(TestConnector {
                    calls: calls.clone(),
                }),
            )
            .unwrap(),
        );

    let report = loaded.run(&kernel).unwrap();
    assert!(
        report.complete(),
        "{:?}; gate: {:?}",
        report.outcomes,
        kernel.gate_decision("gate")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(kernel);

    let events = store.replay("run").unwrap();
    let binding: ReviewerExecutionBindingV1 = serde_json::from_value(
        events
            .iter()
            .find(|event| event.event_type == EventType::ReviewerExecutionBoundV1)
            .expect("ReviewerExecutionBound@1")
            .payload
            .clone(),
    )
    .unwrap();
    assert_eq!(binding.credential_mode, BrokerCredentialModeV1::Brokered);
    assert_eq!(binding.broker_handle, *handle.lock().unwrap());
    binding.validate().unwrap();

    let receipts = events
        .iter()
        .filter(|event| event.event_type == EventType::BrokerOperationCompletedV1)
        .map(|event| {
            serde_json::from_value::<BrokerOperationReceiptV1>(event.payload.clone()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 2);
    let receipt = &receipts[0];
    assert_eq!(receipt.handle_id, binding.broker_handle.unwrap());
    assert_eq!(receipt.operation, "model_inference");
    assert_eq!(receipt.charged_usage, 7);
    receipt.validate().unwrap();
    assert_eq!(receipts[1].ordinal, 2);
    assert_eq!(
        receipts[1].failure_reason,
        Some(review_core::BrokerFailureReasonV1::QuotaExceeded)
    );
    receipts[1].validate().unwrap();

    let durable = serde_json::to_string(&events).unwrap();
    assert!(!durable.contains("machine-local-secret"));
    assert!(!durable.contains("bounded model request"));
    assert!(!durable.contains("model response"));
}

#[test]
fn brokered_adapter_cannot_under_report_attempt_usage() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let loaded = Definition::from_toml(BROKERED_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let manifest = Manifest::new(vec![]).unwrap();
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &manifest,
        BROKERED_PIPELINE,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::from_loaded(&cas, &mut store, "run", manifest, &loaded, authority)
        .unwrap()
        .with_checks(loaded.checks().to_vec())
        .with_adapter(
            "reviewer",
            Box::new(BrokeredReviewer {
                handle: Arc::new(Mutex::new(None)),
                reported_cost: 0,
            }),
        )
        .with_broker_provider(
            "reviewer",
            BrokerProvider::new(
                CREDENTIAL,
                Arc::new(TestConnector {
                    calls: calls.clone(),
                }),
            )
            .unwrap(),
        );

    let report = loaded.run(&kernel).unwrap();
    assert!(!report.complete());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(kernel);
    let events = store.replay("run").unwrap();
    let failed = events
        .iter()
        .find(|event| event.event_type == EventType::AttemptFailedV1)
        .expect("AttemptFailed@1");
    assert_eq!(failed.payload["charged"].as_u64(), Some(7));
    assert!(
        failed.payload["error"]
            .as_str()
            .unwrap()
            .contains("usage mismatch")
    );
    assert!(
        events
            .iter()
            .all(|event| event.event_type != EventType::AttemptAdmittedV1)
    );
}

#[test]
fn recovery_fences_an_unbudgeted_brokered_attempt_at_its_durable_authority_bound() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let loaded = Definition::from_toml(BROKERED_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let manifest = Manifest::new(vec![]).unwrap();
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &manifest,
        BROKERED_PIPELINE,
    );
    let attempt = "a".repeat(26);
    let handle = "b".repeat(26);
    let operation = BrokerOperationPolicyV1 {
        name: "model_inference".into(),
        destination: "provider.test".into(),
        method: "responses.create".into(),
        max_request_bytes: 1024,
        max_response_bytes: 1024,
        max_calls: 1,
        max_usage: 100,
    };
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::AttemptDispatchedV1,
                serde_json::to_value(AttemptDispatchedPayloadV1 {
                    reserved: None,
                    prior_findings: None,
                })
                .unwrap(),
            )
            .node("reviewer")
            .attempt(&attempt)
            .caused_by(authority.round_event_id()),
        )
        .unwrap();
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::ReviewerExecutionBoundV1,
                serde_json::to_value(ReviewerExecutionBindingV1 {
                    node: "reviewer".into(),
                    attempt_id: attempt.clone(),
                    lease_epoch: 1,
                    credential_mode: BrokerCredentialModeV1::Brokered,
                    auto_apply: false,
                    broker_handle: Some(handle.clone()),
                    operations: vec![operation],
                    admitted: true,
                })
                .unwrap(),
            )
            .node("reviewer")
            .attempt(&attempt)
            .caused_by(authority.round_event_id()),
        )
        .unwrap();
    // Reconstructing the Kernel is the crash-recovery boundary. It must durably fence the
    // outstanding attempt with enough charge to cover both observed and still in-flight work.
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        manifest.clone(),
        &loaded,
        authority.clone(),
    )
    .unwrap();
    drop(kernel);
    let events = store.replay("run").unwrap();
    let fence: AttemptFencedPayloadV1 = serde_json::from_value(
        events
            .iter()
            .find(|event| {
                event.event_type == EventType::AttemptFencedV1
                    && event.attempt_id.as_deref() == Some(attempt.as_str())
            })
            .expect("recovery fence")
            .payload
            .clone(),
    )
    .unwrap();
    assert_eq!(fence.charged, Some(100));

    // The original process completes after the recovery fence and observes a provider overrun.
    // The redacted revoked receipt remains durable and raises committed spend above the fence.
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::BrokerOperationCompletedV1,
                serde_json::to_value(BrokerOperationReceiptV1 {
                    handle_id: handle,
                    node: "reviewer".into(),
                    attempt_id: attempt.clone(),
                    lease_epoch: 1,
                    operation: "model_inference".into(),
                    destination: "provider.test".into(),
                    method: "responses.create".into(),
                    ordinal: 1,
                    outcome: BrokerOperationOutcomeV1::Revoked,
                    failure_reason: Some(BrokerFailureReasonV1::AuthorityRevoked),
                    request_digest: format!("sha256:{}", "c".repeat(64)),
                    response_digest: None,
                    request_bytes: 7,
                    response_bytes: 0,
                    reserved_usage: 100,
                    charged_usage: 101,
                })
                .unwrap(),
            )
            .node("reviewer")
            .attempt(&attempt)
            .caused_by(authority.round_event_id()),
        )
        .unwrap();
    assert_eq!(
        store
            .round_committed_tokens("run", authority.round_event_id())
            .unwrap(),
        101
    );

    // A third process replays the excess charge cleanly and does not fence twice.
    drop(Kernel::from_loaded(&cas, &mut store, "run", manifest, &loaded, authority).unwrap());
    assert_eq!(
        store
            .replay("run")
            .unwrap()
            .iter()
            .filter(|event| event.event_type == EventType::AttemptFencedV1)
            .count(),
        1
    );
}

struct FailingConnector {
    calls: Arc<AtomicUsize>,
}

impl Connector for FailingConnector {
    fn execute(&self, _: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(ConnectorError)
    }
}

#[test]
fn a_receipted_connector_failure_is_charged_not_released() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let loaded = Definition::from_toml(BROKERED_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let manifest = Manifest::new(vec![]).unwrap();
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &manifest,
        BROKERED_PIPELINE,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::from_loaded(&cas, &mut store, "run", manifest, &loaded, authority)
        .unwrap()
        .with_checks(loaded.checks().to_vec())
        .with_adapter(
            "reviewer",
            Box::new(BrokeredReviewer {
                handle: Arc::new(Mutex::new(None)),
                reported_cost: 0,
            }),
        )
        .with_broker_provider(
            "reviewer",
            BrokerProvider::new(
                CREDENTIAL,
                Arc::new(FailingConnector {
                    calls: calls.clone(),
                }),
            )
            .unwrap(),
        );

    let report = loaded.run(&kernel).unwrap();
    assert!(!report.complete());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(kernel);
    let events = store.replay("run").unwrap();
    let failed = events
        .iter()
        .find(|event| event.event_type == EventType::AttemptFailedV1)
        .expect("AttemptFailed@1");
    assert_eq!(failed.payload["charged"].as_u64(), Some(10));
    assert!(
        events
            .iter()
            .all(|event| event.event_type != EventType::AttemptReleasedV1)
    );
    let receipt: BrokerOperationReceiptV1 = serde_json::from_value(
        events
            .iter()
            .find(|event| event.event_type == EventType::BrokerOperationCompletedV1)
            .expect("BrokerOperationCompleted@1")
            .payload
            .clone(),
    )
    .unwrap();
    assert_eq!(
        receipt.failure_reason,
        Some(review_core::BrokerFailureReasonV1::ConnectorFailed)
    );
}

struct PanickingConnector {
    side_effects: Arc<AtomicUsize>,
}

impl Connector for PanickingConnector {
    fn execute(&self, _: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        self.side_effects.fetch_add(1, Ordering::SeqCst);
        panic!("connector panicked after the external side effect")
    }
}

#[test]
fn an_unbudgeted_attempt_charges_and_receipts_a_connector_panic() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let loaded = Definition::from_toml(BROKERED_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let manifest = Manifest::new(vec![]).unwrap();
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &manifest,
        BROKERED_PIPELINE,
    );
    let side_effects = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::from_loaded(&cas, &mut store, "run", manifest, &loaded, authority)
        .unwrap()
        .with_checks(loaded.checks().to_vec())
        .with_adapter(
            "reviewer",
            Box::new(BrokeredReviewer {
                handle: Arc::new(Mutex::new(None)),
                reported_cost: 0,
            }),
        )
        .with_broker_provider(
            "reviewer",
            BrokerProvider::new(
                CREDENTIAL,
                Arc::new(PanickingConnector {
                    side_effects: side_effects.clone(),
                }),
            )
            .unwrap(),
        );

    let report = loaded.run(&kernel).unwrap();
    assert!(!report.complete());
    assert_eq!(side_effects.load(Ordering::SeqCst), 1);
    drop(kernel);
    let events = store.replay("run").unwrap();
    let receipt: BrokerOperationReceiptV1 = serde_json::from_value(
        events
            .iter()
            .find(|event| event.event_type == EventType::BrokerOperationCompletedV1)
            .expect("panic receipt")
            .payload
            .clone(),
    )
    .unwrap();
    assert_eq!(
        receipt.failure_reason,
        Some(review_core::BrokerFailureReasonV1::ConnectorFailed)
    );
    assert_eq!(receipt.charged_usage, 10);
    let failed = events
        .iter()
        .find(|event| event.event_type == EventType::AttemptFailedV1)
        .expect("AttemptFailed@1");
    assert_eq!(failed.payload["charged"].as_u64(), Some(10));
    assert!(
        events
            .iter()
            .all(|event| event.event_type != EventType::AttemptReleasedV1)
    );
}

#[test]
fn adapter_mode_mismatch_is_refused_before_reviewer_dispatch() {
    let credential_free = BROKERED_PIPELINE.replace(
        "credential_mode = \"brokered\", operations = [{ name = \"model_inference\", destination = \"provider.test\", method = \"responses.create\", max_request_bytes = 1024, max_response_bytes = 1024, max_calls = 1, max_usage = 100 }]",
        "credential_mode = \"credential_free\"",
    );
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let loaded = Definition::from_toml(&credential_free)
        .unwrap()
        .load()
        .unwrap();
    let manifest = Manifest::new(vec![]).unwrap();
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &manifest,
        &credential_free,
    );
    let kernel = Kernel::from_loaded(&cas, &mut store, "run", manifest, &loaded, authority)
        .unwrap()
        .with_adapter(
            "reviewer",
            Box::new(BrokeredReviewer {
                handle: Arc::new(Mutex::new(None)),
                reported_cost: 7,
            }),
        );

    let error = loaded.run(&kernel).unwrap_err();
    assert!(
        matches!(error, ConfigError::Binding(ref message) if message.contains("CredentialFree") && message.contains("Brokered")),
        "{error}"
    );
    drop(kernel);
    assert!(store.replay("run").unwrap().iter().all(|event| {
        !matches!(
            event.event_type,
            EventType::AttemptDispatchedV1 | EventType::ReviewerExecutionBoundV1
        )
    }));
}
