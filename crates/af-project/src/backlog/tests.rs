use super::*;
use af_contracts::backlog::PublicFields;
use serde_json::{Value, json};

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn emit(store: &mut ProjectStore, record: BacklogRecordV1) -> String {
    let payload = serde_json::to_value(record).unwrap();
    let id = content_id(&payload).unwrap();
    store
        .events
        .append(
            PROCESSING,
            &store.cas,
            NewEvent::new(EventType::BacklogRecordStoredV1, payload),
        )
        .unwrap();
    id
}

#[test]
fn verified_projection_exposes_only_current_approved_fields() {
    let state = tempfile::tempdir().unwrap();
    let project_id = digest('a');
    let mut store = ProjectStore::open_unrecorded(state.path(), &project_id).unwrap();
    let mut revision = ItemRevision {
        project_id: project_id.clone(),
        item_id: digest('b'),
        report_ids: vec!["private-report".into()],
        claim: "private-claim".into(),
        source_revision: "c".repeat(40),
        evidence_ids: vec![digest('d')],
        proposed: PublicFields {
            title: "Approved title".into(),
            body: "Approved body".into(),
        },
    };
    let revision_id = emit(&mut store, BacklogRecordV1::ItemRevision(revision.clone()));
    assert!(store.verified_backlog().unwrap().is_empty());
    let batch_id = emit(
        &mut store,
        BacklogRecordV1::Batch(Batch {
            project_id: project_id.clone(),
            authority_id: digest('e'),
            first_sequence: 1.into(),
            last_sequence: 1.into(),
            report_ids: revision.report_ids.clone(),
            scenario_id: digest('f'),
            policy_id: digest('1'),
        }),
    );
    let mut decision = Decision {
        project_id,
        batch_id,
        revision_id: revision_id.clone(),
        independent_verification_id: Some(digest('2')),
        task_results: vec![digest('2')],
        disposition: Disposition::Verified,
        reason: "private-reason".into(),
        evidence_ids: revision.evidence_ids.clone(),
    };
    let verified = decision.clone();
    emit(&mut store, BacklogRecordV1::Decision(verified.clone()));
    let value: Value = serde_json::to_value(store.verified_backlog().unwrap()).unwrap();
    assert_eq!(
        value,
        json!([{
            "item_id": revision.item_id,
            "revision_id": revision_id,
            "public": {"title": "Approved title", "body": "Approved body"}
        }])
    );

    decision.disposition = Disposition::Held;
    emit(&mut store, BacklogRecordV1::Decision(decision));
    emit(&mut store, BacklogRecordV1::Decision(verified));
    assert!(store.verified_backlog().unwrap().is_empty());

    revision.proposed.body = "Changed body".into();
    emit(&mut store, BacklogRecordV1::ItemRevision(revision));
    assert!(store.verified_backlog().unwrap().is_empty());
}
