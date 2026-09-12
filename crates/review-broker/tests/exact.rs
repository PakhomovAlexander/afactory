use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use review_broker::{
    AuthorityError, BrokerError, BrokerHandle, Connector, ConnectorCall, ConnectorError,
    ConnectorReply, Credential, ExactBroker, ExactBrokerClient, ExactReceiptSink, LeaseAuthority,
    ReceiptError,
};
use review_core::{
    BrokerFailureReasonV1, BrokerLeaseV1, BrokerOperationOutcomeV1, BrokerOperationPolicyV1,
    BrokerOperationReceiptV2,
};

struct Authority(AtomicBool);
impl LeaseAuthority for Authority {
    fn ensure_current(&self, _: &BrokerLeaseV1, _: &BrokerHandle) -> Result<(), AuthorityError> {
        self.0
            .load(Ordering::SeqCst)
            .then_some(())
            .ok_or(AuthorityError)
    }
}

struct ConnectorFixture<'a> {
    calls: AtomicUsize,
    charges: Vec<u64>,
    revoke: Option<&'a Authority>,
}
impl Connector for ConnectorFixture<'_> {
    fn execute(&self, call: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        assert_eq!(call.destination, "provider.personal");
        assert_eq!(call.method, "inference");
        assert_eq!(call.credential(), b"local-private-secret");
        let charge = self.charges[self.calls.fetch_add(1, Ordering::SeqCst)];
        if let Some(authority) = self.revoke {
            authority.0.store(false, Ordering::SeqCst);
        }
        Ok(ConnectorReply::credential_free(b"bounded answer", charge))
    }
}

#[derive(Default)]
struct Receipts {
    values: Mutex<Vec<BrokerOperationReceiptV2>>,
    fence_at_commit: bool,
    unavailable: bool,
}
impl ExactReceiptSink for Receipts {
    fn record(&self, receipt: &BrokerOperationReceiptV2) -> Result<(), ReceiptError> {
        receipt.validate().unwrap();
        if self.unavailable {
            return Err(ReceiptError::Unavailable);
        }
        if self.fence_at_commit && receipt.outcome != BrokerOperationOutcomeV1::Revoked {
            return Err(ReceiptError::AuthorityRevoked);
        }
        self.values.lock().unwrap().push(receipt.clone());
        Ok(())
    }
}

fn issue<'a>(
    authority: &'a Authority,
    connector: &'a dyn Connector,
    receipts: &'a Receipts,
) -> ExactBroker<'a> {
    ExactBroker::issue(
        BrokerLeaseV1 {
            campaign_id: "captured-review".into(),
            round_event_id: "0123456789abcdefghijklmnop".into(),
            node_id: "reviewer".into(),
            attempt_id: "abcdefghijklmnopqrstuv0123".into(),
            lease_epoch: 1,
        },
        vec![BrokerOperationPolicyV1 {
            name: "ask".into(),
            destination: "provider.personal".into(),
            method: "inference".into(),
            max_request_bytes: 100,
            max_response_bytes: 100,
            max_calls: 3,
            max_usage: 100,
        }],
        Credential::new(b"local-private-secret".to_vec()).unwrap(),
        authority,
        connector,
        receipts,
    )
    .unwrap()
}

#[test]
fn prior_paid_operation_plus_u64_max_is_exact_and_revokes_without_a_response() {
    let authority = Authority(AtomicBool::new(true));
    let connector = ConnectorFixture {
        calls: AtomicUsize::new(0),
        charges: vec![7, u64::MAX],
        revoke: None,
    };
    let receipts = Receipts::default();
    let broker = issue(&authority, &connector, &receipts);
    let first = broker.call("ask", b"first", 20).unwrap();
    assert_eq!(first.body, b"bounded answer");
    assert_eq!(first.receipt.charged_usage.get(), 7);
    assert!(matches!(
        broker.call("ask", b"second", 20),
        Err(BrokerError::UsageOverrun)
    ));
    assert_eq!(broker.charged_usage(), u128::from(u64::MAX) + 7);
    assert!(broker.is_revoked());
    assert!(matches!(
        broker.call("ask", b"third", 20),
        Err(BrokerError::Revoked)
    ));
    assert_eq!(connector.calls.load(Ordering::SeqCst), 2);
    let values = receipts.values.lock().unwrap();
    assert_eq!(values.iter().map(|r| r.ordinal).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(values[1].charged_usage.get(), u64::MAX);
    assert_eq!(
        values[1].failure_reason,
        Some(BrokerFailureReasonV1::UsageOverrun)
    );
    let encoded = serde_json::to_value(&values[1]).unwrap();
    assert_eq!(encoded["charged_usage"], u64::MAX.to_string());
    assert_eq!(
        serde_json::from_value::<BrokerOperationReceiptV2>(encoded.clone()).unwrap(),
        values[1]
    );
    assert!(values[1].clone().try_into_legacy().is_err());
    assert!(values[0].clone().try_into_legacy().is_ok());
    for invalid in [
        serde_json::json!(u64::MAX),
        serde_json::json!("18446744073709551616"),
        serde_json::json!("01"),
        serde_json::json!("1e3"),
    ] {
        let mut bad = encoded.clone();
        bad["charged_usage"] = invalid;
        assert!(serde_json::from_value::<BrokerOperationReceiptV2>(bad).is_err());
    }
}

#[test]
fn receipt_commit_fencing_preserves_full_width_overrun_charge() {
    let authority = Authority(AtomicBool::new(true));
    let connector = ConnectorFixture {
        calls: AtomicUsize::new(0),
        charges: vec![u64::MAX],
        revoke: None,
    };
    let receipts = Receipts {
        fence_at_commit: true,
        ..Receipts::default()
    };
    let broker = issue(&authority, &connector, &receipts);
    assert!(matches!(
        broker.call("ask", b"input", 20),
        Err(BrokerError::Revoked)
    ));
    let values = receipts.values.lock().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].outcome, BrokerOperationOutcomeV1::Revoked);
    assert_eq!(values[0].charged_usage.get(), u64::MAX);
    assert_eq!(broker.charged_usage(), u128::from(u64::MAX));
}

#[test]
fn post_call_revocation_and_unavailable_receipts_withhold_bodies_and_retain_charge() {
    for unavailable in [false, true] {
        let authority = Authority(AtomicBool::new(true));
        let connector = ConnectorFixture {
            calls: AtomicUsize::new(0),
            charges: vec![9],
            revoke: (!unavailable).then_some(&authority),
        };
        let receipts = Receipts {
            unavailable,
            ..Receipts::default()
        };
        let broker = issue(&authority, &connector, &receipts);
        let result = broker.call("ask", b"input", 20);
        assert!(matches!(result, Err(BrokerError::ReceiptFailed)) == unavailable);
        assert!(broker.is_revoked());
        assert_eq!(broker.charged_usage(), 9);
        if !unavailable {
            let values = receipts.values.lock().unwrap();
            assert_eq!(values.len(), 1);
            assert_eq!(values[0].outcome, BrokerOperationOutcomeV1::Revoked);
            assert_eq!(values[0].charged_usage.get(), 9);
        }
    }
}
