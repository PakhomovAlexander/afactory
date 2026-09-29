//! The wall-clock sidecar: recorded beside the event stream, one row per Attempt, and never
//! lowered by an older or absent measurement.

use review_core::task::usage::{DecimalU128, TaskTokenUsageV3};
use review_store::{EventStore, TaskAttemptWall};

fn task_wall(attempt: &str, tokens: u128, elapsed_ms: u64) -> TaskAttemptWall {
    TaskAttemptWall {
        run_id: "campaign-x".into(),
        attempt_id: attempt.into(),
        node_id: "correctness".into(),
        round: 1,
        epoch: 1,
        started_unix_ms: 1,
        elapsed_ms,
        usage: Some(TaskTokenUsageV3 {
            input_tokens: Some(u128::from(u64::MAX).into()),
            chargeable_tokens: tokens.into(),
            ..Default::default()
        }),
    }
}

#[test]
fn rows_round_trip_ordered_by_start_and_replace_per_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let wall = |attempt: &str, started_unix_ms: u64, elapsed_ms: u64| TaskAttemptWall {
        started_unix_ms,
        ..task_wall(attempt, 23_000, elapsed_ms)
    };
    store
        .record_task_attempt_wall(&wall("b", 2_000, 10))
        .unwrap();
    store
        .record_task_attempt_wall(&wall("a", 1_000, 10))
        .unwrap();
    let mut unmeasured = wall("c", 3_000, 5);
    unmeasured.usage = None;
    store.record_task_attempt_wall(&unmeasured).unwrap();
    // A second write for the same Attempt replaces the first.
    store
        .record_task_attempt_wall(&wall("b", 2_000, 42))
        .unwrap();

    let rows = store.task_attempt_wall("campaign-x").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], wall("a", 1_000, 10));
    assert_eq!(rows[1], wall("b", 2_000, 42));
    assert_eq!(rows[2], unmeasured);
    assert!(
        store
            .task_attempt_wall("campaign-other")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_fresh_store_creates_the_single_usage_encoding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    drop(EventStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let mut statement = conn
        .prepare("SELECT name FROM pragma_table_info('attempt_wall') ORDER BY cid")
        .unwrap();
    let columns: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        columns,
        [
            "run_id",
            "attempt_id",
            "node_id",
            "round",
            "epoch",
            "started_unix_ms",
            "elapsed_ms",
            "usage_v3_json",
            "usage_observation_v1_json",
        ]
    );
    let store = EventStore::open_read_only(&path).unwrap();
    assert!(store.task_attempt_wall("campaign-x").unwrap().is_empty());
}

#[test]
fn exact_usage_survives_reopen_and_cannot_be_replaced_by_a_lower_observation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    let exact = u128::from(u64::MAX) + 20;
    let mut wide = task_wall("wide", exact, 1);
    let usage = wide.usage.as_mut().unwrap();
    usage.input_tokens = Some(exact.into());
    usage.output_tokens = Some(3_000.into());
    usage.cache_read_tokens = Some(exact.into());
    usage.cache_write_tokens = Some(u128::from(i64::MAX as u64 + 1).into());
    usage.reasoning_tokens = Some(exact.into());
    {
        let store = EventStore::open(&path).unwrap();
        store
            .record_task_attempt_wall(&task_wall("wide", 7, 1))
            .unwrap();
        store.record_task_attempt_wall(&wide).unwrap();
        // Neither an older cumulative report nor an absent report can erase known spend.
        store
            .record_task_attempt_wall(&task_wall("wide", 1, 2))
            .unwrap();
        let mut absent = task_wall("wide", 0, 3);
        absent.usage = None;
        store.record_task_attempt_wall(&absent).unwrap();
    }
    let store = EventStore::open_read_only(&path).unwrap();
    let rows = store.task_attempt_wall("campaign-x").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].usage, wide.usage);
    assert_eq!(rows[0].elapsed_ms, 3);
    assert_eq!(
        rows[0]
            .usage
            .as_ref()
            .unwrap()
            .input_tokens
            .map(DecimalU128::get),
        Some(exact)
    );
    let conn = rusqlite::Connection::open(&path).unwrap();
    let (kind, text): (String, String) = conn
        .query_row(
            "SELECT typeof(usage_v3_json), usage_v3_json FROM attempt_wall",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "text", "never coerce wide counters into SQLite REAL");
    assert!(text.contains("\"chargeable_tokens\":\"18446744073709551635\""));
}

#[test]
fn malformed_task_usage_never_falls_back_or_gets_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    let store = EventStore::open(&path).unwrap();
    store
        .record_task_attempt_wall(&task_wall("a", 23_000, 1))
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    for invalid in [
        "{}",
        "{\"chargeable_tokens\":1}",
        "{\"chargeable_tokens\":\"01\"}",
        "{\"chargeable_tokens\":\"340282366920938463463374607431768211456\"}",
        "{\"chargeable_tokens\":\"1\",\"input_tokens\":null}",
        "{\"chargeable_tokens\":\"1\",\"input_tokens\":\"340282366920938463463374607431768211456\"}",
    ] {
        conn.execute("UPDATE attempt_wall SET usage_v3_json = ?1", [invalid])
            .unwrap();
        assert!(store.task_attempt_wall("campaign-x").is_err());
        assert!(
            store
                .record_task_attempt_wall(&task_wall("a", 7, 2))
                .is_err()
        );
        let (retained, elapsed): (String, i64) = conn
            .query_row(
                "SELECT usage_v3_json, elapsed_ms FROM attempt_wall",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(retained, invalid);
        assert_eq!(elapsed, 1);
    }
}

#[test]
fn observation_is_atomic_sticky_and_never_erased_by_older_or_absent_measurement() {
    use review_core::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    let store = EventStore::open(&path).unwrap();
    let observation = TaskUsageObservationV1 {
        reported_usage: Some(TaskTokenUsageV3::charge_only(7)),
        charge_complete: false,
    };
    store
        .record_task_attempt_wall_with_observation(&task_wall("a", 100, 1), &observation)
        .unwrap();
    for supplied in [
        None,
        Some(TaskUsageObservationV1 {
            reported_usage: Some(TaskTokenUsageV3::charge_only(9)),
            charge_complete: true,
        }),
    ] {
        if let Some(value) = supplied {
            store
                .record_task_attempt_wall_with_observation(&task_wall("a", 9, 2), &value)
                .unwrap();
        } else {
            let mut wall = task_wall("a", 0, 2);
            wall.usage = None;
            store.record_task_attempt_wall(&wall).unwrap();
        }
        assert!(
            !store
                .task_attempt_usage_observation("campaign-x", "a")
                .unwrap()
                .unwrap()
                .charge_complete
        );
        assert_eq!(
            store.task_attempt_wall("campaign-x").unwrap()[0]
                .usage
                .as_ref()
                .unwrap()
                .chargeable_tokens
                .get(),
            100
        );
    }
    store
        .record_task_attempt_wall(&task_wall("a", 23_000, 3))
        .unwrap();
    let expected = store
        .task_attempt_usage_observation("campaign-x", "a")
        .unwrap()
        .unwrap();
    assert!(!expected.charge_complete);
    assert_eq!(
        expected
            .reported_usage
            .as_ref()
            .unwrap()
            .chargeable_tokens
            .get(),
        9
    );
    assert_eq!(
        store.task_attempt_wall("campaign-x").unwrap()[0]
            .usage
            .as_ref()
            .unwrap()
            .chargeable_tokens
            .get(),
        23000
    );
    let too_high = TaskUsageObservationV1 {
        reported_usage: Some(TaskTokenUsageV3::charge_only(u128::MAX)),
        charge_complete: false,
    };
    assert!(
        store
            .record_task_attempt_wall_with_observation(&task_wall("a", 9, 99), &too_high)
            .is_err()
    );
    assert_eq!(
        store.task_attempt_wall("campaign-x").unwrap()[0].elapsed_ms,
        3,
        "failed observation cannot partially write wall or usage"
    );
    drop(store);
    let store = EventStore::open_read_only(&path).unwrap();
    assert_eq!(
        store
            .task_attempt_usage_observation("campaign-x", "a")
            .unwrap(),
        Some(expected)
    );
    let conn = rusqlite::Connection::open(&path).unwrap();
    for bad in [
        "{}",
        "null",
        r#"{"charge_complete":true}"#,
        r#"{"charge_complete":false,"reported_usage":null}"#,
    ] {
        conn.execute(
            "UPDATE attempt_wall SET usage_observation_v1_json=?1",
            [bad],
        )
        .unwrap();
        assert!(
            store
                .task_attempt_usage_observation("campaign-x", "a")
                .is_err()
        );
        assert!(
            store.task_attempt_wall("campaign-x").is_err(),
            "invalid observation cannot fall back to known usage"
        );
        let writer = EventStore::open(&path).unwrap();
        assert!(
            writer
                .record_task_attempt_wall(&task_wall("a", 0, 100))
                .is_err()
        );
    }
}
