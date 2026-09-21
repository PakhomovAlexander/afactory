//! The real common CLI, native Codex framing and original writer recovery; no model calls.
use super::*;
use review_store::{Cas, EventStore};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        && stat
            .rsplit_once(')')
            .is_some_and(|(_, s)| s.trim_start().starts_with('Z'))
    {
        return false;
    }
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
        None,
    )
    .is_ok()
}

struct ChildGuard(Option<std::process::Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn heartbeat_failure_cancels_native_group_and_new_writer_recovers_exact_usage_without_repeating() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = native_diff_fixture(dir.path());
    let program = home.join("codex");
    let ready = home.join("native-ready");
    let calls = home.join("native-calls");
    let old = std::fs::read_to_string(&program).unwrap();
    let marker = "input=$(cat)\n";
    assert_eq!(old.matches(marker).count(), 1);
    let usage = format!(
        "{}\n{}\n{}\n",
        serde_json::json!({"type":"turn.completed","usage":{"input_tokens":u64::MAX,"output_tokens":20}}),
        serde_json::json!({"type":"turn.completed","usage":{"input_tokens":7,"output_tokens":3}}),
        serde_json::json!({"type":"turn.completed","usage":{"input_tokens":"unknown","output_tokens":5}})
    );
    let raw_usage = usage.clone();
    let quote = |p: &Path| p.to_str().unwrap().replace('\'', "'\\''");
    let body = format!(
        "{marker}printf call >>'{}'\nsleep 60 & child=$!\nprintf '%s' '{}'\nprintf diagnostic >&2\nprintf '%s %s' $$ $child >'{}'\nwait\nexit 0\n",
        quote(&calls),
        usage,
        quote(&ready)
    );
    std::fs::write(&program, old.replacen(marker, &body, 1)).unwrap();
    let mut paths = vec![home.clone()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut child = ChildGuard(Some(
        Command::new(env!("CARGO_BIN_EXE_af"))
            .args([
                "review",
                "run",
                "--pipeline",
                PIPELINE,
                "--campaign",
                "heartbeat",
                "--state",
                &state,
                "--heavy",
                "--policy-rev",
                "HEAD",
                "--base",
                "HEAD^",
                "--candidate",
                "HEAD",
                "--provider",
                "reviewer=test-codex",
                "--json",
            ])
            .current_dir(&repo)
            .env("HOME", &home)
            .env("USER", "loop-test")
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env(
                "AF_PROVIDERS_FILE",
                home.join(".config/afactory/providers.toml"),
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let until = Instant::now() + Duration::from_secs(15);
    let pids = loop {
        if let Ok(s) = std::fs::read_to_string(&ready) {
            let p: Vec<u32> = s
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if p.len() == 2 {
                break p;
            }
        }
        if child.0.as_mut().unwrap().try_wait().unwrap().is_some() {
            let out = child.0.take().unwrap().wait_with_output().unwrap();
            panic!(
                "native did not start: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert!(Instant::now() < until, "native readiness deadline");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        pids.iter().all(|p| alive(*p)),
        "observe both live processes before fault"
    );
    let db = Path::new(&state).join("events.sqlite");
    let cas = Cas::open_existing(Path::new(&state).join("cas")).unwrap();
    let mut store = EventStore::open(&db).unwrap();
    let ids = store.task_ids(&cas).unwrap();
    assert_eq!(ids.len(), 1);
    let task_id = &ids[0];
    let before = store.task_projection(&cas, task_id).unwrap().unwrap();
    let run = review_store::store::task::task_run_id(task_id).unwrap();
    let sql = rusqlite::Connection::open(&db).unwrap();
    // A durable-write outage prevents renewal and settlement while sidecar retention remains
    // available. Only a later real lease takeover can recover the original Started Attempt.
    sql.execute_batch(&format!("CREATE TRIGGER heartbeat_write_outage BEFORE INSERT ON events WHEN NEW.run_id = '{run}' AND NEW.type LIKE 'TaskTransition@%' BEGIN SELECT RAISE(ABORT,'heartbeat write outage'); END")).unwrap();
    let fault = Instant::now();
    while child.0.as_mut().unwrap().try_wait().unwrap().is_none() {
        assert!(
            fault.elapsed() < Duration::from_secs(10),
            "heartbeat must interrupt before native timeout"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = child.0.take().unwrap().wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("heartbeat write outage"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read(&calls).unwrap(), b"call");
    let reap_until = Instant::now() + Duration::from_secs(2);
    while pids.iter().any(|p| alive(*p)) {
        assert!(
            Instant::now() < reap_until,
            "native owned group survived heartbeat cancellation"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(i32::try_from(pids[0]).unwrap()),
            None
        )
        .is_err(),
        "native leader must be reaped"
    );
    assert_eq!(
        cas.get(&review_store::canonical::blob_content_id(
            raw_usage.as_bytes()
        ))
        .unwrap(),
        raw_usage.as_bytes()
    );
    let walls = store.task_attempt_wall(&run).unwrap();
    assert_eq!(walls.len(), 1);
    let usage = walls[0].usage.as_ref().unwrap();
    assert_eq!(usage.input_tokens.unwrap().get(), u128::from(u64::MAX) + 7);
    assert_eq!(usage.output_tokens.unwrap().get(), 28);
    assert_eq!(usage.chargeable_tokens.get(), u128::from(u64::MAX) + 35);
    let observation = store
        .task_attempt_usage_observation(&run, &walls[0].attempt_id)
        .unwrap()
        .unwrap();
    assert!(!observation.charge_complete);
    assert_eq!(observation.reported_usage.as_ref(), Some(usage));
    assert_eq!(
        cas.get(&review_store::canonical::blob_content_id(b"diagnostic"))
            .unwrap(),
        b"diagnostic"
    );
    let waiting = store.task_projection(&cas, task_id).unwrap().unwrap();
    assert_eq!(waiting.revision.limits, before.revision.limits);
    assert_eq!(waiting.lease_until_unix_ms(), before.lease_until_unix_ms());
    let pending = waiting.execution.as_ref().unwrap();
    assert_eq!(pending.budget.begun_attempts(), 1);
    assert!(pending.pending_attempts().contains(&walls[0].attempt_id));
    assert_eq!(
        pending.invocations.len(),
        before.execution.as_ref().unwrap().invocations.len(),
        "no later invocation, including pure work"
    );
    assert!(
        store
            .take_task_lease(&cas, task_id, "recovery", 15_000)
            .is_err(),
        "outage is not takeover authority"
    );
    sql.execute_batch("DROP TRIGGER heartbeat_write_outage")
        .unwrap();
    while SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        <= u128::from(waiting.lease_until_unix_ms())
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let lease = store
        .take_task_lease(&cas, task_id, "recovery", 15_000)
        .unwrap();
    store.recover_task_attempts(&cas, &lease).unwrap();
    let prefix = store.len(&run).unwrap();
    store.recover_task_attempts(&cas, &lease).unwrap();
    assert_eq!(store.len(&run).unwrap(), prefix);
    store.release_task_lease(&cas, &lease).unwrap();
    drop(store);
    let store = EventStore::open(&db).unwrap();
    let recovered = store.task_projection(&cas, task_id).unwrap().unwrap();
    assert_eq!(recovered.revision.limits, before.revision.limits);
    let execution = recovered.execution.unwrap();
    assert!(execution.pending_attempts().is_empty());
    assert_eq!(
        execution.budget.committed_tokens(),
        u128::from(u64::MAX) + 35
    );
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert!(execution.budget.breached());
    drop(store);
    let _ = af(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "heartbeat",
            "--state",
            &state,
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD^",
            "--candidate",
            "HEAD",
            "--provider",
            "reviewer=test-codex",
            "--json",
        ],
    );
    assert_eq!(
        std::fs::read(&calls).unwrap(),
        b"call",
        "reopen cannot repeat the paid probe"
    );
}
