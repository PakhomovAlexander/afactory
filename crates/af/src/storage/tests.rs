//! The sweep over small real files in a private layout: never the machine's own directories.

use super::*;

const DAY: Duration = Duration::from_secs(86_400);
const CHUNK: usize = 256 * 1024;

struct Layout {
    _root: tempfile::TempDir,
    root: PathBuf,
    roots: Roots,
}

fn layout() -> Layout {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let roots = Roots {
        warm: base.join("cache/af/task-build-cache"),
        workspaces: base.join("cache/af/workspaces"),
        campaigns: base.join("state/af/review/campaigns"),
        local_reviews: base.join("state/af/review/local"),
        tasks: base.join("state/af/task/local"),
        registry: base.join("state/af/stores.toml"),
        installs: crate::selfmgmt::Paths {
            versions: base.join("data/af/versions"),
            bin: base.join("bin/af"),
            state_file: base.join("state/af/self.toml"),
            cache_file: base.join("cache/af/self/latest.toml"),
        },
    };
    Layout {
        _root: root,
        root: base,
        roots,
    }
}

fn policy(max_bytes: u64) -> StoragePolicy {
    StoragePolicy {
        max_bytes,
        min_free_bytes: 0,
        auto_gc: true,
        keep_days: 14,
        keep_tasks: 20,
        keep_campaigns: 20,
        keep_worker_transcripts: false,
        keep_gate_pull_requests: false,
    }
}

fn private_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Every file and directory below `path`, `path` included, last modified `age` ago.
fn age(path: &Path, age: Duration) {
    let when = SystemTime::now() - age;
    let mut stack = vec![path.to_path_buf()];
    let mut all = Vec::new();
    while let Some(next) = stack.pop() {
        if next.is_dir() {
            for entry in std::fs::read_dir(&next).unwrap() {
                stack.push(entry.unwrap().path());
            }
        }
        all.push(next);
    }
    // Deepest first, so setting a directory's time is not undone by touching its children.
    all.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for path in all {
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
}

fn hex(byte: char) -> String {
    byte.to_string().repeat(64)
}

fn warm_key(
    layout: &Layout,
    project: char,
    toolchain: char,
    bytes: usize,
    old: Duration,
) -> PathBuf {
    private_dir(&layout.roots.warm);
    let project = layout.roots.warm.join(hex(project));
    private_dir(&project);
    let key = project.join(hex(toolchain));
    private_dir(&key.join("cargo_target"));
    std::fs::write(key.join("cargo_target/blob"), vec![1_u8; bytes]).unwrap();
    std::fs::write(key.join(review_sandbox::KEY_LOCK), b"").unwrap();
    age(&key, old);
    key
}

fn workspace(layout: &Layout, id: char, old: Duration) -> PathBuf {
    let path = layout
        .roots
        .workspaces
        .join(id.to_string().repeat(review_core::WORKSPACE_ID_HEX_LEN));
    std::fs::create_dir_all(path.join("tree")).unwrap();
    std::fs::write(path.join("tree/file"), vec![2_u8; CHUNK]).unwrap();
    age(&path, old);
    path
}

fn campaign(layout: &Layout, id: char, old: Duration) -> PathBuf {
    let path = layout.roots.campaigns.join(format!("c-{}", hex(id)));
    std::fs::create_dir_all(&path).unwrap();
    EventStore::open(path.join("events.sqlite")).unwrap();
    Cas::open(path.join("cas")).unwrap();
    std::fs::write(path.join("filler"), vec![3_u8; CHUNK]).unwrap();
    age(&path, old);
    path
}

fn version(layout: &Layout, version: &str, old: Duration) -> PathBuf {
    let path = layout.roots.installs.versions.join(version);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("af"), vec![4_u8; CHUNK]).unwrap();
    std::fs::write(
        path.join("receipt.toml"),
        format!(
            "version = \"{version}\"\ntarget = \"{}\"\nsource = \"test\"\nasset = \"a\"\n\
             sha256 = \"0\"\nverified_by = \"sha256sums\"\ninstalled_at = \"2026-10-01T00:00:00Z\"\n",
            crate::selfmgmt::TARGET
        ),
    )
    .unwrap();
    age(&path, old);
    path
}

/// A Task Store whose log holds an event type this release does not know.
fn unreadable_store(layout: &Layout, name: &str, old: Duration) -> PathBuf {
    let path = layout.roots.tasks.join(name);
    std::fs::create_dir_all(&path).unwrap();
    EventStore::open(path.join("events.sqlite")).unwrap();
    let connection = rusqlite::Connection::open(path.join("events.sqlite")).unwrap();
    connection
        .execute(
            "INSERT INTO events (run_id, sequence, event_id, type, occurred_at, artifact_refs, \
             payload) VALUES ('task-x', 0, 'e-1', 'TaskFromTheFuture@9', '2026-01-01T00:00:00Z', \
             '[]', '{}')",
            [],
        )
        .unwrap();
    drop(connection);
    std::fs::write(path.join("filler"), vec![5_u8; 4096]).unwrap();
    age(&path, old);
    path
}

fn removed_paths(report: &SweepReport) -> Vec<PathBuf> {
    report
        .removals
        .iter()
        .map(|removal| removal.path.clone())
        .collect()
}

#[test]
fn the_budget_evicts_the_least_recently_used_first_and_stops_when_af_fits() {
    let layout = layout();
    let locked = warm_key(&layout, 'a', '1', CHUNK, 7 * DAY);
    let default = version(&layout, "0.9.0", 6 * DAY);
    let oldest = version(&layout, "0.9.1", 5 * DAY);
    let old_campaign = campaign(&layout, 'c', 4 * DAY);
    let old_key = warm_key(&layout, 'a', '2', CHUNK, 3 * DAY);
    let old_workspace = workspace(&layout, 'd', 2 * DAY);
    let newer_key = warm_key(&layout, 'b', '3', CHUNK, DAY);
    let fresh_key = warm_key(&layout, 'b', '4', 32 * 1024, Duration::from_secs(60));
    std::fs::create_dir_all(layout.roots.installs.bin.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(default.join("af"), &layout.roots.installs.bin).unwrap();
    // A check holds this key's lock, from another open file description.
    let held = nix::fcntl::Flock::lock(
        std::fs::File::open(locked.join(review_sandbox::KEY_LOCK)).unwrap(),
        nix::fcntl::FlockArg::LockExclusive,
    )
    .unwrap();

    let stock = inventory(&layout.roots, Detail::Tasks);
    let by_kind = bytes_by_kind(&stock);
    assert!(by_kind[&Kind::WarmKey] >= 3 * CHUNK as u64, "{by_kind:?}");
    assert_eq!(
        stock
            .entries
            .iter()
            .find(|entry| entry.path == locked)
            .and_then(|entry| entry.in_use.clone())
            .as_deref(),
        Some("a check holds its warm.lock")
    );
    assert_eq!(
        stock
            .entries
            .iter()
            .find(|entry| entry.path == default)
            .and_then(|entry| entry.in_use.clone())
            .as_deref(),
        Some("the default version")
    );

    // A preview names exactly what --apply then removes, oldest first, and removes nothing.
    let budget = policy(1024 * 1024);
    let preview = sweep(&layout.roots, &budget, false, false);
    assert_eq!(
        removed_paths(&preview),
        [
            oldest.clone(),
            old_campaign.clone(),
            old_key.clone(),
            old_workspace.clone()
        ]
    );
    assert!(
        preview
            .removals
            .iter()
            .all(|removal| removal.rule == Rule::Budget)
    );
    assert_eq!(preview.stop, Stop::Fits);
    assert!(oldest.exists() && old_key.exists());

    let applied = sweep(&layout.roots, &budget, false, true);
    assert_eq!(
        removed_paths(&applied),
        removed_paths(&preview),
        "{applied:?}"
    );
    assert!(applied.failures.is_empty(), "{:?}", applied.failures);
    assert_eq!(applied.stop, Stop::Fits);
    assert!(applied.total_after <= budget.max_bytes, "{applied:?}");
    for gone in [&oldest, &old_campaign, &old_key, &old_workspace] {
        assert!(!gone.exists(), "{}", gone.display());
    }
    // In use, the default version, used within the hour, or not needed to fit: all kept.
    for kept in [&locked, &default, &newer_key, &fresh_key] {
        assert!(kept.exists(), "{}", kept.display());
    }
    drop(held);
}

#[test]
fn over_the_budget_with_nothing_evictable_the_sweep_says_so_and_removes_nothing() {
    let layout = layout();
    let fresh = warm_key(&layout, 'a', '1', CHUNK, Duration::from_secs(10));
    let report = sweep(&layout.roots, &policy(1024), true, true);
    assert_eq!(report.stop, Stop::NothingEvictable);
    assert!(report.removals.is_empty());
    assert!(fresh.exists());
}

#[test]
fn a_store_this_release_cannot_read_is_removed_when_idle_and_kept_when_recent_or_locked() {
    let layout = layout();
    let idle = unreadable_store(&layout, "0000000000000001", 30 * DAY);
    let recent = unreadable_store(&layout, "0000000000000002", 2 * DAY);
    let locked = unreadable_store(&layout, "0000000000000003", 30 * DAY);
    std::fs::write(locked.join("writer.lock"), b"").unwrap();
    age(&locked, 30 * DAY);
    let held = nix::fcntl::Flock::lock(
        std::fs::File::open(locked.join("writer.lock")).unwrap(),
        nix::fcntl::FlockArg::LockExclusive,
    )
    .unwrap();

    let stock = inventory(&layout.roots, Detail::Tasks);
    let unreadable: Vec<&Entry> = stock
        .entries
        .iter()
        .filter(|entry| entry.kind == Kind::UnreadableStore)
        .collect();
    assert_eq!(unreadable.len(), 3);
    assert!(
        unreadable
            .iter()
            .all(|entry| entry.reason.as_deref().is_some_and(|reason| reason
                .contains(review_core::event::ANOTHER_RELEASE)
                && reason.contains("TaskFromTheFuture@9")))
    );

    let report = sweep(&layout.roots, &policy(u64::MAX), true, true);
    assert_eq!(removed_paths(&report), [idle.clone()]);
    assert_eq!(report.removals[0].rule, Rule::Collection);
    assert!(!idle.exists());
    assert!(recent.exists());
    assert!(locked.exists());
    drop(held);
}

#[test]
fn collection_keeps_the_newest_campaigns_and_takes_idle_ones_beyond_them() {
    let layout = layout();
    let newest = campaign(&layout, 'a', 40 * DAY);
    let older = campaign(&layout, 'b', 50 * DAY);
    let oldest_running = campaign(&layout, 'c', 60 * DAY);
    let mut keep_one = policy(u64::MAX);
    keep_one.keep_campaigns = 1;
    // The third is old but holds a running review's lock: never taken.
    let running = hold_running(&oldest_running).unwrap();
    let report = sweep(&layout.roots, &keep_one, true, true);
    assert_eq!(removed_paths(&report), [older.clone()]);
    assert!(newest.exists() && oldest_running.exists() && !older.exists());
    assert_eq!(
        campaign_in_use(&oldest_running).as_deref(),
        Some("a running `af review run` holds it")
    );
    drop(running);
    assert_eq!(campaign_in_use(&oldest_running), None);
    // Without collection only the budget acts, and it has room.
    let report = sweep(&layout.roots, &keep_one, false, true);
    assert!(report.removals.is_empty());
}

#[test]
fn a_registered_store_that_is_gone_is_dropped_and_one_that_is_there_is_visited() {
    let layout = layout();
    let elsewhere = layout.root.join("elsewhere");
    let gone = layout.root.join("gone");
    let present = unreadable_store(&layout, "0000000000000009", 30 * DAY);
    let moved = elsewhere.join("store");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::rename(&present, &moved).unwrap();
    let now = now_unix_ms();
    let mut text = "version = 1\n".to_string();
    for path in [&moved, &gone] {
        text.push_str(&format!(
            "\n[[store]]\npath = \"{}\"\nkind = \"task\"\nfirst_use_unix_ms = {now}\nlast_use_unix_ms = {now}\n",
            path.display(),
        ));
    }
    std::fs::create_dir_all(layout.roots.registry.parent().unwrap()).unwrap();
    std::fs::write(&layout.roots.registry, text).unwrap();
    assert_eq!(registry::read(&layout.roots.registry).unwrap().len(), 2);
    let preview = sweep(&layout.roots, &policy(u64::MAX), true, false);
    assert_eq!(preview.registry_dropped, [gone.clone()]);
    assert_eq!(removed_paths(&preview), [moved.clone()], "{preview:?}");
    assert_eq!(registry::read(&layout.roots.registry).unwrap().len(), 2);
    let applied = sweep(&layout.roots, &policy(u64::MAX), true, true);
    assert_eq!(applied.registry_dropped, [gone]);
    assert_eq!(removed_paths(&applied), [moved.clone()]);
    assert!(!moved.exists());
    // The next sweep drops the entry whose Store it just removed.
    let after = sweep(&layout.roots, &policy(u64::MAX), true, true);
    assert_eq!(after.registry_dropped, [moved]);
    assert!(registry::read(&layout.roots.registry).unwrap().is_empty());
}

#[test]
fn the_refusal_names_the_free_bytes_the_floor_af_storage_and_the_knob() {
    let message = refusal(3 << 30, 10 << 30, Path::new("/var/tmp"));
    assert!(message.starts_with("insufficient_disk: "), "{message}");
    for needle in [
        "3.0 GiB (3221225472 bytes)",
        "10.0 GiB (10737418240 bytes)",
        "/var/tmp",
        "`af storage`",
        "[storage] min_free_bytes",
        "AF_STORAGE__MIN_FREE_BYTES",
    ] {
        assert!(message.contains(needle), "{needle}: {message}");
    }
}
