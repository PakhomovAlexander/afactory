use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use review_broker::{
    AuthorityError, Broker, BrokerClient, BrokerError, BrokerHandle, Connector, ConnectorCall,
    ConnectorError, ConnectorReply, Credential, LeaseAuthority, ReceiptError, ReceiptSink,
};
use review_core::{
    BrokerFailureReasonV1, BrokerLeaseV1, BrokerOperationOutcomeV1, BrokerOperationPolicyV1,
};

fn lease() -> BrokerLeaseV1 {
    BrokerLeaseV1 {
        campaign_id: "campaign-m6-3".into(),
        round_event_id: "0123456789abcdefghijklmnop".into(),
        node_id: "correctness".into(),
        attempt_id: "abcdefghijklmnopqrstuv0123".into(),
        lease_epoch: 1,
    }
}

fn policy() -> BrokerOperationPolicyV1 {
    BrokerOperationPolicyV1 {
        name: "model_inference".into(),
        destination: "provider.openai".into(),
        method: "responses.create".into(),
        max_request_bytes: 1024,
        max_response_bytes: 1024,
        max_calls: 2,
        max_usage: 100,
    }
}

struct Authority(Arc<AtomicBool>);

impl LeaseAuthority for Authority {
    fn ensure_current(&self, _: &BrokerLeaseV1, _: &BrokerHandle) -> Result<(), AuthorityError> {
        self.0
            .load(Ordering::SeqCst)
            .then_some(())
            .ok_or(AuthorityError)
    }
}

#[derive(Default)]
struct Receipts(Mutex<Vec<review_core::BrokerOperationReceiptV1>>);

impl ReceiptSink for Receipts {
    fn record(&self, receipt: &review_core::BrokerOperationReceiptV1) -> Result<(), ReceiptError> {
        self.0.lock().unwrap().push(receipt.clone());
        Ok(())
    }
}

struct TransformingConnector {
    calls: AtomicUsize,
    remote_bytes: Mutex<Vec<u8>>,
}

impl Connector for TransformingConnector {
    fn execute(&self, call: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(call.destination, "provider.openai");
        assert_eq!(call.method, "responses.create");
        let mut remote = b"authorization=bearer ".to_vec();
        remote.extend_from_slice(call.credential());
        remote.extend_from_slice(b"\n");
        remote.extend_from_slice(call.request);
        *self.remote_bytes.lock().unwrap() = remote;
        Ok(ConnectorReply::credential_free(b"provider answer", 7))
    }
}

#[test]
fn transformed_secret_exists_only_inside_the_trusted_connector() {
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(current);
    let connector = TransformingConnector {
        calls: AtomicUsize::new(0),
        remote_bytes: Mutex::new(Vec::new()),
    };
    let receipts = Receipts::default();
    let secret = b"rt_live_secret_123";
    let broker = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(secret.to_vec()).unwrap(),
        &authority,
        &connector,
        &receipts,
    )
    .unwrap();

    let response = broker.call("model_inference", b"review this", 20).unwrap();
    assert_eq!(response.body, b"provider answer");
    assert!(
        connector
            .remote_bytes
            .lock()
            .unwrap()
            .windows(secret.len())
            .any(|window| window == secret),
        "the trusted connector performs the credential transformation"
    );
    let worker_visible = serde_json::to_vec(&response.receipt).unwrap();
    assert!(
        !worker_visible
            .windows(secret.len())
            .any(|window| window == secret)
    );
    assert!(
        !response
            .body
            .windows(secret.len())
            .any(|window| window == secret)
    );
}

struct EchoingConnector;

impl Connector for EchoingConnector {
    fn execute(&self, call: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        Ok(ConnectorReply::credential_free(call.credential(), 3))
    }
}

#[test]
fn a_connector_cannot_echo_credential_bytes_to_the_worker() {
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(current);
    let receipts = Receipts::default();
    let secret = b"rt_live_secret_123";
    let broker = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(secret.to_vec()).unwrap(),
        &authority,
        &EchoingConnector,
        &receipts,
    )
    .unwrap();

    assert!(matches!(
        broker.call("model_inference", b"review this", 20),
        Err(BrokerError::CredentialExposure)
    ));
    assert_eq!(broker.charged_usage(), 3);
    let receipt = &receipts.0.lock().unwrap()[0];
    assert_eq!(
        receipt.failure_reason,
        Some(BrokerFailureReasonV1::CredentialExposure)
    );
    assert!(receipt.response_digest.is_none());
    assert!(
        !serde_json::to_vec(receipt)
            .unwrap()
            .windows(secret.len())
            .any(|window| window == secret)
    );
}

struct OverrunningEchoConnector;

impl Connector for OverrunningEchoConnector {
    fn execute(&self, call: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        Ok(ConnectorReply::credential_free(call.credential(), 1_000))
    }
}

#[test]
fn a_credential_reflection_cannot_hide_a_usage_overrun() {
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(current);
    let receipts = Receipts::default();
    let secret = b"rt_live_secret_123";
    let broker = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(secret.to_vec()).unwrap(),
        &authority,
        &OverrunningEchoConnector,
        &receipts,
    )
    .unwrap();

    assert!(matches!(
        broker.call("model_inference", b"review this", 10),
        Err(BrokerError::UsageOverrun)
    ));
    assert_eq!(broker.charged_usage(), 1_000);
    let receipt = &receipts.0.lock().unwrap()[0];
    assert_eq!(
        receipt.failure_reason,
        Some(BrokerFailureReasonV1::UsageOverrun)
    );
    assert_eq!(receipt.charged_usage, 1_000);
    assert!(receipt.response_digest.is_none());
    assert_eq!(receipt.response_bytes, 0);
    receipt.validate().unwrap();
}

struct EncodedEcho(Vec<u8>);

impl Connector for EncodedEcho {
    fn execute(&self, _: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        Ok(ConnectorReply::credential_free(self.0.clone(), 3))
    }
}

#[test]
fn common_encoded_credential_reflections_are_withheld() {
    let secret = b"\xfb\xffsecret?";
    let hex = secret
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let percent = secret
        .iter()
        .map(|byte| format!("%{byte:02X}"))
        .collect::<String>();
    let mixed_hex = hex
        .bytes()
        .enumerate()
        .map(|(index, byte)| {
            if index % 2 == 0 {
                byte.to_ascii_uppercase()
            } else {
                byte
            }
        })
        .collect::<Vec<_>>();
    let mixed_percent = percent
        .bytes()
        .enumerate()
        .map(|(index, byte)| {
            if index % 2 == 0 {
                byte.to_ascii_lowercase()
            } else {
                byte
            }
        })
        .collect::<Vec<_>>();
    let encodings = [
        STANDARD.encode(secret).into_bytes(),
        URL_SAFE_NO_PAD.encode(secret).into_bytes(),
        hex.into_bytes(),
        percent.into_bytes(),
        mixed_hex,
        mixed_percent,
    ];

    for encoded in encodings {
        let current = Arc::new(AtomicBool::new(true));
        let authority = Authority(current);
        let receipts = Receipts::default();
        let connector = EncodedEcho(encoded);
        let broker = Broker::issue(
            lease(),
            vec![policy()],
            Credential::new(secret.to_vec()).unwrap(),
            &authority,
            &connector,
            &receipts,
        )
        .unwrap();

        assert!(matches!(
            broker.call("model_inference", b"review this", 20),
            Err(BrokerError::CredentialExposure)
        ));
        let receipt = &receipts.0.lock().unwrap()[0];
        assert_eq!(
            receipt.failure_reason,
            Some(BrokerFailureReasonV1::CredentialExposure)
        );
        assert!(receipt.response_digest.is_none());
    }
}

#[test]
fn partial_and_nested_percent_encoded_credential_reflections_are_withheld() {
    let encodings: [&[u8]; 5] = [
        b"%73ecret",
        b"s%65cret",
        b"%73%65c%72%65t",
        b"%2573ecret",
        b"%zz%73ecret",
    ];

    for encoded in encodings {
        let current = Arc::new(AtomicBool::new(true));
        let authority = Authority(current);
        let receipts = Receipts::default();
        let connector = EncodedEcho(encoded.to_vec());
        let broker = Broker::issue(
            lease(),
            vec![policy()],
            Credential::new(b"secret".to_vec()).unwrap(),
            &authority,
            &connector,
            &receipts,
        )
        .unwrap();

        assert!(matches!(
            broker.call("model_inference", b"review this", 20),
            Err(BrokerError::CredentialExposure)
        ));
        let receipt = &receipts.0.lock().unwrap()[0];
        assert_eq!(
            receipt.failure_reason,
            Some(BrokerFailureReasonV1::CredentialExposure)
        );
        assert!(receipt.response_digest.is_none());
    }
}

#[test]
fn a_reflection_that_also_loses_the_fence_race_leaves_no_response_digest() {
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(current);
    let receipts = FenceAtReceipt::default();
    let broker = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(b"secret".to_vec()).unwrap(),
        &authority,
        &EchoingConnector,
        &receipts,
    )
    .unwrap();

    assert!(matches!(
        broker.call("model_inference", b"payload", 20),
        Err(BrokerError::Revoked)
    ));
    let receipt = &receipts.0.lock().unwrap()[0];
    assert_eq!(receipt.outcome, BrokerOperationOutcomeV1::Revoked);
    assert_eq!(
        receipt.failure_reason,
        Some(BrokerFailureReasonV1::AuthorityRevoked)
    );
    assert!(receipt.response_digest.is_none());
    assert_eq!(receipt.response_bytes, 0);
}

struct OutOfRangeUsageConnector;

impl Connector for OutOfRangeUsageConnector {
    fn execute(&self, _: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        Ok(ConnectorReply::credential_free(
            b"provider answer",
            u64::MAX,
        ))
    }
}

#[test]
fn usage_outside_the_durable_numeric_domain_is_a_normalized_overrun() {
    const JSON_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(current);
    let receipts = Receipts::default();
    let mut bounded = policy();
    bounded.max_usage = JSON_SAFE_INTEGER - 1;
    let broker = Broker::issue(
        lease(),
        vec![bounded],
        Credential::new(b"secret".to_vec()).unwrap(),
        &authority,
        &OutOfRangeUsageConnector,
        &receipts,
    )
    .unwrap();

    assert!(matches!(
        broker.call("model_inference", b"payload", JSON_SAFE_INTEGER - 1),
        Err(BrokerError::UsageOverrun)
    ));
    let receipt = &receipts.0.lock().unwrap()[0];
    assert_eq!(
        receipt.failure_reason,
        Some(BrokerFailureReasonV1::UsageOverrun)
    );
    assert_eq!(receipt.charged_usage, JSON_SAFE_INTEGER);
    assert!(receipt.response_digest.is_some());

    let mut unrepresentable = policy();
    unrepresentable.max_usage = JSON_SAFE_INTEGER;
    assert!(unrepresentable.validate().is_err());
}

struct PanickingConnector(Arc<AtomicBool>);

impl Connector for PanickingConnector {
    fn execute(&self, _: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        self.0.store(true, Ordering::SeqCst);
        panic!("connector panicked after its external side effect")
    }
}

#[test]
fn a_connector_panic_after_a_side_effect_is_charged_and_receipted() {
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(current);
    let receipts = Receipts::default();
    let side_effect = Arc::new(AtomicBool::new(false));
    let connector = PanickingConnector(Arc::clone(&side_effect));
    let broker = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(b"secret".to_vec()).unwrap(),
        &authority,
        &connector,
        &receipts,
    )
    .unwrap();

    assert!(matches!(
        broker.call("model_inference", b"payload", 20),
        Err(BrokerError::ConnectorFailed)
    ));
    assert!(side_effect.load(Ordering::SeqCst));
    assert_eq!(broker.charged_usage(), 20);
    let receipt = &receipts.0.lock().unwrap()[0];
    assert_eq!(receipt.outcome, BrokerOperationOutcomeV1::Failed);
    assert_eq!(
        receipt.failure_reason,
        Some(BrokerFailureReasonV1::ConnectorFailed)
    );
    assert_eq!(receipt.charged_usage, 20);
    assert!(receipt.response_digest.is_none());
}

#[test]
fn project_authority_fixes_egress_and_unknown_operations_never_reach_a_connector() {
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(current);
    let connector = TransformingConnector {
        calls: AtomicUsize::new(0),
        remote_bytes: Mutex::new(Vec::new()),
    };
    let receipts = Receipts::default();
    let broker = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(b"secret".to_vec()).unwrap(),
        &authority,
        &connector,
        &receipts,
    )
    .unwrap();

    assert!(matches!(
        broker.call("arbitrary_egress", b"payload", 10),
        Err(BrokerError::OperationNotAllowed)
    ));
    assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_fenced_lease_makes_no_external_call_even_if_the_process_kept_its_handle() {
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(Arc::clone(&current));
    let connector = TransformingConnector {
        calls: AtomicUsize::new(0),
        remote_bytes: Mutex::new(Vec::new()),
    };
    let receipts = Receipts::default();
    let broker = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(b"secret".to_vec()).unwrap(),
        &authority,
        &connector,
        &receipts,
    )
    .unwrap();
    current.store(false, Ordering::SeqCst);

    assert!(matches!(
        broker.call("model_inference", b"payload", 10),
        Err(BrokerError::Revoked)
    ));
    assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        receipts.0.lock().unwrap()[0].outcome,
        BrokerOperationOutcomeV1::Revoked
    );
}

struct FenceDuringCall {
    current: Arc<AtomicBool>,
}

struct BlockingConnector {
    entered: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl Connector for BlockingConnector {
    fn execute(&self, _: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        self.entered.send(()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        Ok(ConnectorReply::credential_free(b"late provider answer", 5))
    }
}

#[test]
fn revoke_waits_for_and_withholds_an_in_flight_response() {
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(current);
    let receipts = Receipts::default();
    let (entered_tx, entered_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let connector = BlockingConnector {
        entered: entered_tx,
        release: Mutex::new(release_rx),
    };
    let broker = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(b"secret".to_vec()).unwrap(),
        &authority,
        &connector,
        &receipts,
    )
    .unwrap();

    std::thread::scope(|scope| {
        let call = scope.spawn(|| broker.call("model_inference", b"payload", 10));
        entered_rx.recv().unwrap();
        let revocation = scope.spawn(|| broker.revoke());
        while !broker.is_revoked() {
            std::thread::yield_now();
        }
        assert!(!revocation.is_finished());
        release_tx.send(()).unwrap();
        assert!(matches!(call.join().unwrap(), Err(BrokerError::Revoked)));
        revocation.join().unwrap();
    });

    let receipt = &receipts.0.lock().unwrap()[0];
    assert_eq!(receipt.outcome, BrokerOperationOutcomeV1::Revoked);
    assert_eq!(receipt.charged_usage, 5);
}

impl Connector for FenceDuringCall {
    fn execute(&self, _: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        self.current.store(false, Ordering::SeqCst);
        Ok(ConnectorReply::credential_free(b"late provider answer", 5))
    }
}

#[test]
fn a_response_that_races_with_fencing_is_receipted_but_withheld() {
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(Arc::clone(&current));
    let connector = FenceDuringCall { current };
    let receipts = Receipts::default();
    let broker = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(b"secret".to_vec()).unwrap(),
        &authority,
        &connector,
        &receipts,
    )
    .unwrap();

    assert!(matches!(
        broker.call("model_inference", b"payload", 10),
        Err(BrokerError::Revoked)
    ));
    let receipt = &receipts.0.lock().unwrap()[0];
    assert_eq!(receipt.outcome, BrokerOperationOutcomeV1::Revoked);
    assert_eq!(receipt.charged_usage, 5);
    assert!(receipt.response_digest.is_some());
}

#[derive(Default)]
struct FenceAtReceipt(Mutex<Vec<review_core::BrokerOperationReceiptV1>>);

impl ReceiptSink for FenceAtReceipt {
    fn record(&self, receipt: &review_core::BrokerOperationReceiptV1) -> Result<(), ReceiptError> {
        if receipt.outcome != BrokerOperationOutcomeV1::Revoked {
            return Err(ReceiptError::AuthorityRevoked);
        }
        self.0.lock().unwrap().push(receipt.clone());
        Ok(())
    }
}

#[test]
fn fencing_that_wins_at_receipt_commit_rewrites_success_to_revoked() {
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(current);
    let connector = TransformingConnector {
        calls: AtomicUsize::new(0),
        remote_bytes: Mutex::new(Vec::new()),
    };
    let receipts = FenceAtReceipt::default();
    let broker = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(b"secret".to_vec()).unwrap(),
        &authority,
        &connector,
        &receipts,
    )
    .unwrap();

    assert!(matches!(
        broker.call("model_inference", b"payload", 10),
        Err(BrokerError::Revoked)
    ));
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    let receipt = &receipts.0.lock().unwrap()[0];
    assert_eq!(receipt.outcome, BrokerOperationOutcomeV1::Revoked);
    assert_eq!(
        receipt.failure_reason,
        Some(BrokerFailureReasonV1::AuthorityRevoked)
    );
    assert!(receipt.response_digest.is_some());
}

#[test]
fn quota_refusal_is_receipted_once_and_then_revokes_locally() {
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(Arc::clone(&current));
    let connector = TransformingConnector {
        calls: AtomicUsize::new(0),
        remote_bytes: Mutex::new(Vec::new()),
    };
    let receipts = Receipts::default();
    let broker = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(b"secret".to_vec()).unwrap(),
        &authority,
        &connector,
        &receipts,
    )
    .unwrap();

    broker.call("model_inference", b"one", 10).unwrap();
    broker.call("model_inference", b"two", 10).unwrap();
    assert!(matches!(
        broker.call("model_inference", b"three", 10),
        Err(BrokerError::QuotaExceeded)
    ));
    for _ in 0..10 {
        assert!(matches!(
            broker.call("model_inference", b"again", 10),
            Err(BrokerError::Revoked)
        ));
    }
    assert_eq!(connector.calls.load(Ordering::SeqCst), 2);
    assert_eq!(receipts.0.lock().unwrap().len(), 3);

    let zero_connector = TransformingConnector {
        calls: AtomicUsize::new(0),
        remote_bytes: Mutex::new(Vec::new()),
    };
    let zero_receipts = Receipts::default();
    let zero = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(b"secret".to_vec()).unwrap(),
        &authority,
        &zero_connector,
        &zero_receipts,
    )
    .unwrap();
    assert!(matches!(
        zero.call("model_inference", b"zero", 0),
        Err(BrokerError::QuotaExceeded)
    ));
    assert!(matches!(
        zero.call("model_inference", b"again", 0),
        Err(BrokerError::Revoked)
    ));
    assert_eq!(zero_connector.calls.load(Ordering::SeqCst), 0);
    assert_eq!(zero_receipts.0.lock().unwrap().len(), 1);
}

struct FailingReceipts;

impl ReceiptSink for FailingReceipts {
    fn record(&self, _: &review_core::BrokerOperationReceiptV1) -> Result<(), ReceiptError> {
        Err(ReceiptError::Unavailable)
    }
}

#[test]
fn a_receipt_failure_withholds_the_response_and_revokes_further_calls() {
    let current = Arc::new(AtomicBool::new(true));
    let authority = Authority(current);
    let connector = TransformingConnector {
        calls: AtomicUsize::new(0),
        remote_bytes: Mutex::new(Vec::new()),
    };
    let broker = Broker::issue(
        lease(),
        vec![policy()],
        Credential::new(b"secret".to_vec()).unwrap(),
        &authority,
        &connector,
        &FailingReceipts,
    )
    .unwrap();

    assert!(matches!(
        broker.call("model_inference", b"payload", 10),
        Err(BrokerError::ReceiptFailed)
    ));
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        broker.call("model_inference", b"retry", 10),
        Err(BrokerError::Revoked)
    ));
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
}
