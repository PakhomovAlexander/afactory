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
