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
        keep_surveys: 50,
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
    assert!(
        applied
            .total_after
            .is_some_and(|total| total <= budget.max_bytes),
        "{applied:?}"
    );
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
    let mut text = "version = 2\n".to_string();
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

/// A registered Store as the registry names it: `kind = "task"` at `path`.
fn register(layout: &Layout, paths: &[&Path]) {
    let now = now_unix_ms();
    let mut text = "version = 2\n".to_string();
    for path in paths {
        text.push_str(&format!(
            "\n[[store]]\npath = \"{}\"\nkind = \"task\"\nfirst_use_unix_ms = {now}\nlast_use_unix_ms = {now}\n",
            path.display(),
        ));
    }
    std::fs::create_dir_all(layout.roots.registry.parent().unwrap()).unwrap();
    std::fs::write(&layout.roots.registry, text).unwrap();
}

#[test]
fn a_registered_store_behind_a_replaced_ancestor_is_never_followed_into_a_decoy() {
    let layout = layout();
    // Registered at `elsewhere/project/store`; after registration `project` is swapped for a
    // link to a directory that holds a decoy Store under the same name.
    let registered = layout.root.join("elsewhere/project/store");
    let made = unreadable_store(&layout, "0000000000000007", 30 * DAY);
    std::fs::create_dir_all(registered.parent().unwrap()).unwrap();
    std::fs::rename(&made, &registered).unwrap();
    register(&layout, &[&registered]);
    let decoy_root = layout.root.join("decoy");
    let decoy = unreadable_store(&layout, "0000000000000008", 30 * DAY);
    std::fs::create_dir_all(&decoy_root).unwrap();
    std::fs::rename(&decoy, decoy_root.join("store")).unwrap();
    std::fs::rename(
        layout.root.join("elsewhere/project"),
        layout.root.join("elsewhere/moved"),
    )
    .unwrap();
    std::os::unix::fs::symlink(&decoy_root, layout.root.join("elsewhere/project")).unwrap();

    let report = sweep(&layout.roots, &policy(u64::MAX), true, true);
    // The planned entry stays in the document as failed, and freed nothing.
    assert!(
        report.removals.iter().all(|removal| removal.bytes == 0
            && removal
                .outcome
                .as_deref()
                .is_some_and(|outcome| outcome.starts_with("failed: "))),
        "{report:?}"
    );
    assert_eq!(report.total_after, Some(report.total_before));
    assert!(
        report.failures.iter().any(
            |failure| failure.contains(&registered.display().to_string())
                && failure.contains("symlink")
        ),
        "{:?}",
        report.failures
    );
    assert!(
        decoy_root.join("store/events.sqlite").is_file(),
        "the decoy stays"
    );
    assert!(
        layout
            .root
            .join("elsewhere/moved/store/events.sqlite")
            .is_file()
    );
}

#[test]
fn an_entry_that_changed_since_inventory_is_refused_and_reported() {
    let layout = layout();
    let measured = campaign(&layout, 'e', 40 * DAY);
    let stock = inventory(&layout.roots, Detail::Tasks);
    let entry = stock
        .entries
        .iter()
        .find(|entry| entry.path == measured)
        .unwrap()
        .clone();
    assert!(entry.identity.is_some());
    // Another directory now holds the name.
    std::fs::rename(&measured, layout.root.join("moved-campaign")).unwrap();
    std::fs::create_dir_all(measured.join("someone-elses")).unwrap();
    let error = evict(&layout.roots, &policy(u64::MAX), &stock, &entry)
        .unwrap_err()
        .reason;
    assert!(
        error.contains("not the directory that was measured"),
        "{error}"
    );
    assert!(measured.join("someone-elses").is_dir());
    assert!(layout.root.join("moved-campaign/events.sqlite").is_file());
}

#[test]
fn a_version_made_the_default_after_inventory_is_kept() {
    let layout = layout();
    let old = version(&layout, "0.9.3", 5 * DAY);
    let stock = inventory(&layout.roots, Detail::Tasks);
    let entry = stock
        .entries
        .iter()
        .find(|entry| entry.path == old)
        .unwrap()
        .clone();
    assert_eq!(entry.in_use, None, "evictable when measured");
    // `af self` makes it the default between inventory and eviction.
    std::fs::create_dir_all(layout.roots.installs.bin.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(old.join("af"), &layout.roots.installs.bin).unwrap();
    let error = evict(&layout.roots, &policy(1), &stock, &entry)
        .unwrap_err()
        .reason;
    assert!(error.contains("became the default version"), "{error}");
    assert!(old.join("af").is_file(), "kept");
    // A version that stays unprotected is removed under the same lock.
    let other = version(&layout, "0.9.4", 5 * DAY);
    let stock = inventory(&layout.roots, Detail::Tasks);
    let entry = stock
        .entries
        .iter()
        .find(|entry| entry.path == other)
        .unwrap();
    evict(&layout.roots, &policy(1), &stock, entry).unwrap();
    assert!(!other.exists());
}

#[test]
fn a_killed_installers_staging_is_counted_and_evicted_but_a_live_one_is_kept() {
    let layout = layout();
    // Named after a process id no process can have: its installer is gone.
    let staging = |name: &str| {
        let path = layout.roots.installs.versions.join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("af"), vec![6_u8; CHUNK]).unwrap();
        path
    };
    let abandoned = staging(".0.9.5.2147483647");
    let taken = staging(".0.9.6.2147483647");
    let stock = inventory(&layout.roots, Detail::Tasks);
    let entry = |path: &Path| {
        stock
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .unwrap()
            .clone()
    };
    let (abandoned_entry, taken_entry) = (entry(&abandoned), entry(&taken));
    assert_eq!(abandoned_entry.kind, Kind::Version);
    assert!(abandoned_entry.bytes >= CHUNK as u64, "its bytes count");
    assert_eq!(abandoned_entry.in_use, None);

    // An installer took the other one between inventory and eviction: it holds it locked.
    let held = std::fs::File::open(&taken).unwrap();
    rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let error = evict(&layout.roots, &policy(1), &stock, &taken_entry).unwrap_err();
    assert!(
        error.reason.contains("staged by a running installer"),
        "{}",
        error.reason
    );
    assert!(taken.join("af").is_file(), "kept");
    evict(&layout.roots, &policy(1), &stock, &abandoned_entry).unwrap();
    assert!(!abandoned.exists());
}

fn volume(path: &str, free: Result<u64, &str>) -> Volume {
    Volume {
        path: PathBuf::from(path),
        free: free.map_err(str::to_string),
    }
}

#[test]
fn the_free_disk_floor_fails_closed() {
    let floor = 10 << 30;
    let measured = |free| volume("/tmp", Ok(free));
    let unmeasurable = volume("/cache", Err("/cache: Permission denied (os error 13)"));
    assert_eq!(
        judge_floor(&[measured(11 << 30), measured(12 << 30)], floor),
        Ok(())
    );
    // A measured volume below the floor refuses even when the other cannot be measured.
    let below = judge_floor(&[measured(1 << 30), unmeasurable.clone()], floor).unwrap_err();
    assert!(
        below.starts_with(
            "insufficient_disk: 1.0 GiB (1073741824 bytes) free on the volume of /tmp"
        ),
        "{below}"
    );
    assert!(below.contains("Permission denied"), "{below}");
    // A volume that cannot be measured refuses, with its measurement error.
    let unknown = judge_floor(&[measured(11 << 30), unmeasurable], floor).unwrap_err();
    assert!(unknown.starts_with("insufficient_disk: "), "{unknown}");
    assert!(unknown.contains("cannot be measured"), "{unknown}");
    assert!(
        unknown.contains("/cache: Permission denied (os error 13)"),
        "{unknown}"
    );
    for needle in ["`af storage`", "AF_STORAGE__MIN_FREE_BYTES"] {
        assert!(unknown.contains(needle), "{needle}: {unknown}");
    }
    assert!(
        judge_floor(&[], floor).is_err(),
        "nothing measured is never enough"
    );
}

#[test]
fn a_volume_behind_an_unreadable_directory_cannot_be_measured() {
    if nix::unistd::geteuid().is_root() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let locked = root.path().join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let measured = measure_volume(&locked.join("cache"));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(measured.free.is_err(), "{measured:?}");
    // One not created yet is measured where it will be.
    let fresh = measure_volume(&root.path().join("not/yet/there"));
    assert!(fresh.free.is_ok(), "{fresh:?}");
    assert_eq!(fresh.path, root.path());
}

#[test]
fn claude_project_directories_are_removed_through_their_anchor_and_links_are_left() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("claude");
    let projects = config.join("projects");
    let ours = "-private-tmp-af-sandbox-1-abc";
    std::fs::create_dir_all(projects.join(ours)).unwrap();
    std::fs::write(projects.join(ours).join("session.jsonl"), b"{}\n").unwrap();
    std::fs::create_dir_all(projects.join("-")).unwrap();
    std::fs::write(projects.join("-/mine.jsonl"), b"keep\n").unwrap();
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("precious"), b"keep\n").unwrap();
    let linked = "-private-tmp-af-sandbox-2-def";
    std::os::unix::fs::symlink(&elsewhere, projects.join(linked)).unwrap();
    let unmeasured = "-private-tmp-af-sandbox-4-ghi";
    std::fs::create_dir_all(projects.join(unmeasured)).unwrap();
    let identity = review_sandbox::Identity::of(&projects.join(ours)).ok();
    // The link is measured as what its name holds, the link itself, and is still never followed.
    let link_identity = review_sandbox::Identity::of(&projects.join(linked)).ok();
    let (removed, failures) = remove_claude_projects(
        &config,
        &[
            (ours.to_string(), identity),
            (linked.to_string(), link_identity),
            ("-private-tmp-af-sandbox-3-gone".to_string(), None),
            (unmeasured.to_string(), None),
        ],
    );
    assert_eq!(removed, 1);
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(failures[0].contains(linked) && failures[0].contains("symlink"));
    assert!(failures[1].contains(unmeasured) && failures[1].contains("not measured"));
    assert!(
        projects.join(unmeasured).is_dir(),
        "an unmeasured directory is left"
    );
    assert!(!projects.join(ours).exists());
    assert!(projects.join("-/mine.jsonl").is_file());
    assert!(elsewhere.join("precious").is_file());
}

#[test]
fn a_sweep_becomes_a_bounded_task_observation_only_when_it_did_something() {
    let removal = |index: usize| Removal {
        kind: Kind::Task,
        path: PathBuf::from(format!("/state/store-{index}")),
        task_id: Some(format!("task-{index}")),
        bytes: 10,
        rule: Rule::Collection,
        total_after: Some(0),
        unreadable: None,
        outcome: None,
    };
    let report = |removals: Vec<Removal>, failures: Vec<String>, applied: bool| SweepReport {
        applied,
        collection: true,
        max_bytes: 100,
        total_before: 1000,
        total_after: Some(900),
        stop: Stop::NothingEvictable,
        removals,
        left: Vec::new(),
        gate_cleanups: Vec::new(),
        registry_dropped: Vec::new(),
        failures,
    };
    assert!(
        observation(&report(vec![], vec![], true)).is_none(),
        "nothing done"
    );
    assert!(
        observation(&report(vec![removal(0)], vec![], false)).is_none(),
        "a preview"
    );
    let many: Vec<Removal> = (0..300).map(removal).collect();
    let failures: Vec<String> = (0..70).map(|index| format!("failure {index}")).collect();
    let sweep = observation(&report(many, failures, true)).unwrap();
    sweep.validate().unwrap();
    assert_eq!(sweep.removals.len(), 256);
    assert_eq!(sweep.omitted_removals, 44);
    assert_eq!(sweep.failures.len(), 64);
    assert_eq!(sweep.omitted_failures, 6);
    assert_eq!(sweep.removals[0].task_id.as_deref(), Some("task-0"));
    // A path no record can name is counted, not listed.
    let mut odd = removal(1);
    odd.path = PathBuf::from("/state/line\nbreak");
    let sweep = observation(&report(vec![removal(0), odd], vec![], true)).unwrap();
    assert_eq!(sweep.removals.len(), 1);
    assert_eq!(sweep.omitted_removals, 1);
    // A Survey's removal, `unreadable` or not, has no kind in `af/TaskStorageSweep@1`: counted.
    let survey = |unreadable: Option<&str>| Removal {
        kind: Kind::Survey,
        path: PathBuf::from("/state/af/project/ab/cas/objects/cd/ef"),
        task_id: None,
        bytes: 4096,
        rule: Rule::Budget,
        total_after: Some(0),
        unreadable: unreadable.map(str::to_string),
        outcome: None,
    };
    let sweep = observation(&report(
        vec![
            removal(0),
            survey(None),
            survey(Some("its bytes do not match its digest")),
        ],
        vec![],
        true,
    ))
    .unwrap();
    sweep.validate().unwrap();
    assert_eq!(sweep.removals.len(), 1);
    assert_eq!(sweep.omitted_removals, 2);
    let only = observation(&report(vec![survey(None)], vec![], true)).unwrap();
    sweep.validate().unwrap();
    assert!(only.removals.is_empty());
    assert_eq!(only.omitted_removals, 1);
}

#[test]
fn a_store_still_being_created_is_no_collection_failure() {
    let layout = layout();
    // A Task Store another process is creating: its log exists, its CAS not yet.
    let creating = layout.root.join("elsewhere/creating");
    std::fs::create_dir_all(&creating).unwrap();
    EventStore::open(creating.join("events.sqlite")).unwrap();
    register(&layout, &[&creating]);
    let report = sweep(&layout.roots, &policy(u64::MAX), true, true);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert!(
        observation(&report).is_none(),
        "nothing to record on a Task"
    );
}

#[test]
fn a_removal_a_dead_process_left_claimed_is_finished_and_reported_by_the_sweep() {
    let layout = layout();
    let claim = layout.roots.campaigns.join(".af-removing-999999999-7-0");
    std::fs::create_dir_all(claim.join("cas")).unwrap();
    std::fs::write(claim.join("cas/object"), vec![1_u8; 64 * 1024]).unwrap();
    // The claim is in the snapshot: the preview plans it, and touches nothing.
    let previewed = sweep(&layout.roots, &policy(20 << 30), false, false);
    assert!(
        claim.join("cas/object").is_file(),
        "a preview removes nothing"
    );
    let report = sweep(&layout.roots, &policy(20 << 30), false, true);
    assert_eq!(
        serde_json::to_value(&previewed.removals).unwrap(),
        serde_json::to_value(&report.removals).unwrap(),
        "preview and apply list the same recovery entry with the same bytes"
    );
    assert_eq!(previewed.total_before, report.total_before);
    assert_eq!(previewed.total_after, report.total_after);
    assert!(!claim.exists(), "the claim is finished");
    let [removal] = report.removals.as_slice() else {
        panic!("one removal: {:?}", report.removals);
    };
    assert_eq!(removal.kind, Kind::Claim);
    assert_eq!(removal.rule, Rule::Recovery);
    assert_eq!(removal.path, claim);
    assert!(removal.bytes >= 64 * 1024, "{}", removal.bytes);
    assert!(
        report.total_before >= removal.bytes,
        "counted in the total it had"
    );
    let recorded = observation(&report)
        .unwrap_or_else(|| panic!("a sweep that removed something is recorded: {report:?}"));
    assert_eq!(recorded.removals.len(), 1);
}

/// A planned campaign whose directory is replaced between the plan and its removal, as another
/// process could: the applied sweep refuses it, keeps it in its document as failed with nothing
/// freed and the running total unchanged, deletes nothing of either directory, and the totals
/// it reports are what af then holds.
#[test]
fn a_planned_campaign_replaced_before_its_removal_stays_as_failed_with_the_total_unchanged() {
    let layout = layout();
    let replaced = campaign(&layout, 'e', 30 * DAY);
    let other = campaign(&layout, 'f', 31 * DAY);
    let mut keep_none = policy(20 << 30);
    keep_none.keep_campaigns = 0;
    let previewed = sweep(&layout.roots, &keep_none, true, false);
    assert_eq!(removed_paths(&previewed), [replaced.clone(), other.clone()]);

    // Just before its removal the campaign's directory is replaced by a new one holding the
    // same files: another directory, the same bytes.
    let aside = layout.root.join("aside");
    let target = replaced.clone();
    let swapped = std::rc::Rc::new(std::cell::Cell::new(false));
    let seen = std::rc::Rc::clone(&swapped);
    BEFORE_EXECUTE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |planned: &plan::Planned| {
            if planned.removal.path != target {
                return;
            }
            std::fs::rename(&target, &aside).unwrap();
            std::fs::create_dir(&target).unwrap();
            for entry in std::fs::read_dir(&aside).unwrap() {
                let name = entry.unwrap().file_name();
                std::fs::rename(aside.join(&name), target.join(&name)).unwrap();
            }
            std::fs::remove_dir(&aside).unwrap();
            seen.set(true);
        }));
    });
    let applied = sweep(&layout.roots, &keep_none, true, true);
    BEFORE_EXECUTE.with(|hook| *hook.borrow_mut() = None);
    assert!(swapped.get());

    assert_eq!(
        removed_paths(&applied),
        removed_paths(&previewed),
        "the same plan"
    );
    let [failed, done] = applied.removals.as_slice() else {
        panic!("{applied:?}");
    };
    assert_eq!(failed.kind, Kind::Campaign);
    assert_eq!(failed.bytes, 0);
    let outcome = failed.outcome.as_deref().unwrap();
    assert!(outcome.starts_with("failed: removing "), "{outcome}");
    assert!(
        outcome.contains("not the directory that was measured"),
        "{outcome}"
    );
    assert_eq!(
        failed.total_after,
        Some(applied.total_before),
        "unchanged by it"
    );
    assert!(
        applied
            .failures
            .iter()
            .any(|failure| outcome.ends_with(failure.as_str()))
    );
    assert_eq!(done.outcome, None);
    assert_eq!(done.total_after, Some(applied.total_before - done.bytes));
    assert_eq!(applied.total_after, done.total_after);
    assert!(
        replaced.join("events.sqlite").is_file(),
        "nothing of it is deleted"
    );
    assert!(!other.exists());
    // What the document says is left is what af holds.
    assert_eq!(
        Some(inventory(&layout.roots, Detail::Stores).total()),
        applied.total_after
    );
    // Not recorded as a removal on a Task: it freed nothing.
    let recorded = observation(&applied).unwrap();
    assert_eq!(recorded.removals.len(), 1);
}

/// A planned campaign that grew between the plan and its removal, as another process could make
/// it: a large file, and a child deeper than the removal walk. The walk unlinks what it reaches
/// and leaves the rest under its claim. The applied sweep keeps the entry as failed with the
/// bytes that actually left the disk, accounted from what the campaign held when its removal
/// started, never from what inventory measured: what grew since is counted when the walk
/// unlinked it. Every total counts regular files alone, so a directory's blocks (4096 bytes on
/// ext4, none on APFS) never enter either side, and the running total keeps exactly what the
/// claim still holds in place of what inventory measured.
#[test]
fn a_partial_deletion_reports_the_bytes_it_actually_freed() {
    let layout = layout();
    let target = campaign(&layout, 'd', 30 * DAY);
    for n in 0..16 {
        std::fs::write(target.join(format!("blob-{n:02}")), vec![6_u8; CHUNK]).unwrap();
    }
    std::fs::create_dir(target.join("grows")).unwrap();
    // The lock the removal holds exists already, so nothing but the hook changes the campaign.
    std::fs::write(target.join(RUNNING_LOCK), b"").unwrap();
    age(&target, 30 * DAY);
    let mut keep_none = policy(20 << 30);
    keep_none.keep_campaigns = 0;
    let previewed = sweep(&layout.roots, &keep_none, true, false);
    let [planned] = previewed.removals.as_slice() else {
        panic!("{previewed:?}");
    };
    assert_eq!(planned.path, target);
    let measured = planned.bytes;

    // Just before its removal the campaign grows: a large file, and a chain of directories, each
    // with an empty file of its own, deeper than `REMOVAL_MAX_DEPTH`.
    let grows = target.join("grows");
    let started = std::rc::Rc::new(std::cell::Cell::new(0_u64));
    let seen = std::rc::Rc::clone(&started);
    let at = target.clone();
    BEFORE_EXECUTE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |_: &plan::Planned| {
            std::fs::write(at.join("grown"), vec![8_u8; 4 * CHUNK]).unwrap();
            let mut level = grows.clone();
            for n in 0..=review_sandbox::REMOVAL_MAX_DEPTH + 1 {
                level = level.join("d");
                std::fs::create_dir(&level).unwrap();
                std::fs::write(level.join(format!("f{n}")), b"").unwrap();
            }
            seen.set(review_sandbox::storage::allocated_bytes(&at).unwrap());
        }));
    });
    let applied = sweep(&layout.roots, &keep_none, true, true);
    BEFORE_EXECUTE.with(|hook| *hook.borrow_mut() = None);
    let started = started.get();
    assert!(started >= measured + 4 * CHUNK as u64, "it grew: {started}");

    let [failed] = applied.removals.as_slice() else {
        panic!("{applied:?}");
    };
    let outcome = failed.outcome.as_deref().unwrap();
    assert!(outcome.starts_with("failed: removing "), "{outcome}");
    assert!(outcome.contains("only partly removed"), "{outcome}");
    assert!(!target.exists(), "claimed, never renamed back");
    let claim = only_claim(&layout.roots.campaigns);
    assert!(
        claim.join("grows/d").is_dir(),
        "the walk could not reach all of it"
    );
    // The files the claim still holds: the chain's own are empty, so none of their bytes.
    let remaining = review_sandbox::storage::allocated_bytes(&claim).unwrap();
    assert!(started >= remaining, "{started} {remaining}");
    assert_eq!(
        failed.bytes,
        started - remaining,
        "what the walk unlinked, accounted from the removal's own start"
    );
    assert_eq!(
        failed.total_after,
        Some(applied.total_before - measured + remaining),
        "what the claim still holds in place of what inventory measured"
    );
    assert_eq!(applied.total_after, failed.total_after);
    // The schema accepts a failed entry that freed bytes.
    prune_document(&applied);
    let line = removal_line(failed, true);
    assert!(line.contains("partly removed"), "{line}");
    // Left for the walk to finish once this process is gone; here, the test's own cleanup.
    std::fs::remove_dir_all(claim).unwrap();
}

/// The one claim a removal that stopped part way left in `parent`.
fn only_claim(parent: &Path) -> PathBuf {
    let claims: Vec<PathBuf> = std::fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".af-removing-"))
        })
        .collect();
    let [claim] = claims.as_slice() else {
        panic!("one claim holds what is left: {claims:?}");
    };
    claim.clone()
}

/// A planned campaign whose children were all unlinked but whose final `rmdir` failed, since a
/// child appeared in its claim meanwhile: a partial removal too, its entry failed with the bytes
/// of the children it removed, never a failure that freed nothing.
#[cfg(debug_assertions)]
#[test]
fn a_failure_at_the_final_rmdir_is_a_partial_removal_with_what_it_freed() {
    let layout = layout();
    let target = campaign(&layout, 'c', 30 * DAY);
    std::fs::write(target.join(RUNNING_LOCK), b"").unwrap();
    age(&target, 30 * DAY);
    let mut keep_none = policy(20 << 30);
    keep_none.keep_campaigns = 0;
    let started = std::rc::Rc::new(std::cell::Cell::new(0_u64));
    let seen = std::rc::Rc::clone(&started);
    let at = target.clone();
    BEFORE_EXECUTE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |_: &plan::Planned| {
            seen.set(review_sandbox::storage::allocated_bytes(&at).unwrap());
        }));
    });
    let parent = layout.roots.campaigns.clone();
    review_sandbox::removal::set_before_final_unlink(Some(Box::new(move |claim: &str| {
        std::fs::write(parent.join(claim).join("late"), b"").unwrap();
    })));
    let applied = sweep(&layout.roots, &keep_none, true, true);
    review_sandbox::removal::set_before_final_unlink(None);
    BEFORE_EXECUTE.with(|hook| *hook.borrow_mut() = None);

    let [failed] = applied.removals.as_slice() else {
        panic!("{applied:?}");
    };
    let outcome = failed.outcome.as_deref().unwrap();
    assert!(outcome.contains("removing its claim"), "{outcome}");
    let claim = only_claim(&layout.roots.campaigns);
    assert!(claim.join("late").is_file());
    let remaining = review_sandbox::storage::allocated_bytes(&claim).unwrap();
    assert_eq!(failed.bytes, started.get() - remaining);
    assert!(
        failed.bytes >= CHUNK as u64,
        "the children it removed: {}",
        failed.bytes
    );
    assert_eq!(
        failed.total_after,
        Some(applied.total_before - failed.bytes)
    );
    std::fs::remove_dir_all(claim).unwrap();
}

/// The applied document of `report`, `af/storage-prune@3`, validated against its schema.
fn prune_document(report: &SweepReport) -> serde_json::Value {
    let mut document = serde_json::to_value(report).unwrap();
    document["schema"] = serde_json::json!("af/storage-prune@3");
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../../schemas/storage-prune-v3.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&document)
        .map(|error| format!("{error} at {}", error.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{errors:?}: {document}");
    document
}

/// Whether this process is held to directory permissions: an unprivileged user cannot create a
/// file in a directory it may not write. Run as root, a permission fixture proves nothing and is
/// skipped.
fn enforced(root: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let probe = root.join("probe");
    std::fs::create_dir_all(&probe).unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o555)).unwrap();
    let enforced = std::fs::write(probe.join("file"), b"").is_err();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_dir_all(&probe).unwrap();
    enforced
}

const MIB: u64 = 1 << 20;

/// A claim a dead process left in the campaigns root, holding `free/` (`free` files of 1 MiB)
/// and `kept/` (`kept` files of 1 MiB).
fn dead_claim(layout: &Layout, name: &str, free: u64, kept: u64) -> PathBuf {
    let claim = layout.roots.campaigns.join(name);
    for (directory, files) in [("free", free), ("kept", kept)] {
        std::fs::create_dir_all(claim.join(directory)).unwrap();
        for n in 0..files {
            std::fs::write(
                claim.join(directory).join(format!("{n:02}")),
                vec![7_u8; MIB as usize],
            )
            .unwrap();
        }
    }
    claim
}

/// ADR-0158 clause 13's running total: a claim inventory measured at 10 MiB grows to 20 MiB
/// before its removal, and 12 MiB appear in it while its walk unlinks the 20, so its final
/// `rmdir` fails and the walk freed 8 MiB net of what it still holds. The entry reports the 8
/// MiB, and the total keeps the 12 MiB the claim retains in place of the 10 MiB inventory
/// measured: the difference first, then what left.
#[cfg(debug_assertions)]
#[test]
fn a_grown_claims_partial_deletion_reports_the_bytes_it_retains() {
    let layout = layout();
    let claim = dead_claim(&layout, ".af-removing-999999999-8-0", 8, 2);
    let inventoried = review_sandbox::storage::allocated_bytes(&claim).unwrap();
    assert_eq!(inventoried, 10 * MIB);
    let previewed = sweep(&layout.roots, &policy(20 << 30), false, false);
    let [planned] = previewed.removals.as_slice() else {
        panic!("{previewed:?}");
    };
    assert_eq!((planned.kind, planned.bytes), (Kind::Claim, inventoried));
    let at = claim.join("kept");
    BEFORE_EXECUTE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |_: &plan::Planned| {
            for n in 2..12 {
                std::fs::write(at.join(format!("{n:02}")), vec![8_u8; MIB as usize]).unwrap();
            }
        }));
    });
    let parent = layout.roots.campaigns.clone();
    review_sandbox::removal::set_before_final_unlink(Some(Box::new(move |claim: &str| {
        let late = parent.join(claim).join("late");
        std::fs::create_dir(&late).unwrap();
        for n in 0..12 {
            std::fs::write(late.join(format!("{n:02}")), vec![9_u8; MIB as usize]).unwrap();
        }
    })));
    let applied = sweep(&layout.roots, &policy(20 << 30), false, true);
    review_sandbox::removal::set_before_final_unlink(None);
    BEFORE_EXECUTE.with(|hook| *hook.borrow_mut() = None);

    let [failed] = applied.removals.as_slice() else {
        panic!("{applied:?}");
    };
    let outcome = failed.outcome.as_deref().unwrap();
    assert!(outcome.contains("removing its claim"), "{outcome}");
    let claim = only_claim(&layout.roots.campaigns);
    let retained = review_sandbox::storage::allocated_bytes(&claim).unwrap();
    assert_eq!(retained, 12 * MIB);
    assert_eq!(
        failed.bytes,
        8 * MIB,
        "20 MiB at its start less the 12 it holds"
    );
    assert_eq!(
        failed.total_after,
        Some(applied.total_before - 10 * MIB + 12 * MIB),
        "the 12 MiB retained in place of the 10 MiB inventory measured"
    );
    assert_eq!(applied.total_after, failed.total_after);
    assert_eq!(applied.stop, Stop::Fits);
    prune_document(&applied);
    std::fs::remove_dir_all(claim).unwrap();
}

/// A claim whose remainder cannot be walked once its removal stopped part way leaves af's
/// total unknown: the entry and the document say `unknown`, `fits` is not claimed, the schema
/// accepts it, and no Task record claims a number.
#[test]
fn a_partial_deletion_whose_remainder_cannot_be_walked_reports_the_total_unknown() {
    use std::os::unix::fs::PermissionsExt;
    let layout = layout();
    if !enforced(&layout.root) {
        eprintln!("skipped: directory permissions are not enforced for this user");
        return;
    }
    let claim = dead_claim(&layout, ".af-removing-999999999-9-0", 2, 1);
    let sealed = claim.join("kept");
    let at = sealed.clone();
    BEFORE_EXECUTE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |_: &plan::Planned| {
            std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o000)).unwrap();
        }));
    });
    let applied = sweep(&layout.roots, &policy(20 << 30), false, true);
    BEFORE_EXECUTE.with(|hook| *hook.borrow_mut() = None);
    std::fs::set_permissions(
        only_claim(&layout.roots.campaigns).join("kept"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(!sealed.exists());

    let [failed] = applied.removals.as_slice() else {
        panic!("{applied:?}");
    };
    let outcome = failed.outcome.as_deref().unwrap();
    assert!(outcome.contains("cannot be measured"), "{outcome}");
    assert_eq!(failed.bytes, 0, "nothing is counted freed");
    assert_eq!(failed.total_after, None);
    assert_eq!(applied.total_after, None);
    assert_eq!(applied.stop, Stop::Unknown);
    let document = prune_document(&applied);
    assert_eq!(document["total_after"], "unknown");
    assert_eq!(document["stop"], "unknown");
    assert_eq!(document["removals"][0]["total_after"], "unknown");
    assert!(observation(&applied).is_none(), "no record claims a number");
    std::fs::remove_dir_all(only_claim(&layout.roots.campaigns)).unwrap();
}

/// A Project Store as a refresh leaves one, empty: its log, its CAS and its registration.
fn project_store(layout: &Layout) -> PathBuf {
    let store = layout.root.join("state/af/project").join(hex('a'));
    std::fs::create_dir_all(&store).unwrap();
    EventStore::open(store.join("events.sqlite")).unwrap();
    Cas::open(store.join("cas")).unwrap();
    std::fs::create_dir_all(store.join("cas/objects")).unwrap();
    let now = now_unix_ms();
    std::fs::create_dir_all(layout.roots.registry.parent().unwrap()).unwrap();
    std::fs::write(
        &layout.roots.registry,
        format!(
            "version = 2\n\n[[store]]\npath = \"{}\"\nkind = \"project\"\nfirst_use_unix_ms = \
             {now}\nlast_use_unix_ms = {now}\n",
            store.display()
        ),
    )
    .unwrap();
    store
}

/// A planned object gone since the inventory is a failed entry: an empty container of a dead
/// Survey collection removed by another process between inventory and its job is kept in the
/// applied document as `failed: … gone since the inventory`, nothing freed and the total
/// unchanged, never reported as removed.
#[test]
fn a_planned_container_gone_since_the_inventory_is_a_failed_entry() {
    let layout = layout();
    let store = project_store(&layout);
    let container = store.join("cas/removing/0123456789abcdef0123456789abcdef");
    std::fs::create_dir_all(&container).unwrap();
    let previewed = sweep(&layout.roots, &policy(20 << 30), true, false);
    let [planned] = previewed.removals.as_slice() else {
        panic!("{previewed:?}");
    };
    assert_eq!(planned.path, container);
    assert_eq!(planned.rule, Rule::Recovery);
    let target = container.clone();
    BEFORE_EXECUTE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |planned: &plan::Planned| {
            if planned.removal.path == target {
                std::fs::remove_dir(&target).unwrap();
            }
        }));
    });
    let applied = sweep(&layout.roots, &policy(20 << 30), true, true);
    BEFORE_EXECUTE.with(|hook| *hook.borrow_mut() = None);
    let [failed] = applied.removals.as_slice() else {
        panic!("{applied:?}");
    };
    assert_eq!(failed.path, container);
    assert_eq!(failed.bytes, 0);
    assert_eq!(
        failed.total_after,
        Some(applied.total_before),
        "unchanged by it"
    );
    let outcome = failed.outcome.as_deref().unwrap();
    assert!(outcome.starts_with("failed: finishing "), "{outcome}");
    assert!(outcome.contains("gone since the inventory"), "{outcome}");
    assert_eq!(applied.total_after, Some(applied.total_before));
}
