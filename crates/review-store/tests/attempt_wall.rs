//! The wall-clock sidecar: recorded beside the event stream, replaced per Attempt, and absent —
//! never an error — in a store written before it existed.

use review_store::{AttemptUsage, AttemptWall, EventStore};

fn wall(attempt: &str, started_unix_ms: u64, elapsed_ms: u64) -> AttemptWall {
    AttemptWall {
        run_id: "campaign-x".into(),
        attempt_id: attempt.into(),
        node_id: "correctness".into(),
        round: 1,
        epoch: 1,
        started_unix_ms,
        elapsed_ms,
        usage: Some(AttemptUsage {
            input_tokens: Some(200_000),
            output_tokens: Some(3_000),
            cache_read_tokens: Some(180_000),
            cache_write_tokens: None,
            reasoning_tokens: Some(1_000),
            chargeable_tokens: 23_000,
        }),
    }
}

#[test]
fn rows_round_trip_ordered_by_start_and_replace_per_attempt() {
    let store = EventStore::open_in_memory().unwrap();
    store.record_attempt_wall(&wall("b", 2_000, 10)).unwrap();
    store.record_attempt_wall(&wall("a", 1_000, 10)).unwrap();
    let mut unmeasured = wall("c", 3_000, 5);
    unmeasured.usage = None;
    store.record_attempt_wall(&unmeasured).unwrap();
    // A second write for the same Attempt replaces the first.
    store.record_attempt_wall(&wall("b", 2_000, 42)).unwrap();

    let rows = store.attempt_wall("campaign-x").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], wall("a", 1_000, 10));
    assert_eq!(rows[1], wall("b", 2_000, 42));
    assert_eq!(rows[2], unmeasured);
    assert!(store.attempt_wall("campaign-other").unwrap().is_empty());
}

#[test]
fn a_store_written_before_the_sidecar_reads_as_no_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE events (
                 run_id TEXT NOT NULL, sequence INTEGER NOT NULL, event_id TEXT NOT NULL UNIQUE,
                 type TEXT NOT NULL, occurred_at TEXT NOT NULL, node_id TEXT, attempt_id TEXT,
                 causation_id TEXT, correlation_id TEXT, artifact_refs TEXT NOT NULL,
                 payload TEXT NOT NULL, PRIMARY KEY (run_id, sequence));",
        )
        .unwrap();
    }
    let read_only = EventStore::open_read_only(&path).unwrap();
    assert!(read_only.attempt_wall("campaign-x").unwrap().is_empty());
    drop(read_only);

    // Opening read-write adds the table without touching the events; then recording works.
    let store = EventStore::open(&path).unwrap();
    store.record_attempt_wall(&wall("a", 1, 1)).unwrap();
    assert_eq!(store.attempt_wall("campaign-x").unwrap().len(), 1);
}
