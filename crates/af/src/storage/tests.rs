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
        telemetry: TelemetryRoots {
            registry: base.join("state/af/telemetry-dirs.v1.json"),
            current: Some(base.join("state/af/telemetry")),
            max_bytes: Some(4 << 30),
            state_root: base.join("state/af"),
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
        max_bytes_source: crate::config::MaxBytesSource::Set,
        min_free_bytes: 0,
        auto_gc: true,
        keep_days: 14,
        keep_tasks: 20,
        keep_campaigns: 20,
        keep_surveys: 50,
        keep_worker_transcripts: false,
        keep_gate_pull_requests: false,
        sandbox_root: None,
        build_cache_root: None,
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

/// Caches go before records (finding I9), least recently used first within each, and the
/// newest warm key of a project checked in the last 7 days stays (finding K18).
#[test]
fn the_budget_evicts_caches_before_records_least_recently_used_first_and_stops_when_af_fits() {
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

    // Project a's newest key (old_key, 3 days) and project b's (fresh_key) are kept.
    for newest in [&old_key, &fresh_key] {
        assert_eq!(
            stock
                .entries
                .iter()
                .find(|entry| entry.path == *newest)
                .and_then(|entry| entry.in_use.clone())
                .as_deref(),
            Some(NEWEST_WARM_KEY),
            "{}",
            newest.display()
        );
    }

    // A preview names exactly what --apply then removes, caches oldest first, then the campaign,
    // and removes nothing.
    let budget = policy(1024 * 1024);
    let preview = sweep(&layout.roots, &budget, false, false);
    assert_eq!(
        removed_paths(&preview),
        [
            oldest.clone(),
            old_workspace.clone(),
            newer_key.clone(),
            old_campaign.clone(),
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
    for gone in [&oldest, &old_campaign, &newer_key, &old_workspace] {
        assert!(!gone.exists(), "{}", gone.display());
    }
    // In use, the default version, a project's newest warm key, or used within the hour: all
    // kept.
    for kept in [&locked, &default, &old_key, &fresh_key] {
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
    // A Store af made where nothing was, holding only af's entries, goes whole.
    std::fs::remove_file(moved.join("filler")).unwrap();
    std::fs::write(moved.join(owned::MARKER), b"").unwrap();
    age(&moved, 30 * DAY);
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
fn the_refusal_names_the_volume_the_free_bytes_the_floor_af_storage_and_the_knob() {
    let message = refusal(&volume("/var/tmp", Ok(3 << 30)), 3 << 30, 10 << 30);
    assert!(
        message.starts_with("insufficient_disk: /var/tmp (tmpfs): "),
        "{message}"
    );
    for needle in [
        "3.0 GiB (3221225472 bytes) free",
        "floor 10.0 GiB (10737418240 bytes)",
        "the sandbox root is /var/tmp",
        "set TMPDIR or [storage] sandbox_root to a volume with room",
        "`af storage`",
        "[storage] min_free_bytes",
        "AF_STORAGE__MIN_FREE_BYTES",
    ] {
        assert!(message.contains(needle), "{needle}: {message}");
    }
    // Each volume says what af keeps there and how to move it; one whose mount is unknown is
    // named by the path measured.
    let cache = Volume {
        role: Role::Cache,
        root: PathBuf::from("/home/u/.cache"),
        mount: None,
        ..volume("/home/u", Ok(1))
    };
    let message = refusal(&cache, 1, 10 << 30);
    assert!(
        message.starts_with("insufficient_disk: /home/u: 1 B (1 bytes) free")
            && message.contains("the cache home is /home/u/.cache; set XDG_CACHE_HOME"),
        "{message}"
    );
    let build = Volume {
        role: Role::BuildCache,
        root: PathBuf::from("/srv/af-cache"),
        ..volume("/srv", Ok(1))
    };
    assert!(refusal(&build, 1, 10 << 30).contains(
        "the build cache root is /srv/af-cache; set [storage] build_cache_root to a volume \
             with room"
    ));
}

/// The floor measures the volumes af actually uses: the configured sandbox root, the cache
/// home and the configured build cache root, each named by its mount point and type when the
/// host says them (finding I17, T18).
#[test]
fn the_floor_measures_the_configured_sandbox_and_build_cache_roots() {
    let root = tempfile::tempdir().unwrap();
    let mut configured = policy(1);
    configured.sandbox_root = Some(root.path().join("sandboxes"));
    configured.build_cache_root = Some(root.path().join("warm"));
    let volumes = volumes_of(&configured);
    let roles: Vec<(Role, PathBuf)> = volumes
        .iter()
        .map(|volume| (volume.role, volume.root.clone()))
        .collect();
    assert_eq!(roles[0], (Role::Sandbox, root.path().join("sandboxes")));
    assert_eq!(roles[1].0, Role::Cache);
    assert_eq!(roles[2], (Role::BuildCache, root.path().join("warm")));
    for volume in &volumes {
        assert!(volume.free.is_ok(), "{volume:?}");
    }
    // A directory not made yet is measured at its nearest ancestor, and that volume's mount
    // point holds it.
    let (mount, kind) = volumes[0].mount.clone().expect("the host names its volume");
    assert!(
        root.path().starts_with(&mount)
            || std::fs::canonicalize(root.path())
                .unwrap()
                .starts_with(&mount),
        "{mount:?}"
    );
    assert!(!kind.is_empty());
    // Unset, the sandbox root is the temporary directory.
    assert_eq!(volumes_of(&policy(1))[0].root, std::env::temp_dir());
    assert_eq!(volumes_of(&policy(1)).len(), 2);
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
        role: Role::Sandbox,
        root: PathBuf::from(path),
        mount: Some((PathBuf::from(path), "tmpfs".into())),
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
            "insufficient_disk: /tmp (tmpfs): 1.0 GiB (1073741824 bytes) free, floor 10.0 GiB \
             (10737418240 bytes); the sandbox root is /tmp; set TMPDIR or [storage] sandbox_root \
             to a volume with room;"
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
        segment_id: None,
        directory: None,
        bytes: 10,
        rule: Rule::Collection,
        total_after: Some(0),
        unreadable: None,
        outcome: None,
        left: Vec::new(),
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
        telemetry: TelemetrySweep {
            max_bytes: None,
            total_before: 0,
            total_after: 0,
            stop: Stop::Fits,
            reclaimed: Vec::new(),
            notes: Vec::new(),
            registry_dropped: Vec::new(),
            changed: Vec::new(),
            problems: Vec::new(),
        },
        warm_fit: None,
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
    assert_eq!(
        sweep.freed_bytes, 3000,
        "every removal's bytes, listed or not"
    );
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
    // A Survey's removal, `unreadable` or not, has no kind in `af/TaskStorageSweep@2`: counted.
    let survey = |unreadable: Option<&str>| Removal {
        kind: Kind::Survey,
        path: PathBuf::from("/state/af/project/ab/cas/objects/cd/ef"),
        task_id: None,
        segment_id: None,
        directory: None,
        bytes: 4096,
        rule: Rule::Budget,
        total_after: Some(0),
        unreadable: unreadable.map(str::to_string),
        outcome: None,
        left: Vec::new(),
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
    let claim = dead_claim(&layout.roots.campaigns.join("campaign"));
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

// ------------------------------------------------------------------------------------------
// The telemetry kind (ADR-0153 clause 8, design §6 "Budget").

/// A sealed telemetry segment in `root`, its files last modified `old` ago; its id.
fn telemetry_segment(root: &Path, old: Duration) -> String {
    telemetry_segment_as(root, old, None)
}

/// [`telemetry_segment`] with the segment id `id` when given.
fn telemetry_segment_as(root: &Path, old: Duration, id: Option<&str>) -> String {
    use review_telemetry::record::{Attrs, SealExit, SpanKind, SpanStatus};
    let mut options = review_telemetry::segment::writer::WriterOptions::new(
        root,
        review_telemetry::level::Level::Spans,
    );
    options.redactor = review_telemetry::redact::Redactor::new();
    options.segment_id = id.map(str::to_string);
    // Sealed `old` ago on the writer's clock: a sealed segment's last use is its seal time.
    let when = SystemTime::now() - old;
    let at = when
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    options.clock = Some(std::sync::Arc::new(move || at));
    let writer = review_telemetry::segment::writer::Writer::new(options).unwrap();
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event(
        &span,
        "af.note",
        Attrs::new().text("filler", "x".repeat(900)),
    );
    writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    for path in [&sealed.path, &sealed.refs] {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
    sealed.segment_id
}

/// A corrupt segment in `root`, last modified `old` ago; its path.
fn corrupt_segment(root: &Path, id: &str, bytes: usize, old: Duration) -> PathBuf {
    let layout = review_telemetry::layout::Layout::new(root);
    std::fs::create_dir_all(layout.segments()).unwrap();
    let path = layout.corrupt_segment(id);
    std::fs::write(&path, vec![b'#'; bytes]).unwrap();
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(SystemTime::now() - old)
        .unwrap();
    path
}

fn telemetry_removals(report: &SweepReport) -> Vec<(String, Rule)> {
    report
        .removals
        .iter()
        .filter(|removal| removal.kind == Kind::Telemetry)
        .map(|removal| (removal.segment_id.clone().unwrap(), removal.rule))
        .collect()
}

/// ADR-0144's rules, unchanged for telemetry: least recently used first (a corrupt segment
/// first among equals), never one in use (a live writer's) or used within the last hour, and
/// by age only when collection is on (`auto_gc`, or an applied prune).
#[test]
fn telemetry_eviction_follows_storage_rules() {
    let layout = layout();
    let root = layout.roots.telemetry.current.clone().unwrap();
    let oldest = telemetry_segment(&root, 40 * DAY);
    let old = telemetry_segment(&root, 30 * DAY);
    let corrupt = corrupt_segment(&root, "1700000000000-9-0a1b2c3d", 4096, 30 * DAY);
    let recent = telemetry_segment(&root, Duration::from_secs(30 * 60));
    // A live writer holds its segment's lock.
    let live = review_telemetry::segment::writer::Writer::new(
        review_telemetry::segment::writer::WriterOptions::new(
            &root,
            review_telemetry::level::Level::Spans,
        ),
    )
    .unwrap();
    live.span_start(
        None,
        None,
        "af.command",
        review_telemetry::record::SpanKind::Internal,
        review_telemetry::record::Attrs::new(),
    );
    live.flush();
    let live_id = live.segment_id().unwrap();
    std::fs::File::open(live.path().unwrap())
        .unwrap()
        .set_modified(SystemTime::now() - 50 * DAY)
        .unwrap();

    let stock = inventory(&layout.roots, Detail::Stores);
    let entry = |id: &str| {
        stock
            .entries
            .iter()
            .find(|entry| entry.segment_id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("{id}: {:?}", stock.entries))
            .clone()
    };
    assert_eq!(
        entry(&live_id).in_use.as_deref(),
        Some("a writer holds its lock")
    );
    assert_eq!(entry(&live_id).state.as_deref(), Some("live"));
    assert_eq!(entry(&old).state.as_deref(), Some("sealed"));
    assert!(entry(&old).in_use.is_none());

    // With room, no collection: nothing goes, however old.
    let mut room = policy(u64::MAX);
    room.auto_gc = false;
    let kept = sweep(&layout.roots, &room, false, true);
    assert!(telemetry_removals(&kept).is_empty(), "{kept:?}");
    assert!(
        review_telemetry::layout::Layout::new(&root)
            .sealed_segment(&old)
            .exists()
    );

    // Over the budget: least recently used first, the corrupt segment before its equal, never
    // the live one or the one used within the hour.
    let corrupt_id = "1700000000000-9-0a1b2c3d".to_string();
    std::fs::File::open(&corrupt)
        .unwrap()
        .set_modified(
            std::fs::metadata(review_telemetry::layout::Layout::new(&root).sealed_segment(&old))
                .unwrap()
                .modified()
                .unwrap(),
        )
        .unwrap();
    let preview = sweep(&layout.roots, &policy(1), false, false);
    assert_eq!(
        telemetry_removals(&preview),
        [
            (oldest.clone(), Rule::Budget),
            (corrupt_id.clone(), Rule::Budget),
            (old.clone(), Rule::Budget),
        ]
    );
    assert_eq!(preview.stop, Stop::NothingEvictable);
    let applied = sweep(&layout.roots, &policy(1), false, true);
    assert_eq!(telemetry_removals(&applied), telemetry_removals(&preview));
    let layout_t = review_telemetry::layout::Layout::new(&root);
    assert!(!layout_t.sealed_segment(&oldest).exists() && !corrupt.exists());
    assert!(
        layout_t.sealed_segment(&recent).exists(),
        "used within the hour"
    );
    assert!(layout_t.live_segment(&live_id).exists(), "in use");

    // By age, only with collection on: an applied prune takes what is idle past keep_days.
    let aged = telemetry_segment(&root, 20 * DAY);
    let no_collection = sweep(&layout.roots, &room, false, true);
    assert!(telemetry_removals(&no_collection).is_empty());
    let collected = sweep(&layout.roots, &room, true, true);
    assert_eq!(
        telemetry_removals(&collected),
        [(aged.clone(), Rule::Collection)]
    );
    assert!(layout_t.sealed_segment(&recent).exists());
    live.finish(review_telemetry::record::SealExit::Ok).unwrap();
}

/// Two registered telemetry directories hold a segment of the same id: the copy in the
/// configured directory is idle past `keep_days`, the other one only two days old. A prune
/// preview collects the first copy by age and takes it out of its own directory's model only:
/// the other directory's model keeps its copy, so the telemetry total and the cap evaluation
/// see it, and the cap evicts it by the budget rule — as the applied prune does. A workspace
/// older than the second copy keeps the preview over the whole budget after the collection (so
/// its budget step runs) and is what the whole budget evicts, leaving the copy to the cap.
#[test]
fn telemetry_prune_preview_removes_a_segment_from_one_directory_only() {
    let mut layout = layout();
    let first = layout.roots.telemetry.current.clone().unwrap();
    let second = layout.root.join("elsewhere/telemetry");
    let id = "1700000000000-4242-0a1b2c3d";
    telemetry_segment_as(&first, 30 * DAY, Some(id));
    telemetry_segment_as(&second, 2 * DAY, Some(id));
    review_telemetry::dirs::register(&layout.roots.telemetry.registry, &first, &[]).unwrap();
    review_telemetry::dirs::register(&layout.roots.telemetry.registry, &second, &[]).unwrap();
    let canonical = |path: &Path| path.canonicalize().unwrap();
    let (first, second) = (canonical(&first), canonical(&second));
    let workspace = workspace(&layout, 'a', 10 * DAY);

    let stock = inventory(&layout.roots, Detail::Stores);
    let model = |root: &Path| stock.telemetry[root].clone().unwrap();
    let copy = |root: &Path| {
        stock
            .entries
            .iter()
            .find(|entry| {
                entry.directory.as_deref() == Some(root) && entry.segment_id.as_deref() == Some(id)
            })
            .unwrap_or_else(|| panic!("{}: {:?}", root.display(), stock.entries))
            .bytes
    };
    let (first_total, second_total) = (model(&first).total, model(&second).total);
    let (first_copy, second_copy) = (copy(&first), copy(&second));
    assert!(first_copy > 0 && second_copy > 0);
    // Telemetry without the collected copy stays one byte over the cap while the second copy is
    // counted, and fits once it is not: the cap evicts it only if its model still holds it.
    let rest = first_total - first_copy + second_total;
    layout.roots.telemetry.max_bytes = Some(rest - 1);
    // One byte over the whole budget once the collected copy is out of the count, so the
    // preview's budget step evicts the workspace, least recently used.
    let mut prune = policy(stock.total() - first_copy - 1);
    prune.auto_gc = false;

    let preview = sweep(&layout.roots, &prune, true, false);
    assert!(preview.failures.is_empty(), "{:?}", preview.failures);
    let removed = |report: &SweepReport| -> Vec<(PathBuf, String, Rule)> {
        report
            .removals
            .iter()
            .filter(|removal| removal.kind == Kind::Telemetry)
            .map(|removal| {
                (
                    removal.directory.clone().unwrap(),
                    removal.segment_id.clone().unwrap(),
                    removal.rule,
                )
            })
            .collect()
    };
    assert_eq!(
        removed(&preview),
        [
            (first.clone(), id.to_string(), Rule::Collection),
            (second.clone(), id.to_string(), Rule::Budget),
        ],
        "{preview:?}"
    );
    let bytes: Vec<u64> = preview
        .removals
        .iter()
        .filter(|removal| removal.kind == Kind::Telemetry)
        .map(|removal| removal.bytes)
        .collect();
    assert_eq!(bytes, [first_copy, second_copy]);
    let others: Vec<(PathBuf, Rule)> = preview
        .removals
        .iter()
        .filter(|removal| removal.kind != Kind::Telemetry)
        .map(|removal| (removal.path.clone(), removal.rule))
        .collect();
    assert_eq!(others, [(workspace.clone(), Rule::Budget)], "{preview:?}");
    assert_eq!(preview.telemetry.total_before, first_total + second_total);
    assert_eq!(
        preview.telemetry.total_after,
        first_total - first_copy + second_total - second_copy
    );
    assert_eq!(preview.telemetry.stop, Stop::Fits);
    for root in [&first, &second] {
        assert!(
            review_telemetry::layout::Layout::new(root)
                .sealed_segment(id)
                .exists(),
            "a preview removes nothing"
        );
    }

    let applied = sweep(&layout.roots, &prune, true, true);
    assert_eq!(removed(&applied), removed(&preview), "{applied:?}");
}

/// An inventory of only old corrupt segments, orphan temporaries and free index pages is
/// brought under `[telemetry] max_bytes`: the corrupt segments by eviction, the rest by every
/// applied sweep's reclamation, all measured as allocated bytes.
#[test]
fn telemetry_sweep_reclaims_every_class() {
    let mut layout = layout();
    let root = layout.roots.telemetry.current.clone().unwrap();
    let telemetry = review_telemetry::layout::Layout::new(&root);
    {
        let mut index = review_telemetry::index::Index::open(&telemetry).unwrap();
        index.ingest().unwrap();
        let connection = index.connection();
        connection
            .execute_batch("CREATE TABLE ballast(payload BLOB);")
            .unwrap();
        for _ in 0..24 {
            connection
                .execute("INSERT INTO ballast VALUES (zeroblob(1048576))", [])
                .unwrap();
        }
        connection.execute_batch("DROP TABLE ballast;").unwrap();
        let _ = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
    let corrupt = [
        corrupt_segment(&root, "1700000000000-1-0a1b2c3d", 64 * 1024, 10 * DAY),
        corrupt_segment(&root, "1700000000000-2-0a1b2c3d", 64 * 1024, 9 * DAY),
    ];
    std::fs::write(
        telemetry.segments().join(".tmp-write-1-0a1b2c3d"),
        vec![7_u8; 128 * 1024],
    )
    .unwrap();
    std::fs::create_dir_all(telemetry.blobs().join("ab")).unwrap();
    std::fs::write(
        telemetry
            .blobs()
            .join("ab")
            .join(format!("{}.gz", "ab".repeat(32))),
        vec![1_u8; 128 * 1024],
    )
    .unwrap();
    age(&root, 2 * DAY);
    let cap = 2 * 1024 * 1024;
    layout.roots.telemetry.max_bytes = Some(cap);
    let before = review_sandbox::storage::allocated_bytes(&root).unwrap();
    let mut room = policy(u64::MAX);
    room.auto_gc = false;
    let report = sweep(&layout.roots, &room, false, true);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let evicted: u64 = report
        .removals
        .iter()
        .filter(|removal| removal.kind == Kind::Telemetry)
        .map(|removal| removal.bytes)
        .sum();
    assert_eq!(telemetry_removals(&report).len(), 2, "{report:?}");
    assert!(corrupt.iter().all(|path| !path.exists()));
    let [reclaimed] = report.telemetry.reclaimed.as_slice() else {
        panic!("{report:?}");
    };
    assert!(reclaimed.temporary_bytes > 0 && reclaimed.blob_bytes > 0);
    assert_eq!(reclaimed.index, "vacuumed");
    let after = review_sandbox::storage::allocated_bytes(&root).unwrap();
    assert_eq!(
        before - after,
        evicted + reclaimed.temporary_bytes + reclaimed.blob_bytes + reclaimed.index_bytes,
        "{report:?}"
    );
    assert_eq!(report.telemetry.total_before, before);
    assert_eq!(report.telemetry.total_after, after);
    assert!(after <= cap, "{after} > {cap}");
}

/// A Store registered inside an existing telemetry directory: the directory is reported
/// `overlapping`, counted once (the Store by its own entries), and never evicted as telemetry;
/// a telemetry directory registered inside another is counted by the outer one.
#[test]
fn telemetry_dirs_overlap_reported_at_inventory() {
    let layout = layout();
    let outer = layout.root.join("elsewhere/telemetry");
    let old = telemetry_segment(&outer, 30 * DAY);
    review_telemetry::dirs::register(&layout.roots.telemetry.registry, &outer, &[]).unwrap();
    let inner = outer.join("nested");
    telemetry_segment(&inner, 30 * DAY);
    // Written into the registry by hand, as an edit would: registration itself refuses it.
    let mut registry: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&layout.roots.telemetry.registry).unwrap()).unwrap();
    registry["dirs"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "path": inner.canonicalize().unwrap(),
            "first_use_unix_ms": 1,
            "last_use_unix_ms": 1,
        }));
    std::fs::write(
        &layout.roots.telemetry.registry,
        serde_json::to_vec_pretty(&registry).unwrap(),
    )
    .unwrap();
    // A Store registered later inside the outer directory.
    let store = outer.join("store");
    std::fs::create_dir_all(&store).unwrap();
    EventStore::open(store.join("events.sqlite")).unwrap();
    Cas::open(store.join("cas")).unwrap();
    std::fs::write(store.join("filler"), vec![3_u8; CHUNK]).unwrap();
    register(&layout, &[&store.canonicalize().unwrap()]);

    let stock = inventory(&layout.roots, Detail::Stores);
    let telemetry: Vec<&Entry> = stock
        .entries
        .iter()
        .filter(|entry| entry.kind == Kind::Telemetry)
        .collect();
    assert_eq!(telemetry.len(), 2, "{telemetry:?}");
    for entry in &telemetry {
        assert_eq!(entry.state.as_deref(), Some("overlapping"), "{entry:?}");
        assert!(entry.in_use.is_some());
        assert!(
            entry
                .reason
                .as_deref()
                .unwrap()
                .starts_with("overlapping: ")
        );
    }
    let canonical = |path: &Path| path.canonicalize().unwrap();
    let outer_entry = telemetry
        .iter()
        .find(|entry| entry.path == canonical(&outer))
        .unwrap();
    let inner_entry = telemetry
        .iter()
        .find(|entry| entry.path == canonical(&inner))
        .unwrap();
    assert_eq!(inner_entry.bytes, 0, "the outer directory counts it");
    let store_bytes = review_sandbox::storage::allocated_bytes(&store).unwrap();
    assert_eq!(
        outer_entry.bytes,
        review_sandbox::storage::allocated_bytes(&outer).unwrap() - store_bytes,
        "counted once: the Store by its own entry"
    );
    // Never evicted as telemetry, however far over the budget.
    let report = sweep(&layout.roots, &policy(1), true, true);
    assert!(telemetry_removals(&report).is_empty(), "{report:?}");
    assert!(
        review_telemetry::layout::Layout::new(&outer)
            .sealed_segment(&old)
            .exists()
    );
}

/// `directory` with a `cas` in it, claimed as a removal by a process that has since died left
/// it: renamed to the claim name that binds its device and inode.
fn dead_claim(directory: &Path) -> PathBuf {
    std::fs::create_dir_all(directory.join("cas")).unwrap();
    let identity = review_sandbox::Identity::of(directory).unwrap();
    let claim = directory.with_file_name(review_sandbox::claim_name(999_999_999, identity));
    std::fs::rename(directory, &claim).unwrap();
    claim
}

/// A review Store registered outside af's roots (`--state`), as a run makes one; `marked` when
/// af created its directory, and `foreign` when the user's own files share it.
fn outside_store(layout: &Layout, name: &str, marked: bool, foreign: bool) -> PathBuf {
    let path = layout.root.join(name);
    std::fs::create_dir_all(path.join("git-home")).unwrap();
    EventStore::open(path.join("events.sqlite")).unwrap();
    Cas::open(path.join("cas")).unwrap();
    std::fs::write(path.join("cas/filler"), vec![3_u8; CHUNK]).unwrap();
    std::fs::write(path.join(RUNNING_LOCK), b"").unwrap();
    if marked {
        std::fs::write(path.join(owned::MARKER), b"").unwrap();
    }
    if foreign {
        std::fs::write(path.join("important.txt"), b"mine").unwrap();
        std::fs::create_dir_all(path.join("photos")).unwrap();
        std::fs::write(path.join("photos/1.jpg"), vec![9_u8; 8 * CHUNK]).unwrap();
    }
    age(&path, 3 * DAY);
    path
}

fn register_reviews(layout: &Layout, paths: &[&Path]) {
    register(layout, paths);
    let text = std::fs::read_to_string(&layout.roots.registry)
        .unwrap()
        .replace("kind = \"task\"", "kind = \"review\"");
    std::fs::write(&layout.roots.registry, text).unwrap();
}

/// H2 of the 2026-10-20 audit: a `--state` directory that also holds the user's files is
/// measured by af's entries alone, and the budget sweep takes af's entries out of it and leaves
/// the rest, naming it; a marked, af-only Store still goes whole.
#[test]
fn a_registered_store_holding_what_is_not_afs_keeps_it_through_the_budget_sweep() {
    let layout = layout();
    let mixed = outside_store(&layout, "mydocs", true, true);
    let unmarked = outside_store(&layout, "older", false, false);
    let whole = outside_store(&layout, "elsewhere-state", true, false);
    register_reviews(&layout, &[&mixed, &unmarked, &whole]);

    let stock = inventory(&layout.roots, Detail::Stores);
    let measured = stock
        .entries
        .iter()
        .find(|entry| entry.path == mixed)
        .unwrap();
    assert_eq!(measured.kind, Kind::Campaign);
    assert!(
        measured.bytes < 4 * CHUNK as u64,
        "the user's 2 MiB are not af's: {}",
        measured.bytes
    );

    let report = sweep(&layout.roots, &policy(1024), false, true);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let removal = |path: &Path| {
        report
            .removals
            .iter()
            .find(|removal| removal.path == path)
            .unwrap_or_else(|| panic!("{} in {report:?}", path.display()))
    };
    assert_eq!(
        removal(&mixed).left,
        [
            mixed.clone(),
            mixed.join("important.txt"),
            mixed.join("photos")
        ]
    );
    assert_eq!(std::fs::read(mixed.join("important.txt")).unwrap(), b"mine");
    assert!(mixed.join("photos/1.jpg").is_file());
    for gone in [
        "events.sqlite",
        "cas",
        "git-home",
        RUNNING_LOCK,
        owned::MARKER,
    ] {
        assert!(!mixed.join(gone).exists(), "{gone}");
    }
    assert_eq!(removal(&unmarked).left, [unmarked.clone()]);
    assert!(
        unmarked.is_dir(),
        "an unmarked directory is not proven af's"
    );
    assert!(
        removal(&whole).left.is_empty(),
        "{:?}",
        removal(&whole).left
    );
    assert!(!whole.exists(), "a marked, af-only Store goes whole");
}

/// `af self uninstall --purge` over the registered Stores: the same rule as the sweep.
#[test]
fn purge_removes_a_marked_af_only_store_whole_and_only_afs_entries_of_any_other() {
    let layout = layout();
    let mixed = outside_store(&layout, "mydocs", true, true);
    let whole = outside_store(&layout, "elsewhere-state", true, false);
    register_reviews(&layout, &[&mixed, &whole]);
    let lines = purge_registered(
        &layout.roots,
        &registry::read(&layout.roots.registry).unwrap(),
    );
    assert!(!whole.exists());
    assert!(
        lines.contains(&format!("removed {}", whole.display())),
        "{lines:?}"
    );
    assert!(mixed.join("important.txt").is_file());
    assert!(mixed.join("photos/1.jpg").is_file());
    assert!(!mixed.join("events.sqlite").exists() && !mixed.join("cas").exists());
    assert!(
        lines.iter().any(
            |line| line.contains("left the directory with what is not af's")
                && line.contains("important.txt")
        ),
        "{lines:?}"
    );
}

/// V1 of the security close-out: a Store entry is af's only by its exact name. A file named
/// like one of af's but not one (`notes.lock`, `backup.lock`, `events.sqlite.notes`, and a
/// claim-shaped `.af-removing-1-2-3` that binds no claim af made, audit C2) in a marked Store is
/// not measured, keeps the Store from going whole, and survives the budget sweep, `af storage
/// prune --apply` and `af self uninstall --purge`, each named as left.
#[test]
fn a_name_only_like_afs_survives_every_removal_and_is_not_measured() {
    for name in [
        "notes.lock",
        "backup.lock",
        "events.sqlite.notes",
        ".af-removing-1-2-3",
    ] {
        let user = |layout: &Layout, store: &str| {
            let path = outside_store(layout, store, true, false);
            std::fs::write(path.join(name), vec![7_u8; 4 * CHUNK]).unwrap();
            age(&path, 3 * DAY);
            path
        };
        let check = |path: &Path, left: &[PathBuf], how: &str| {
            assert_eq!(
                std::fs::read(path.join(name)).unwrap(),
                vec![7_u8; 4 * CHUNK],
                "{name} survives {how}"
            );
            assert_eq!(
                left,
                [path.to_path_buf(), path.join(name)],
                "{name} is left by {how}"
            );
            for gone in [
                "events.sqlite",
                "cas",
                "git-home",
                RUNNING_LOCK,
                owned::MARKER,
            ] {
                assert!(!path.join(gone).exists(), "{gone} after {how}");
            }
        };

        let first = layout();
        let swept = user(&first, "swept");
        let pruned = user(&first, "pruned");
        register_reviews(&first, &[&swept, &pruned]);
        let measured = owned::af_bytes(&swept).unwrap();
        assert!(
            measured < 2 * CHUNK as u64,
            "{name} is not measured: {measured}"
        );
        let stock = inventory(&first.roots, Detail::Stores);
        let entry = stock
            .entries
            .iter()
            .find(|entry| entry.path == swept)
            .unwrap();
        assert!(entry.bytes < 2 * CHUNK as u64, "{name}: {}", entry.bytes);

        // The budget sweep.
        let report = sweep(&first.roots, &policy(1024), false, true);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        let removal = |path: &Path| {
            report
                .removals
                .iter()
                .find(|removal| removal.path == path)
                .unwrap_or_else(|| panic!("{} in {report:?}", path.display()))
                .left
                .clone()
        };
        check(&swept, &removal(&swept), "the sweep");
        check(&pruned, &removal(&pruned), "the sweep");

        // `af storage prune --apply`: the same sweep with collection.
        let second = layout();
        let pruned = user(&second, "pruned");
        register_reviews(&second, &[&pruned]);
        let report = sweep(&second.roots, &policy(1024), true, true);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        let left = report
            .removals
            .iter()
            .find(|removal| removal.path == pruned)
            .unwrap_or_else(|| panic!("{} in {report:?}", pruned.display()))
            .left
            .clone();
        check(&pruned, &left, "prune --apply");

        // `af self uninstall --purge`.
        let third = layout();
        let purged = user(&third, "purged");
        register_reviews(&third, &[&purged]);
        let lines = purge_registered(
            &third.roots,
            &registry::read(&third.roots.registry).unwrap(),
        );
        assert!(purged.is_dir(), "{name} keeps the purged Store's directory");
        assert!(
            lines.iter().any(
                |line| line.contains("left the directory with what is not af's")
                    && line.contains(name)
            ),
            "{lines:?}"
        );
        let left: Vec<String> = std::fs::read_dir(&purged)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, [name], "only {name} is left by the purge");
    }
}

/// C2 of the security close-out: a claim af made in a Store, left by a removal a dead process
/// interrupted, is af's and goes with the Store; a user's claim-shaped file beside it is not af's,
/// is not measured, and keeps the Store's directory through the budget sweep, `af storage prune
/// --apply` and `af self uninstall --purge`.
#[test]
fn an_interrupted_claim_in_a_store_is_afs_and_a_claim_shaped_user_file_is_not() {
    let user = ".af-removing-1-2-3";
    let with_claim = |layout: &Layout, store: &str, foreign: bool| {
        let path = outside_store(layout, store, true, false);
        let claim = dead_claim(&path.join("git-home"));
        std::fs::write(claim.join("cas/object"), vec![1_u8; 4 * CHUNK]).unwrap();
        if foreign {
            std::fs::write(path.join(user), vec![7_u8; 4 * CHUNK]).unwrap();
        }
        age(&path, 3 * DAY);
        (path, claim)
    };
    let check = |path: &Path, claim: &Path, left: &[PathBuf], how: &str| {
        assert!(!claim.exists(), "af's claim is cleaned by {how}");
        assert_eq!(
            std::fs::read(path.join(user)).unwrap(),
            vec![7_u8; 4 * CHUNK],
            "{user} survives {how}"
        );
        assert_eq!(
            left,
            [path.to_path_buf(), path.join(user)],
            "{user} is left by {how}"
        );
    };

    let first = layout();
    let (swept, claim) = with_claim(&first, "swept", true);
    let (whole, whole_claim) = with_claim(&first, "whole", false);
    register_reviews(&first, &[&swept, &whole]);
    let without_user = owned::af_bytes(&whole).unwrap();
    assert!(without_user >= 4 * CHUNK as u64, "af's claim is measured");
    assert!(
        owned::af_bytes(&swept).unwrap() < without_user + CHUNK as u64,
        "{user} is not measured"
    );
    let report = sweep(&first.roots, &policy(1024), false, true);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let removal = |path: &Path| {
        report
            .removals
            .iter()
            .find(|removal| removal.path == path)
            .unwrap_or_else(|| panic!("{} in {report:?}", path.display()))
            .left
            .clone()
    };
    check(&swept, &claim, &removal(&swept), "the sweep");
    assert!(removal(&whole).is_empty(), "{:?}", removal(&whole));
    assert!(
        !whole.exists() && !whole_claim.exists(),
        "a marked Store holding af's own claim goes whole"
    );

    let second = layout();
    let (pruned, claim) = with_claim(&second, "pruned", true);
    register_reviews(&second, &[&pruned]);
    let report = sweep(&second.roots, &policy(1024), true, true);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let left = report
        .removals
        .iter()
        .find(|removal| removal.path == pruned)
        .unwrap_or_else(|| panic!("{} in {report:?}", pruned.display()))
        .left
        .clone();
    check(&pruned, &claim, &left, "prune --apply");

    let third = layout();
    let (purged, claim) = with_claim(&third, "purged", true);
    register_reviews(&third, &[&purged]);
    let lines = purge_registered(
        &third.roots,
        &registry::read(&third.roots.registry).unwrap(),
    );
    assert!(!claim.exists(), "af's claim is cleaned by the purge");
    assert!(purged.is_dir(), "{user} keeps the purged Store's directory");
    assert!(
        lines.iter().any(
            |line| line.contains("left the directory with what is not af's") && line.contains(user)
        ),
        "{lines:?}"
    );
    let left: Vec<String> = std::fs::read_dir(&purged)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, [user], "only {user} is left by the purge");
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
    assert_eq!(recorded.failures.len(), 1);
    assert_eq!(
        recorded.freed_bytes, done.bytes,
        "the failed one freed nothing"
    );
    assert_eq!(recorded.total_after, applied.total_after);
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

/// The applied document of `report`, `af/storage-prune@4`, validated against its schema.
fn prune_document(report: &SweepReport) -> serde_json::Value {
    let mut document = serde_json::to_value(report).unwrap();
    document["schema"] = serde_json::json!("af/storage-prune@4");
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../../schemas/storage-prune-v4.json")).unwrap();
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
/// and `kept/` (`kept` files of 1 MiB): named as af names a claim, binding its device and inode,
/// so af can prove it its own (ADR-0144).
fn dead_claim_holding(layout: &Layout, free: u64, kept: u64) -> PathBuf {
    let made = layout.roots.campaigns.join("claimed");
    for (directory, files) in [("free", free), ("kept", kept)] {
        std::fs::create_dir_all(made.join(directory)).unwrap();
        for n in 0..files {
            std::fs::write(
                made.join(directory).join(format!("{n:02}")),
                vec![7_u8; MIB as usize],
            )
            .unwrap();
        }
    }
    let identity = review_sandbox::Identity::of(&made).unwrap();
    let claim = made.with_file_name(review_sandbox::claim_name(999_999_999, identity));
    std::fs::rename(&made, &claim).unwrap();
    claim
}

/// ADR-0159 clause 13's running total: a claim inventory measured at 10 MiB grows to 20 MiB
/// before its removal, and 12 MiB appear in it while its walk unlinks the 20, so its final
/// `rmdir` fails and the walk freed 8 MiB net of what it still holds. The entry reports the 8
/// MiB, and the total keeps the 12 MiB the claim retains in place of the 10 MiB inventory
/// measured: the difference first, then what left.
#[cfg(debug_assertions)]
#[test]
fn a_grown_claims_partial_deletion_reports_the_bytes_it_retains() {
    let layout = layout();
    let claim = dead_claim_holding(&layout, 8, 2);
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
/// accepts it, and the Task record still says what failed and what left the disk, its total
/// after it and its stop `unknown`, never a number (ADR-0144 clause 4).
#[test]
fn a_partial_deletion_whose_remainder_cannot_be_walked_reports_the_total_unknown() {
    use std::os::unix::fs::PermissionsExt;
    let layout = layout();
    if !enforced(&layout.root) {
        eprintln!("skipped: directory permissions are not enforced for this user");
        return;
    }
    let claim = dead_claim_holding(&layout, 2, 1);
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
    let recorded = observation(&applied).expect("a sweep that failed is recorded");
    recorded.validate().unwrap();
    assert_eq!(recorded.total_after, None, "no record claims a number");
    assert_eq!(
        recorded.stop,
        review_core::task::storage_sweep::StorageSweepStopV2::Unknown
    );
    assert_eq!(recorded.failures.len(), 1);
    assert!(outcome.ends_with(recorded.failures[0].as_str()));
    assert_eq!(recorded.freed_bytes, 0, "what was measured to leave");
    let value = serde_json::to_value(&recorded).unwrap();
    assert_eq!(value["total_after"], "unknown");
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

/// A warm key that stands for `bytes` without holding them: its size record, stamped with its
/// lock's modification time as a check records it, is what inventory counts.
fn recorded_warm_key(layout: &Layout, project: char, toolchain: char, bytes: u64, old: Duration) {
    use std::os::unix::fs::MetadataExt;
    let key = warm_key(layout, project, toolchain, 16, old);
    let lock = std::fs::metadata(key.join(review_sandbox::KEY_LOCK)).unwrap();
    std::fs::write(
        key.with_file_name(format!("{}.size", hex(toolchain))),
        format!("{bytes} {} {}\n", lock.mtime(), lock.mtime_nsec()),
    )
    .unwrap();
}

/// Finding K18: the newest warm key of a project checked in the last 7 days is kept whatever the
/// budget; a project idle longer loses its keys like any cache.
#[test]
fn the_newest_warm_key_of_a_recent_project_stays_and_an_idle_projects_go() {
    let layout = layout();
    let idle_older = warm_key(&layout, 'a', '1', CHUNK, 9 * DAY);
    let idle_newest = warm_key(&layout, 'a', '2', CHUNK, 8 * DAY);
    let active_older = warm_key(&layout, 'b', '3', CHUNK, 6 * DAY);
    let active_newest = warm_key(&layout, 'b', '4', CHUNK, 2 * DAY);
    let applied = sweep(&layout.roots, &policy(1024), false, true);
    assert_eq!(
        removed_paths(&applied),
        [
            idle_older.clone(),
            idle_newest.clone(),
            active_older.clone()
        ]
    );
    assert_eq!(applied.stop, Stop::NothingEvictable);
    assert!(active_newest.exists());
    for gone in [&idle_older, &idle_newest, &active_older] {
        assert!(!gone.exists(), "{}", gone.display());
    }
}

/// Finding K18: a 16 GiB warm key against the 20 GiB budget is kept while its project is
/// active, never evicted while a check holds it, and the sweep says once that the budget is too
/// small for it, with the budget that fits.
#[test]
fn a_budget_smaller_than_its_warm_key_keeps_the_key_and_says_so() {
    let layout = layout();
    recorded_warm_key(&layout, 'a', '1', 16 << 30, 3 * DAY);
    let key = layout.roots.warm.join(hex('a')).join(hex('1'));
    let budget = policy(20 << 30);
    let report = sweep(&layout.roots, &budget, false, true);
    assert!(report.removals.is_empty(), "{:?}", report.removals);
    assert!(key.exists());
    let warning = report.warm_fit.clone().expect("the key does not fit");
    assert_eq!(
        warning,
        "warm key 16.0 GiB does not fit [storage] max_bytes 20.0 GiB: gates run cold whenever \
         the budget must evict it; fix: raise max_bytes to ≥ 24 GiB or lower [warm] max_bytes"
    );
    // Said once by a Task command, beside its one summary line.
    let mut tally = Tally::default();
    tally.add(&report);
    tally.add(&report);
    assert_eq!(tally.warm_fit.as_deref(), Some(warning.as_str()));
    assert_eq!(summary_line(&tally), None, "nothing freed, nothing over");
    // A budget with room for it says nothing.
    assert_eq!(
        warm_fit(24 << 30, &inventory(&layout.roots, Detail::Stores).entries),
        None
    );

    // Idle for over a week and held by a check: still never evicted while held.
    let layout = layout_with_idle_held_key();
    let report = sweep(&layout.0.roots, &policy(1), false, true);
    assert!(
        report
            .removals
            .iter()
            .all(|removal| removal.path != layout.1),
        "{:?}",
        report.removals
    );
    assert!(layout.1.exists());
    drop(layout.2);
}

fn layout_with_idle_held_key() -> (Layout, PathBuf, nix::fcntl::Flock<std::fs::File>) {
    let layout = layout();
    let key = warm_key(&layout, 'c', '5', CHUNK, 30 * DAY);
    let held = nix::fcntl::Flock::lock(
        std::fs::File::open(key.join(review_sandbox::KEY_LOCK)).unwrap(),
        nix::fcntl::FlockArg::LockExclusive,
    )
    .unwrap();
    (layout, key, held)
}

/// Finding C14: a Task command's sweeps say one line, naming what class was freed.
#[test]
fn the_summary_line_names_what_was_freed_and_what_is_left_to_do() {
    let removal = |kind: Kind, bytes: u64, outcome: Option<&str>| Removal {
        kind,
        path: PathBuf::from("/x"),
        task_id: None,
        segment_id: None,
        directory: None,
        bytes,
        rule: Rule::Budget,
        total_after: Some(0),
        unreadable: None,
        outcome: outcome.map(str::to_string),
        left: Vec::new(),
    };
    let mut report = sweep(&layout().roots, &policy(u64::MAX), false, false);
    report.removals = vec![
        removal(Kind::WarmKey, 1 << 30, None),
        removal(Kind::WarmKey, 1 << 29, None),
        removal(Kind::Task, 1 << 20, None),
    ];
    let mut tally = Tally::default();
    tally.add(&report);
    assert_eq!(
        summary_line(&tally).as_deref(),
        Some("storage: freed 1.5 GiB (2 warm keys, 1 finished Task)")
    );
    report.removals = vec![removal(Kind::Campaign, 0, Some("failed: held"))];
    report.stop = Stop::NothingEvictable;
    report.total_after = Some(30 << 30);
    report.max_bytes = 20 << 30;
    tally.add(&report);
    assert_eq!(
        summary_line(&tally).as_deref(),
        Some(
            "storage: freed 1.5 GiB (2 warm keys, 1 finished Task); 1 removal failed; af holds \
             30.0 GiB of a 20.0 GiB budget and nothing more may go now; see `af storage`"
        )
    );
    assert_eq!(summary_line(&Tally::default()), None, "no sweep, no line");
}

/// Issue #302: a Project Store another release wrote is never collected, with or without
/// Workspace directories, by the budget or by age.
#[test]
fn a_project_store_another_release_wrote_survives_a_sweep_under_pressure() {
    let layout = layout();
    let store = project_store(&layout);
    let connection = rusqlite::Connection::open(store.join("events.sqlite")).unwrap();
    connection
        .execute(
            "INSERT INTO events (run_id, sequence, event_id, type, occurred_at, artifact_refs, \
             payload) VALUES ('project', 0, 'e-1', 'ProjectFromTheFuture@9', \
             '2026-01-01T00:00:00Z', '[]', '{}')",
            [],
        )
        .unwrap();
    drop(connection);
    std::fs::write(store.join("filler"), vec![6_u8; CHUNK]).unwrap();
    assert!(!store.join("workspaces").exists());
    age(&store, 60 * DAY);
    let stock = inventory(&layout.roots, Detail::Tasks);
    let entry = stock
        .entries
        .iter()
        .find(|entry| entry.path == store)
        .expect("measured");
    assert_eq!(entry.kind, Kind::UnreadableStore);
    assert_eq!(entry.in_use.as_deref(), Some(OTHER_RELEASE_PROJECT_STORE));
    let applied = sweep(&layout.roots, &policy(0), true, true);
    assert!(
        applied.removals.iter().all(|removal| removal.path != store),
        "{:?}",
        applied.removals
    );
    assert!(store.join("events.sqlite").is_file());
}

/// Finding I9: a finished Task a delivered Workspace was made from is never the sweep's.
#[test]
fn a_task_behind_a_delivered_workspace_is_kept() {
    let task = |id: &str| Entry {
        kind: Kind::Task,
        path: PathBuf::from("/state/af/task/local/s"),
        task_id: Some(id.into()),
        bytes: 10,
        last_use_unix_ms: 0,
        in_use: None,
        reason: None,
        directory: None,
        segment_id: None,
        state: None,
        identity: None,
        found: None,
        too_deep: false,
        unmeasurable: None,
    };
    let mut entries = vec![task("delivered-1"), task("other")];
    keep_delivered(
        &mut entries,
        &BTreeSet::from(["delivered-1".to_string()]),
        None,
    );
    assert_eq!(
        entries[0].in_use.as_deref(),
        Some("Task delivered-1 is the source of a delivered Workspace")
    );
    assert_eq!(entries[1].in_use, None);

    // A Project Store that cannot be read could hold the delivered Workspace of any of them:
    // every finished Task is kept, and the reason names that Project Store.
    let delivered = Delivered {
        tasks: BTreeSet::new(),
        unknown: vec![
            (
                PathBuf::from("/state/af/project/aa"),
                "file is not a database".into(),
            ),
            (
                PathBuf::from("/state/af/project/bb"),
                "unknown event type".into(),
            ),
        ],
    };
    let why = delivered.blocked().unwrap();
    assert_eq!(
        why,
        "the delivered Workspaces of the Project Store /state/af/project/aa (and 1 more) cannot \
         be read, so no finished Task is collected: file is not a database"
    );
    let mut entries = vec![task("delivered-1"), task("other")];
    keep_delivered(&mut entries, &BTreeSet::new(), Some(&why));
    assert!(
        entries
            .iter()
            .all(|entry| entry.in_use.as_deref() == Some(why.as_str()))
    );
    assert_eq!(Delivered::default().blocked(), None);
}

/// A Project Store beyond the Store discovery ceiling, or one whose log does not replay, still
/// counts for delivered-Task protection: the inventory reads every one without the ceiling, and
/// one it cannot read is unknown rather than empty.
#[test]
fn every_project_store_counts_for_delivered_protection_whatever_the_discovery_ceiling() {
    let layout = layout();
    let state = layout.roots.registry.parent().unwrap().to_path_buf();
    let readable = state.join("project").join("a".repeat(64));
    std::fs::create_dir_all(&readable).unwrap();
    review_store::EventStore::open(readable.join("events.sqlite")).unwrap();
    let broken = state.join("project").join("b".repeat(64));
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("events.sqlite"), b"not a database").unwrap();
    let found = delivered(&layout.roots, &BTreeSet::new());
    assert!(found.tasks.is_empty());
    assert_eq!(found.unknown.len(), 1, "{:?}", found.unknown);
    assert_eq!(found.unknown[0].0, broken);
}

/// A review campaign is judged in use when inventory takes stock of it, before any plan: a run
/// holding it keeps it off the sweep's list (the same judgement names a live writer lease).
#[test]
fn a_campaign_in_use_is_never_on_the_sweeps_list() {
    let layout = layout();
    let held = campaign(&layout, 'e', 30 * DAY);
    let _running = hold_running(&held).unwrap();
    let stock = inventory(&layout.roots, Detail::Stores);
    let entry = stock
        .entries
        .iter()
        .find(|entry| entry.path == held)
        .unwrap();
    assert_eq!(
        entry.in_use.as_deref(),
        Some("a running `af review run` holds it")
    );
    let preview = sweep(&layout.roots, &policy(0), true, false);
    assert!(preview.removals.iter().all(|removal| removal.path != held));
}

/// C3 item 3: a campaign whose writer leases cannot be read is in use, never unused: a failed
/// read hides whatever lease a live writer holds. The sweep's judgement and `af review gc`'s
/// (`campaign_in_use`, `hold_campaign_for_removal`) both keep it, and say why.
#[test]
fn a_campaign_whose_leases_cannot_be_read_is_in_use() {
    let layout = layout();
    let fine = campaign(&layout, 'a', 30 * DAY);
    assert_eq!(campaign_in_use(&fine), None);
    drop(hold_campaign_for_removal(&fine).unwrap());

    // Its CAS cannot be opened: the leases its Tasks hold cannot be told.
    let broken_cas = campaign(&layout, 'b', 30 * DAY);
    std::fs::remove_dir_all(broken_cas.join("cas")).unwrap();
    std::fs::write(broken_cas.join("cas"), b"no CAS").unwrap();
    // Its log does not open as one.
    let broken_log = campaign(&layout, 'c', 30 * DAY);
    std::fs::write(broken_log.join("events.sqlite"), b"not a database").unwrap();
    for broken in [&broken_cas, &broken_log] {
        let why = campaign_in_use(broken).unwrap_or_else(|| panic!("{}", broken.display()));
        assert!(why.starts_with("its leases could not be read: "), "{why}");
        let refused = hold_campaign_for_removal(broken).unwrap_err();
        assert!(
            refused.starts_with("its leases could not be read: "),
            "{refused}"
        );
    }
    let applied = sweep(&layout.roots, &policy(0), true, true);
    let removed = removed_paths(&applied);
    assert!(removed.contains(&fine), "{removed:?}");
    assert!(
        !removed.contains(&broken_cas) && !removed.contains(&broken_log),
        "{removed:?}"
    );
    assert!(broken_cas.join("events.sqlite").is_file());
}

/// The sweep that ends a run warns about what it failed to do, never about a path the inventory
/// could not measure: that is a note of `af storage --verbose`.
#[test]
fn a_path_the_inventory_could_not_measure_is_no_warning_after_a_run() {
    let report = SweepReport {
        applied: true,
        collection: false,
        max_bytes: 100,
        total_before: 10,
        total_after: Some(10),
        stop: Stop::Fits,
        removals: Vec::new(),
        left: Vec::new(),
        gate_cleanups: Vec::new(),
        registry_dropped: Vec::new(),
        failures: vec![
            format!("{MEASURING}/tmp/.tmpXYZ/state: event store: no such table: events"),
            "removing /state/x: permission denied".into(),
        ],
        warm_fit: None,
        telemetry: TelemetrySweep {
            max_bytes: None,
            total_before: 0,
            total_after: 0,
            stop: Stop::Fits,
            reclaimed: Vec::new(),
            notes: Vec::new(),
            registry_dropped: Vec::new(),
            changed: Vec::new(),
            problems: Vec::new(),
        },
    };
    let warned: Vec<&String> = run_warnings(&report).collect();
    assert_eq!(warned, ["removing /state/x: permission denied"]);
}

/// ADR-0198: a warm key a check holds is never evicted. The sweep reads its lock at inventory
/// and again at eviction, so a key a check locked after the inventory measured it is refused and
/// left in place; an idle key the sweep evicts leaves its eviction record, the cause a check that
/// finds it gone reports.
#[test]
fn a_held_warm_key_is_refused_at_eviction_and_an_evicted_one_leaves_its_cause() {
    let layout = layout();
    let key = warm_key(&layout, 'a', '1', CHUNK, 30 * DAY);
    let _newest = warm_key(&layout, 'a', '2', CHUNK, Duration::from_secs(60));
    let stock = inventory(&layout.roots, Detail::Tasks);
    let entry = stock
        .entries
        .iter()
        .find(|entry| entry.path == key)
        .unwrap();
    assert_eq!(entry.in_use, None, "idle when the inventory measured it");
    let held = nix::fcntl::Flock::lock(
        std::fs::File::open(key.join(review_sandbox::KEY_LOCK)).unwrap(),
        nix::fcntl::FlockArg::LockExclusive,
    )
    .unwrap();
    let refused = evict(&layout.roots, &policy(1024), &stock, entry).unwrap_err();
    assert!(
        refused.reason.contains("a check holds it now"),
        "{}",
        refused.reason
    );
    assert!(
        key.join("cargo_target/blob").exists(),
        "a held key keeps every byte"
    );
    assert!(review_sandbox::read_key_eviction(&layout.roots.warm, &hex('a'), &hex('1')).is_none());
    drop(held);

    let (bytes, _, _) = evict(&layout.roots, &policy(1024), &stock, entry).unwrap();
    assert!(!key.exists());
    let record: review_core::task::check_layer::WarmEvictionV1 = serde_json::from_slice(
        &review_sandbox::read_key_eviction(&layout.roots.warm, &hex('a'), &hex('1')).unwrap(),
    )
    .unwrap();
    record.validate().unwrap();
    assert_eq!(
        record.by,
        review_core::task::check_layer::WarmEvictorV1::StorageSweep
    );
    assert_eq!(record.bytes, bytes);
    // The record sits beside the key, never below a key, so the next inventory lists no key
    // for it.
    let after = inventory(&layout.roots, Detail::Tasks);
    assert!(after.entries.iter().all(|entry| entry.path != key));
}
