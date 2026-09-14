use super::*;

#[test]
fn historical_task_link_is_idempotent_read_only_and_preserves_ids_and_charges() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("tasks.sqlite");
    let original_cas = Cas::open(dir.path().join("original-cas")).unwrap();
    let artifact_id = original_cas
        .put_json(&json!({"schema":"af/task-outcome@1", "usage":{"chargeable_tokens":17}}))
        .unwrap();
    original_cas.flush().unwrap();
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TABLE task_events(task_id TEXT, sequence INTEGER, event_type TEXT, artifact_id TEXT, PRIMARY KEY(task_id, sequence))").unwrap();
    connection
        .execute(
            "INSERT INTO task_events VALUES (?1, 1, 'task-finished@1', ?2)",
            params!["original-task-42", artifact_id],
        )
        .unwrap();
    drop(connection);
    let original_bytes = std::fs::read(&database).unwrap();
    let origin = LegacyStoreOrigin::at(&database, &dir.path().join("original-cas")).unwrap();
    let common_cas = Cas::open(dir.path().join("common-cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let link = store
        .link_legacy_task(
            &common_cas,
            &origin,
            LegacyExecutionKind::Implementation,
            "original-task-42",
        )
        .unwrap();
    assert_eq!(
        link,
        store
            .link_legacy_task(
                &common_cas,
                &origin,
                LegacyExecutionKind::Implementation,
                "original-task-42"
            )
            .unwrap()
    );
    assert_eq!(link.record_id, "original-task-42");
    assert_eq!(
        store.legacy_task_links(&common_cas).unwrap(),
        [link.clone()]
    );
    assert!(
        store.run_ids().unwrap().is_empty(),
        "linking never copies Attempts or charges into a new run"
    );
    assert_eq!(std::fs::read(&database).unwrap(), original_bytes);
    let LegacyExecutionHistory::Implementation { events } = link.read(&common_cas).unwrap() else {
        panic!("wrong adapter")
    };
    assert_eq!(events[0].artifact_id, artifact_id);
    assert_eq!(
        original_cas.get_json(&events[0].artifact_id).unwrap()["usage"]["chargeable_tokens"],
        17
    );

    // A legacy continuation still belongs to the original runner and can extend the prefix.
    let second = original_cas
        .put_json(&json!({"delivery":"recorded"}))
        .unwrap();
    original_cas.flush().unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "INSERT INTO task_events VALUES (?1, 2, 'delivery-recorded@1', ?2)",
            params!["original-task-42", second],
        )
        .unwrap();
    assert_eq!(
        store
            .link_legacy_task(
                &common_cas,
                &origin,
                LegacyExecutionKind::Implementation,
                "original-task-42"
            )
            .unwrap(),
        link
    );
    let LegacyExecutionHistory::Implementation { events } = link.read(&common_cas).unwrap() else {
        panic!("wrong adapter")
    };
    assert_eq!(events.len(), 2);
    connection
        .execute(
            "UPDATE task_events SET artifact_id = ?1 WHERE sequence = 1",
            [&second],
        )
        .unwrap();
    assert!(
        link.read(&common_cas)
            .unwrap_err()
            .to_string()
            .contains("rewritten")
    );
    assert_eq!(store.legacy_task_links(&common_cas).unwrap().len(), 1);
}

#[test]
fn missing_legacy_store_never_creates_a_database_and_sequence_gaps_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent.sqlite");
    assert!(LegacyStoreOrigin::at(&absent, &dir.path().join("cas")).is_err());
    assert!(!absent.exists());
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let id = cas.put_json(&json!({"legacy":true})).unwrap();
    let database = dir.path().join("tasks.sqlite");
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TABLE task_events(task_id TEXT, sequence INTEGER, event_type TEXT, artifact_id TEXT)").unwrap();
    connection
        .execute(
            "INSERT INTO task_events VALUES ('gap', 2, 'task@1', ?1)",
            [&id],
        )
        .unwrap();
    let origin = LegacyStoreOrigin::at(&database, &dir.path().join("cas")).unwrap();
    assert!(
        origin
            .read(LegacyExecutionKind::Implementation, "gap")
            .is_err()
    );
}

#[test]
fn frozen_campaign_link_retains_original_event_identity_and_verdict_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("review.sqlite");
    let old_cas_path = dir.path().join("original-cas");
    let _original_cas = Cas::open(&old_cas_path).unwrap();
    let old = EventStore::open(&database).unwrap();
    let event_id = crate::store::derive_event_id("campaign-old", 0);
    let report = json!({"outcomes":[{"node":"reviewer", "status":"completed", "detail":{"out":["result"]}}], "blocked_gates":[], "verdict":"Fail(Exhausted)", "spent_tokens":23});
    review_core::event::validate_event_payload(review_core::EventType::RunReportV1, &report)
        .unwrap();
    // These are archived RunReport@1 bytes, written before Task existed.
    old.conn.execute("INSERT INTO events(run_id, sequence, event_id, type, occurred_at, artifact_refs, payload) VALUES ('campaign-old', 0, ?1, 'RunReport@1', '2026-09-01T00:00:00Z', '[]', ?2)", params![event_id, serde_json::to_string(&report).unwrap()]).unwrap();
    drop(old);
    let before = std::fs::read(&database).unwrap();
    let common_cas = Cas::open(dir.path().join("common-cas")).unwrap();
    let mut common = EventStore::open_in_memory().unwrap();
    let origin = LegacyStoreOrigin::at(&database, &old_cas_path).unwrap();
    let link = common
        .link_legacy_task(
            &common_cas,
            &origin,
            LegacyExecutionKind::Review,
            "campaign-old",
        )
        .unwrap();
    let LegacyExecutionHistory::Review { events } = link.read(&common_cas).unwrap() else {
        panic!("wrong adapter")
    };
    assert_eq!(events[0].event_id, event_id);
    assert_eq!(events[0].payload, report);
    assert_eq!(std::fs::read(database).unwrap(), before);
    assert!(common.run_ids().unwrap().is_empty());
}
