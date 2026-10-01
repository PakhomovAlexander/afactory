use super::*;

#[test]
fn heartbeat_reads_exact_fresh_writer_and_expiry_without_granting_authority() {
    let mut f = Fixture::new(false);
    let old = f.open();
    f.propose(&old);
    let reader = EventStore::open_read_only(&f.path).unwrap();
    assert_eq!(
        reader.task_lease_state(&old).unwrap(),
        f.state().lease_until
    );
    let wrong_writer = TaskLease {
        writer: "other".into(),
        ..old.clone()
    };
    let wrong_epoch = TaskLease {
        epoch: old.epoch + 1,
        ..old.clone()
    };
    assert!(reader.task_lease_state(&wrong_writer).is_err());
    assert!(reader.task_lease_state(&wrong_epoch).is_err());
    f.store.renew_task_lease(&f.cas, &old, 2_000_000).unwrap();
    assert_eq!(
        reader.task_lease_state(&old).unwrap(),
        f.state().lease_until
    );
    f.store.release_task_lease(&f.cas, &old).unwrap();
    assert!(reader.task_lease_state(&old).is_err());
    let current = f
        .store
        .take_task_lease(&f.cas, "task-1", "writer-2", 30_000)
        .unwrap();
    assert!(reader.task_lease_state(&old).is_err());
    let prefix = f.store.replay(&task_run_id("task-1").unwrap()).unwrap();
    assert_eq!(
        reader.task_lease_state(&current).unwrap(),
        f.state().lease_until
    );
    // Heartbeat liveness intentionally does not make a corrupt plan usable. The complete
    // renewal and dispatch paths still refuse its missing CAS authority.
    let id = f.plan_id.strip_prefix("sha256:").unwrap();
    let artifact = f
        ._dir
        .path()
        .join("cas/objects")
        .join(&id[..2])
        .join(&id[2..]);
    std::fs::remove_file(artifact).unwrap();
    assert!(reader.task_lease_state(&current).is_ok());
    assert!(f.store.renew_task_lease(&f.cas, &current, 60_000).is_err());
    assert!(
        f.store
            .admit_task_plan(&f.cas, &current, &f.authority)
            .is_err()
    );
    assert_eq!(
        f.store.replay(&task_run_id("task-1").unwrap()).unwrap(),
        prefix
    );
}

#[test]
fn heartbeat_refuses_expiry_future_clocks_and_changed_latest_writer() {
    for case in [
        "expiry",
        "future",
        "writer",
        "epoch",
        "foreign",
        "malformed",
    ] {
        let mut f = Fixture::new(false);
        let lease = f.open();
        f.propose(&lease);
        let run = task_run_id("task-1").unwrap();
        let connection = rusqlite::Connection::open(&f.path).unwrap();
        match case {
            "expiry" => {
                connection.execute("UPDATE events SET payload=json_set(payload,'$.change.lease_until_unix_ms',json_extract(payload,'$.now_unix_ms')+1) WHERE run_id=?1 AND sequence=0", [&run]).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            "future" => {
                connection.execute("UPDATE events SET payload=json_set(payload,'$.now_unix_ms',?2) WHERE run_id=?1 AND sequence=1", rusqlite::params![run, i64::try_from(now().unwrap()+10_000).unwrap()]).unwrap();
            }
            "writer" | "epoch" => {
                let (field, value) = if case == "writer" {
                    ("$.writer", json!("stolen"))
                } else {
                    ("$.epoch", json!(2))
                };
                connection.execute("UPDATE events SET payload=json_set(payload,?2,json(?3)) WHERE run_id=?1 AND sequence=1", rusqlite::params![run, field, value.to_string()]).unwrap();
            }
            "foreign" => {
                connection
                    .execute(
                        "UPDATE events SET type='RoundStarted@1' WHERE run_id=?1 AND sequence=1",
                        [&run],
                    )
                    .unwrap();
            }
            "malformed" => {
                connection
                    .execute(
                        "UPDATE events SET payload='{}' WHERE run_id=?1 AND sequence=1",
                        [&run],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(f.store.task_lease_state(&lease).is_err(), "{case}");
        let reopened = EventStore::open_read_only(&f.path).unwrap();
        assert!(
            reopened.task_lease_state(&lease).is_err(),
            "reopened {case}"
        );
    }
}

#[test]
fn a_reopened_connection_renews_only_the_live_exact_writer_and_the_caller_sees_it() {
    let mut f = Fixture::new(false);
    let lease = f.open();
    f.propose(&lease);
    let run = task_run_id("task-1").unwrap();
    let mut heartbeat = f.store.reopen(std::time::Duration::from_secs(1)).unwrap();
    // The caller's cached projection sees the other connection's renewal and keeps writing.
    heartbeat
        .renew_task_lease(&f.cas, &lease, 2_000_000)
        .unwrap();
    assert_eq!(
        f.state().lease_until,
        heartbeat.task_lease_state(&lease).unwrap()
    );
    f.store.renew_task_lease(&f.cas, &lease, 3_000_000).unwrap();
    assert_eq!(
        heartbeat.task_lease_state(&lease).unwrap(),
        f.state().lease_until
    );
    // A real successor fences the old writer on both connections; nothing is appended for it.
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let successor = f
        .store
        .take_task_lease(&f.cas, "task-1", "writer-2", 30_000)
        .unwrap();
    let prefix = f.store.len(&run).unwrap();
    assert!(heartbeat.task_lease_state(&lease).is_err());
    assert!(heartbeat.renew_task_lease(&f.cas, &lease, 30_000).is_err());
    assert_eq!(f.store.len(&run).unwrap(), prefix);
    assert!(heartbeat.task_lease_state(&successor).is_ok());
    // An expired lease is lost authority: the second connection cannot revive it either.
    let mut expired = Fixture::new(false);
    let short = expired
        .store
        .open_task(&expired.cas, &expired.revision_id, "writer-1", 1)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let mut late = expired
        .store
        .reopen(std::time::Duration::from_secs(1))
        .unwrap();
    assert!(late.task_lease_state(&short).is_err());
    assert!(late.renew_task_lease(&expired.cas, &short, 30_000).is_err());
    assert_eq!(expired.store.len(&run).unwrap(), 1);
}

/// Take the database's write lock on a connection of its own, as a Store operation in its
/// append transaction does.
fn write_lock(path: &std::path::Path) -> rusqlite::Connection {
    let lock = rusqlite::Connection::open(path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    lock
}

/// Release a held write lock from another thread once `until` has passed.
fn release_after(
    lock: rusqlite::Connection,
    until: std::time::Instant,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        std::thread::sleep(until.saturating_duration_since(std::time::Instant::now()));
        lock.execute_batch("ROLLBACK").unwrap();
    })
}

// Issue #134: rusqlite's implicit 5 s busy wait let a heartbeat renewal sit on a held write lock
// far past a lease's last seconds. The reopened connection waits only as long as its caller asks.
#[test]
fn a_reopened_connection_waits_its_bounded_busy_timeout_for_a_held_write_lock() {
    use std::time::{Duration, Instant};
    let mut f = Fixture::new(false);
    let lease = f.open();
    let run = task_run_id("task-1").unwrap();
    let prefix = f.store.len(&run).unwrap();
    let mut heartbeat = f.store.reopen(Duration::from_millis(300)).unwrap();
    let lock = write_lock(&f.path);
    // Observation never waits for the writer: the log is WAL.
    assert!(heartbeat.task_lease_state(&lease).is_ok());
    let started = Instant::now();
    assert!(
        heartbeat
            .renew_task_lease(&f.cas, &lease, 2_000_000)
            .is_err()
    );
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(250), "{waited:?}");
    assert!(waited < Duration::from_millis(2_500), "{waited:?}");
    assert_eq!(f.store.len(&run).unwrap(), prefix);
    // A lock released within the timeout is waited out, and the renewal lands.
    let mut patient = f.store.reopen(Duration::from_secs(5)).unwrap();
    let release = release_after(lock, Instant::now() + Duration::from_millis(300));
    let started = Instant::now();
    patient.renew_task_lease(&f.cas, &lease, 2_000_000).unwrap();
    assert!(started.elapsed() >= Duration::from_millis(250));
    release.join().unwrap();
    assert_eq!(f.store.len(&run).unwrap(), prefix + 1);
}

// A renewal checks expiry when it starts; one that then waits for the write lock past its
// lease's expiry is lost authority and must mint nothing when it finally gets the lock.
#[test]
fn a_renewal_that_waits_past_its_lease_for_the_write_lock_mints_nothing() {
    use std::time::{Duration, Instant};
    let mut f = Fixture::new(false);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer-1", 400)
        .unwrap();
    let until = f.store.task_lease_state(&lease).unwrap();
    let run = task_run_id("task-1").unwrap();
    let mut heartbeat = f.store.reopen(Duration::from_secs(5)).unwrap();
    let remaining = until.saturating_sub(now().unwrap());
    let release = release_after(
        write_lock(&f.path),
        Instant::now() + Duration::from_millis(remaining + 200),
    );
    assert!(heartbeat.renew_task_lease(&f.cas, &lease, 30_000).is_err());
    release.join().unwrap();
    assert_eq!(f.store.len(&run).unwrap(), 1, "no renewal after expiry");
    assert!(heartbeat.task_lease_state(&lease).is_err());
    f.store
        .take_task_lease(&f.cas, "task-1", "writer-2", 30_000)
        .unwrap();
}

#[test]
fn a_store_without_a_database_file_cannot_be_reopened() {
    let store = EventStore::open(":memory:").unwrap();
    assert!(store.reopen(std::time::Duration::from_secs(1)).is_err());
}

#[test]
fn heartbeat_projection_synthetic_history_benchmark() {
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    // Real protected writes; no fake payloads or local-time lease approximation.
    for index in 1..=128 {
        f.store
            .renew_task_lease(&f.cas, &lease, 1_000_000 + index * 1000)
            .unwrap();
    }
    let prefix = f.store.len(&task_run_id("task-1").unwrap()).unwrap();
    let started = std::time::Instant::now();
    for _ in 0..64 {
        assert!(f.store.task_lease_state(&lease).unwrap() > now().unwrap());
    }
    let elapsed = started.elapsed().as_micros();
    assert_eq!(
        f.store.len(&task_run_id("task-1").unwrap()).unwrap(),
        prefix
    );
    eprintln!(
        "heartbeat benchmark: events={prefix}, iterations=64, lease_only_us={elapsed}; synthetic single Task, no multi-Round performance claim"
    );
}

/// The race behind the second half of #134's failures: the work's operation reads the time,
/// its own heartbeat renews through its second connection a moment later, then the work
/// appends with the earlier time. The writer is the same and its lease is valid throughout.
#[test]
fn a_live_writer_append_timed_before_its_own_renewal_is_not_fenced() {
    let mut f = Fixture::new(false);
    let lease = f.open();
    f.propose(&lease);
    let before = now().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut own = f.store.reopen(std::time::Duration::from_secs(1)).unwrap();
    own.renew_task_lease(&f.cas, &lease, 100_000_000).unwrap();
    let result = f.store.task_change(
        &f.cas,
        &lease,
        TaskChangeV1::LeaseRenewed {
            lease_until_unix_ms: before + 200_000_000,
        },
        before,
    );
    assert!(result.is_ok(), "{result:?}");
    // It is recorded at the renewal's time, not before it: the log stays monotonic.
    let state = f.state();
    assert!(
        state.last_time > before,
        "stamped at the last recorded time"
    );
    // Another writer's stale time is still its own: a different epoch is fenced.
    let stale = TaskLease {
        epoch: lease.epoch + 1,
        ..lease.clone()
    };
    assert!(
        f.store
            .task_change(&f.cas, &stale, TaskChangeV1::Resumed {}, before)
            .is_err()
    );
}
