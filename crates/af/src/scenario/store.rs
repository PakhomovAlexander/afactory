//! A private, checksummed SQLite journal. Immediate transactions serialize tick admission;
//! separate per-Run OS locks serialize dispatch without blocking cancellation or other Runs.
use super::contract::*;
use super::schedule::AntState;
use super::state::Run;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct World {
    pub schema: String,
    pub writer_version: String,
    pub runs: BTreeMap<String, Run>,
    pub ants: BTreeMap<String, AntState>,
    pub roadmaps: BTreeMap<String, super::roadmap::Roadmap>,
}
impl World {
    fn empty() -> Self {
        Self {
            schema: "af/scenario-store@4".into(),
            writer_version: env!("CARGO_PKG_VERSION").into(),
            runs: BTreeMap::new(),
            ants: BTreeMap::new(),
            roadmaps: BTreeMap::new(),
        }
    }
    pub fn reconcile(&mut self) -> Result<()> {
        for roadmap in self.roadmaps.values_mut() {
            roadmap.reconcile(&self.runs)?;
        }
        Ok(())
    }
    pub fn admit_run(&mut self, mut run: Run) -> Result<Run> {
        require(
            !self.runs.contains_key(&run.id),
            "Run ID already exists; inspect or resume it",
        )?;
        if let Some(admission) = run.pin.bundle.definitions[&run.pin.bundle.root]
            .admission
            .clone()
        {
            let roadmap = self
                .roadmaps
                .get_mut(&admission.roadmap)
                .ok_or("Roadmap not found")?;
            if let Some(claim) = roadmap.admit(&run)? {
                run.goal_claim = Some(claim);
            } else {
                run.fail(
                    "No eligible complete goal fits all roadmap budgets; inspect deferred reasons"
                        .into(),
                );
            }
        }
        self.runs.insert(run.id.clone(), run.clone());
        Ok(run)
    }
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema == "af/scenario-store@4"
                && self.writer_version == env!("CARGO_PKG_VERSION"),
            "Scenario Store was written by another release or version",
        )?;
        let mut executions = std::collections::BTreeSet::new();
        for (id, run) in &self.runs {
            require(
                executions.insert(&run.execution_id),
                "Duplicate Scenario execution identity",
            )?;
            require(*id == run.id, "Run key differs")?;
            run.validate()?;
            if run.pin.bundle.definitions[&run.pin.bundle.root]
                .admission
                .is_some()
                && run.goal_claim.is_none()
            {
                require(
                    run.status == super::state::Status::Failed
                        && run.dispatches == 0
                        && run.task_charges.is_empty(),
                    "Roadmap Run dispatched without a complete-loop claim",
                )?;
            }
            if let Some(claim) = &run.goal_claim {
                require(
                    claim.run_id == run.id
                        && claim.execution_id == run.execution_id
                        && claim.started_unix_ms == run.created_unix_ms
                        && claim.settled_unix_ms.is_none(),
                    "Run claim identity changed",
                )?;
                let admission = run.pin.bundle.definitions[&run.pin.bundle.root]
                    .admission
                    .as_ref()
                    .ok_or("Goal claim without admission")?;
                let roadmap = self
                    .roadmaps
                    .get(&admission.roadmap)
                    .ok_or("Goal claim without roadmap")?;
                let native = roadmap
                    .progress
                    .get(&claim.goal_id)
                    .and_then(|p| p.claim.as_ref())
                    .ok_or("Run lost its goal claim")?;
                require(
                    native.run_id == run.id
                        && native.execution_id == run.execution_id
                        && native.revision == claim.revision
                        && admission.revision == claim.revision
                        && native.allowance == claim.allowance
                        && admission.identity == roadmap.identity,
                    "Run/goal claim identities differ",
                )?;
            }
            if let Some(ant_id) = &run.ant {
                let ant = self.ants.get(ant_id).ok_or("Scheduled Run has no Ant")?;
                require(ant.occurrences.iter().filter(|o| matches!(&o.outcome,super::schedule::OccurrenceOutcome::Started { run_id,.. } if run_id == id)).count() == 1,"Scheduled Run has no unique occurrence receipt")?;
            }
        }
        for (id, ant) in &self.ants {
            require(*id == ant.pin.definition.id, "Ant key differs")?;
            ant.validate(&self.runs)?;
        }
        for (id, roadmap) in &self.roadmaps {
            require(*id == roadmap.id, "Roadmap key differs")?;
            require(
                executions.insert(&roadmap.identity),
                "Duplicate roadmap identity",
            )?;
            roadmap.validate()?;
            let mut replay = roadmap.clone();
            replay.reconcile(&self.runs)?;
            require(
                replay == *roadmap,
                "Roadmap progress differs from recorded native Run evidence",
            )?;
        }
        Ok(())
    }
}
#[derive(Clone)]
pub(super) struct Store {
    path: PathBuf,
}
pub(super) struct RunGuard {
    _run: File,
    _store: nix::fcntl::Flock<File>,
}
impl Store {
    pub fn open(requested: Option<&Path>, create: bool) -> Result<Self> {
        Self::open_inner(requested, create, true)
    }
    #[cfg(test)]
    pub fn isolated(path: &Path) -> Result<Self> {
        Self::open_inner(Some(path), true, false)
    }
    fn open_inner(requested: Option<&Path>, create: bool, register: bool) -> Result<Self> {
        let path = requested
            .map(Path::to_path_buf)
            .unwrap_or(crate::xdg_state_root()?.join("af/scenarios"));
        require(
            path.is_absolute(),
            "Scenario state path must be absolute and outside the checkout",
        )?;
        let path = crate::resolve_filesystem_path(&path)?;
        require(
            !in_checkout(&path),
            "Scenario state must be outside a Git checkout",
        )?;
        if !path.exists() {
            require(create, "Scenario Store does not exist")?;
        }
        if !create {
            require(
                path.join("runs/scenario-owner").is_file()
                    && path.join("runs/scenarios.sqlite").is_file(),
                "Scenario journal missing; no state was created",
            )?;
        }
        // Use the existing owned Store infrastructure. The Scenario journal is runtime data
        // under runs/, so normal Store inventory counts every byte without introducing a
        // second machine budget or changing the Store registry's wire contract.
        crate::storage::owned::prepare(&path, true)?;
        let directory = path.join("runs");
        if !directory.exists() {
            require(create, "Scenario runtime directory missing")?;
        }
        if let Ok(meta) = directory.symlink_metadata() {
            require(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "Scenario runtime directory is a symlink or not a directory",
            )?;
        }
        crate::storage::owned::private_dir(&directory)?;
        let init_lock = review_store::local_fs::private_file_options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("scenario-init.lock"))
            .map_err(|e| e.to_string())?;
        fs2::FileExt::lock_exclusive(&init_lock).map_err(|e| e.to_string())?;
        let db = directory.join("scenarios.sqlite");
        let marker = directory.join("scenario-owner");
        for p in [
            &db,
            &directory.join("scenarios.sqlite-wal"),
            &directory.join("scenarios.sqlite-shm"),
        ] {
            if let Ok(m) = p.symlink_metadata() {
                require(
                    m.is_file() && !m.file_type().is_symlink(),
                    "Scenario Store contains a symlink or non-file",
                )?;
            }
        }
        if !marker.exists() {
            require(
                create && !db.exists(),
                "Scenario journal ownership is missing; refusing to recreate lost state",
            )?;
            // EventStore and CAS give this normal Store its release marker and reachability
            // infrastructure. Scenarios do not invent Tasks in that log.
            review_store::Cas::open(path.join("cas")).map_err(|e| e.to_string())?;
            review_store::EventStore::open(path.join("events.sqlite"))
                .map_err(|e| e.to_string())?;
            review_store::local_fs::create_private_file(&db).map_err(|e| e.to_string())?;
            let mut conn = Connection::open(&db).map_err(|e| e.to_string())?;
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .map_err(|e| e.to_string())?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
            tx.execute_batch("CREATE TABLE journal (id INTEGER PRIMARY KEY CHECK(id=1), document TEXT NOT NULL, digest TEXT NOT NULL);").map_err(|e| e.to_string())?;
            let world = World::empty();
            let document = serde_json::to_string(&world).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT INTO journal VALUES (1,?1,?2)",
                rusqlite::params![document, digest(document.as_bytes())],
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            let mut file = review_store::local_fs::private_file_options()
                .write(true)
                .create_new(true)
                .open(&marker)
                .map_err(|e| e.to_string())?;
            use std::io::Write;
            file.write_all(b"af/scenario-store@4\n")
                .and_then(|()| file.sync_all())
                .map_err(|e| e.to_string())?;
        }
        require(
            read(&marker, 128)? == b"af/scenario-store@4\n"
                && db.is_file()
                && path.join("events.sqlite").is_file(),
            "Invalid or incomplete Scenario Store; refusing to recreate lost state",
        )?;
        if register {
            crate::storage::registry::record(&path, crate::storage::registry::StoreKind::Task);
        }
        let store = Self { path };
        store.read()?;
        Ok(store)
    }
    fn connection(&self) -> Result<Connection> {
        let c = Connection::open_with_flags(
            self.path.join("runs/scenarios.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        c.pragma_update(None, "synchronous", "FULL")
            .map_err(|e| e.to_string())?;
        Ok(c)
    }
    fn load(c: &Connection) -> Result<World> {
        let (document, expected): (String, String) = c
            .query_row("SELECT document,digest FROM journal WHERE id=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(|e| format!("Corrupt Scenario journal: {e}"))?
            .ok_or("Missing Scenario journal record")?;
        require(
            document.len() as u64 <= MAX_BYTES && digest(document.as_bytes()) == expected,
            "Corrupt Scenario journal checksum or size",
        )?;
        let world: World =
            serde_json::from_str(&document).map_err(|e| format!("Corrupt Scenario state: {e}"))?;
        world.validate()?;
        Ok(world)
    }
    pub fn read(&self) -> Result<World> {
        Self::load(&self.connection()?)
    }
    pub fn update<T>(&self, action: impl FnOnce(&mut World) -> Result<T>) -> Result<T> {
        let mut c = self.connection()?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let mut world = Self::load(&tx)?;
        let old_roadmaps = world.roadmaps.clone();
        world.reconcile()?;
        let value = action(&mut world)?;
        world.reconcile()?;
        for (id, old) in &old_roadmaps {
            world
                .roadmaps
                .get(id)
                .ok_or("Roadmap history cannot be deleted")?
                .validate_revision_of(old)?;
        }
        for (id, roadmap) in &world.roadmaps {
            if !old_roadmaps.contains_key(id) {
                require(
                    roadmap.revisions.len() == 1
                        && roadmap
                            .progress
                            .values()
                            .all(|p| p.state == super::roadmap::GoalState::Pending),
                    "New roadmap must start pending without history",
                )?;
            }
        }
        world.validate()?;
        let document = serde_json::to_string(&world).map_err(|e| e.to_string())?;
        require(
            document.len() as u64 <= MAX_BYTES,
            "Scenario Store record budget exhausted",
        )?;
        tx.execute(
            "UPDATE journal SET document=?1,digest=?2 WHERE id=1",
            rusqlite::params![document, digest(document.as_bytes())],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(value)
    }
    pub fn lock_run(&self, id: &str) -> Result<RunGuard> {
        self.lock(id, "")
    }
    pub fn lock_cancel(&self, id: &str) -> Result<RunGuard> {
        self.lock(id, "-cancel")
    }
    fn lock(&self, id: &str, suffix: &str) -> Result<RunGuard> {
        name(id)?;
        let held = crate::storage::hold_running(&self.path)
            .map_err(|_| "Scenario Store is held for collection; dispatch refused")?;
        let file = review_store::local_fs::private_file_options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.join("runs").join(format!(
                "{}{}.lock",
                digest(id.as_bytes()).replace(':', "-"),
                suffix
            )))
            .map_err(|e| e.to_string())?;
        fs2::FileExt::try_lock_exclusive(&file)
            .map_err(|e| format!("Scenario Run is already executing: {e}"))?;
        Ok(RunGuard {
            _run: file,
            _store: held,
        })
    }
}
pub(super) fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    review_store::local_fs::read_bounded(path, limit)
        .map_err(|e| format!("Reading {}: {e}", path.display()))
}
pub(super) fn load<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&read(path, MAX_BYTES)?)
        .map_err(|e| format!("Reading {}: {e}", path.display()))
}
/// TOML is the human authoring format. JSON is the canonical versioned wire document, also
/// accepted explicitly by filename; a parse error never selects another format or version.
pub(super) fn definition<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    if path.extension().is_some_and(|s| s == "toml") {
        let bytes = read(path, MAX_BYTES)?;
        let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
        toml::from_str(text).map_err(|e| format!("Reading {}: {e}", path.display()))
    } else {
        load(path)
    }
}
pub(super) fn policy(path: &Path) -> Result<Policy> {
    use std::os::unix::fs::MetadataExt;
    let canonical = path.canonicalize().map_err(|e| e.to_string())?;
    let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
    require(
        !in_checkout(&canonical)
            && meta.is_file()
            && !meta.file_type().is_symlink()
            && meta.uid() == nix::unistd::geteuid().as_raw()
            && meta.mode() & 0o077 == 0,
        "Scenario authority must be an owner-only machine file outside the checkout",
    )?;
    let policy: Policy = definition(path)?;
    policy.validate()?;
    for adapter in policy.adapters.values() {
        let canonical = adapter
            .executable
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let m = adapter
            .executable
            .symlink_metadata()
            .map_err(|e| e.to_string())?;
        require(
            !in_checkout(&canonical)
                && m.is_file()
                && (m.uid() == nix::unistd::geteuid().as_raw() || m.uid() == 0)
                && !m.file_type().is_symlink()
                && m.mode() & 0o022 == 0,
            "Adapter must be a machine-owned executable outside the checkout",
        )?;
        require(
            digest(&read(&adapter.executable, MAX_BYTES)?) == adapter.digest,
            "Adapter executable digest differs from machine authority",
        )?;
    }
    Ok(policy)
}

/// Detect the containing checkout without running git or consulting candidate configuration.
/// A cron caller may run from HOME: its XDG state is valid unless HOME itself is a checkout.
fn in_checkout(path: &Path) -> bool {
    path.ancestors()
        .any(|p| p.join(".git").symlink_metadata().is_ok())
}
