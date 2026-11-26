//! Exercise the public CLI against the broker wire format and an authorized native Task Store.
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{common, schemas};

struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for name in ["config", "state", "home", "bin", "tmp", "cache", "data"] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn af(&self, args: &[&str]) -> Command {
        let mut command = common::af();
        let path = std::env::join_paths(
            std::iter::once(self.path("bin"))
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        command
            .current_dir(self.root.path())
            .env("HOME", self.path("home"))
            .env("AF_TEST_WAKE_ACCOUNT_HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("TMPDIR", self.path("tmp"))
            .env("PATH", path)
            .args(args);
        command
    }

    fn origin_reader(&self, project: &Value) -> Command {
        self.af(&[
            "cluster",
            "origin-read",
            "--scope",
            "fleet-fixture",
            "--origin",
            "project",
            "--project",
            project["project_id"].as_str().unwrap(),
            "--workspace",
            project["workspaces"][0]["label"].as_str().unwrap(),
            "--principal",
            "owner",
            "--allow-plan-decisions",
            "--state-home",
            path(&self.path("state")),
        ])
    }

    fn json(&self, args: &[&str]) -> Value {
        as_json(self.af(args).output().unwrap())
    }

    fn write(&self, name: &str, value: &Value) {
        fs::write(self.path(name), serde_json::to_vec(value).unwrap()).unwrap();
    }
}

fn success(output: Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn as_json(output: Output) -> Value {
    serde_json::from_str(&success(output)).unwrap()
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn require_failure(output: Output, message: &str) {
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{output:?}"
    );
}

fn sqlite_bytes(file: &Path) -> Vec<u8> {
    // POSIX byte locks belong to the process. Opening and closing these files here
    // would drop any locks SQLite is holding on the same files in this test process.
    let output = Command::new("/bin/cat").arg(file).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    output.stdout
}

fn sqlite_files(state: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = fs::read_dir(state)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().unwrap();
            name.starts_with("events.sqlite")
                .then(|| (name, sqlite_bytes(&entry.path())))
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn project_for(machine: &Machine, repo: &Path) -> Value {
    as_json(
        machine
            .af(&["project", "show", "--json"])
            .current_dir(repo)
            .output()
            .unwrap(),
    )
}

// Keep a read transaction on the planned state so the later waiting transition must
// be obtained from committed WAL frames, even if the Task writer checkpoints.
fn pin_planned_database(database: &Path) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .unwrap();
    connection
        .execute_batch("PRAGMA wal_autocheckpoint=0; BEGIN; SELECT count(*) FROM events;")
        .unwrap();
    connection
}

#[test]
fn fresh_install_and_offline_standalone() {
    let machine = Machine::new();
    let validator = schemas::validator("fleet-briefing-v3.json");
    let infra = machine.json(&["infra", "--json"]);
    schemas::valid(&validator, &infra);
    assert!(serde_json::from_value::<af_contracts::fleet::BriefingV3>(infra.clone()).is_ok());
    let mut invalid = infra.clone();
    invalid.as_object_mut().unwrap().remove("inventory");
    assert!(!validator.is_valid(&invalid));
    assert!(serde_json::from_value::<af_contracts::fleet::BriefingV3>(invalid).is_err());
    assert_eq!(infra["configured_contexts"], json!([]));
    assert_eq!(infra["discovery"], "unconfigured");
    assert_eq!(infra["next_action"]["action"], "ask_owner");
    assert!(
        infra["next_action"]["reason"]
            .as_str()
            .unwrap()
            .contains("credential reference")
    );
    let text = success(machine.af(&["infra"]).output().unwrap());
    assert!(text.contains("Next ask_owner"), "{text}");
    let cluster = machine.json(&["cluster", "--json"]);
    assert_eq!(cluster["cluster"], "unknown");
    assert_eq!(cluster["distributed_execution_ready"], false);
    let missing = machine.json(&["infra", "status", "--context", "unknown", "--json"]);
    assert_eq!(missing["context"], "unknown");
    assert!(missing["inventory"].is_null());
    assert!(machine.path("config").read_dir().unwrap().next().is_none());
    assert!(machine.path("state").read_dir().unwrap().next().is_none());

    let (repo, _) = crate::task_cli::fixture_named(machine.root.path(), "pagination");
    let planned = as_json(
        machine
            .af(&["task", "plan", "--file", "ticket.json", "--json"])
            .current_dir(&repo)
            .output()
            .unwrap(),
    );
    let shown = as_json(
        machine
            .af(&["task", "show", "pagination-cli", "--json"])
            .current_dir(&repo)
            .output()
            .unwrap(),
    );
    assert_eq!(shown["plan_id"], planned["plan_id"]);
    assert!(shown["plan_id"].is_string());
}

fn planned_origin(machine: &Machine) -> (Value, PathBuf) {
    let (repo, _, _) = crate::task_planning::setup(machine.root.path(), false, false);
    as_json(
        machine
            .af(&["task", "plan", "--file", "ticket.json", "--json"])
            .current_dir(&repo)
            .output()
            .unwrap(),
    );
    let project = project_for(machine, &repo);
    let directory = af_project::store_directory(
        &machine.path("state"),
        project["project_id"].as_str().unwrap(),
    );
    (project, directory.join("events.sqlite"))
}

fn origin_workspace_state(project: &Value) -> &Path {
    Path::new(project["workspaces"][0]["state"].as_str().unwrap())
}

fn origin_inventory_bytes(project: &Value, database: &Path) -> u64 {
    let store = database.parent().unwrap();
    let workspace = origin_workspace_state(project);
    let project_bytes = af_sandbox::storage::allocated_bytes(store).unwrap();
    let workspace_bytes = if workspace.starts_with(store) {
        0
    } else {
        af_sandbox::storage::allocated_bytes(workspace).unwrap()
    };
    project_bytes + workspace_bytes
}

fn origin_copy_sizes(machine: &Machine, project: &Value, database: &Path) -> (u64, u64) {
    let workspace = origin_workspace_state(project);
    let allocation_unit = destination_allocation_unit(machine);
    let allocated = |file: &Path| {
        let metadata = fs::metadata(file).unwrap();
        assert!(!file.with_file_name("events.sqlite-wal").exists());
        let rounded = metadata.len().next_multiple_of(allocation_unit);
        assert_eq!(metadata.blocks() * 512, rounded, "{}", file.display());
        rounded
    };
    (
        allocated(database),
        allocated(&workspace.join("events.sqlite")),
    )
}

fn destination_allocation_unit(machine: &Machine) -> u64 {
    let volume = nix::sys::statvfs::statvfs(&machine.path("tmp")).unwrap();
    #[allow(clippy::useless_conversion)]
    let unit = u64::from(volume.fragment_size());
    unit
}

fn require_snapshot_budget_refusal(output: Output, database: &Path) {
    assert!(output.stdout.is_empty(), "{output:?}");
    require_failure(
        output,
        &format!(
            "cannot observe {}: observation snapshot exceeds the available storage budget",
            database.display()
        ),
    );
}

fn use_small_sqlite_pages(database: &Path, unit: u64) {
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA page_size=1024; VACUUM;
             CREATE TABLE fixture_padding (bytes BLOB);",
        )
        .unwrap();
    let page_size: i64 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .unwrap();
    assert_eq!(page_size, 1024);
    if fs::metadata(database).unwrap().len() % unit == 0 {
        connection
            .execute_batch("CREATE TABLE fixture_extra_page (value INTEGER);")
            .unwrap();
    }
}

#[test]
fn origin_reserves_destination_allocation_before_copying_small_sqlite_pages() {
    let machine = Machine::new();
    let (project, database) = planned_origin(&machine);
    let unit = destination_allocation_unit(&machine);
    use_small_sqlite_pages(&database, unit);
    let logical = fs::metadata(&database).unwrap().len();
    let (allocated, task_copy) = origin_copy_sizes(&machine, &project, &database);
    let used = origin_inventory_bytes(&project, &database);
    let between = logical + (allocated - logical) / 2;
    assert!(logical < between && between < allocated);
    eprintln!(
        "Project logical={logical} allocated={allocated} unit={unit} inventory={used} \
         Workspace copy={task_copy} boundary={between}"
    );

    let command = |max: u64| {
        let mut reader = machine.origin_reader(&project);
        reader
            .env("AF_STORAGE__MAX_BYTES", format!("{max}B"))
            .env("AF_STORAGE__MIN_FREE_BYTES", "0B");
        reader
    };
    assert_eq!(
        as_json(command(used + allocated + task_copy).output().unwrap())["origin"]["tasks"],
        json!([])
    );
    let snapshots = af_sandbox::stale::observation_root(&machine.path("tmp"));
    let refused_by_max = command(used + between).output().unwrap();
    #[cfg(debug_assertions)]
    {
        let marker = machine.path("rounding-snapshot-ready");
        let mut reader = paused_reader(command(used + allocated + task_copy), &marker);
        assert_eq!(
            af_sandbox::storage::allocated_bytes(&snapshots).unwrap(),
            allocated
        );
        fs::remove_file(marker).unwrap();
        assert!(reader.0.take().unwrap().wait().unwrap().success());

        let free = allocated + task_copy + unit;
        let above_floor = |available| {
            command(u64::MAX)
                .env("AF_TEST_FREE_BYTES", free.to_string())
                .env(
                    "AF_STORAGE__MIN_FREE_BYTES",
                    format!("{}B", free - available),
                )
                .output()
                .unwrap()
        };
        assert_eq!(
            as_json(above_floor(allocated + task_copy))["origin"]["tasks"],
            json!([])
        );
        let refused_by_floor = above_floor(between);
        eprintln!(
            "max boundary status={} stderr={}\nfloor boundary status={} stderr={}",
            refused_by_max.status,
            String::from_utf8_lossy(&refused_by_max.stderr),
            refused_by_floor.status,
            String::from_utf8_lossy(&refused_by_floor.stderr)
        );
        require_snapshot_budget_refusal(refused_by_floor, &database);
    }
    require_snapshot_budget_refusal(refused_by_max, &database);
    assert_eq!(fs::read_dir(snapshots).unwrap().count(), 0);
}

#[cfg(debug_assertions)]
#[test]
fn origin_reserves_larger_clusters_before_copying() {
    let machine = Machine::new();
    let (project, database) = planned_origin(&machine);
    let fragment = destination_allocation_unit(&machine);
    let cluster = 65_536;
    use_small_sqlite_pages(&database, fragment);
    let logical = fs::metadata(&database).unwrap().len();
    let (allocated, workspace_allocated) = origin_copy_sizes(&machine, &project, &database);
    let workspace_logical = fs::metadata(origin_workspace_state(&project).join("events.sqlite"))
        .unwrap()
        .len();
    let inventory = origin_inventory_bytes(&project, &database);
    let project_reservation = logical.next_multiple_of(cluster);
    let workspace_reservation = workspace_logical.next_multiple_of(cluster);
    let sufficient = project_reservation + workspace_reservation;
    let boundary = allocated + (project_reservation - allocated) / 2;
    assert!(fragment < cluster && allocated < boundary && boundary < project_reservation);
    eprintln!(
        "Project logical={logical} allocated={allocated} Workspace logical={workspace_logical} \
         allocated={workspace_allocated} inventory={inventory} fragment={fragment} cluster={cluster} \
         Project reservation={project_reservation} Workspace reservation={workspace_reservation} \
         boundary={boundary} sufficient={sufficient}"
    );
    let command = |max: u64, free: u64, floor: u64| {
        let mut reader = machine.origin_reader(&project);
        reader
            .env("AF_TEST_OBSERVATION_ALLOCATION_UNIT", cluster.to_string())
            .env("AF_TEST_FREE_BYTES", free.to_string())
            .env("AF_STORAGE__MAX_BYTES", format!("{max}B"))
            .env("AF_STORAGE__MIN_FREE_BYTES", format!("{floor}B"));
        reader.output().unwrap()
    };
    let free = sufficient + cluster;
    let by_max = command(inventory + boundary, free, 0);
    let by_floor = command(u64::MAX, free, free - boundary);
    let control = command(inventory + sufficient, free, cluster);
    for (case, output) in [
        ("max", &by_max),
        ("floor", &by_floor),
        ("control", &control),
    ] {
        eprintln!(
            "{case} status={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    require_snapshot_budget_refusal(by_max, &database);
    require_snapshot_budget_refusal(by_floor, &database);
    assert_eq!(as_json(control)["origin"]["tasks"], json!([]));
    let snapshots = af_sandbox::stale::observation_root(&machine.path("tmp"));
    assert_eq!(fs::read_dir(snapshots).unwrap().count(), 0);
}

#[test]
fn origin_counts_telemetry_inside_a_registered_external_store_once() {
    let machine = Machine::new();
    let (project, database) = planned_origin(&machine);
    let external = machine.path("external-store");
    as_json(
        machine
            .af(&[
                "task",
                "plan",
                "--file",
                "ticket.json",
                "--state",
                path(&external),
                "--json",
            ])
            .current_dir(machine.path("repo"))
            .output()
            .unwrap(),
    );
    assert!(
        fs::read_to_string(machine.path("state/af/stores.toml"))
            .unwrap()
            .contains(path(&fs::canonicalize(&external).unwrap()))
    );
    let telemetry = external.join("cas/telemetry");
    fs::create_dir_all(&telemetry).unwrap();
    fs::write(telemetry.join("retained"), vec![1_u8; 1024 * 1024]).unwrap();

    let project_bytes = origin_inventory_bytes(&project, &database);
    let store_bytes = af_sandbox::storage::allocated_bytes(&external).unwrap();
    let telemetry_bytes = af_sandbox::storage::allocated_bytes(&telemetry).unwrap();
    let (project_copy, task_copy) = origin_copy_sizes(&machine, &project, &database);
    let exact = project_bytes + store_bytes + project_copy + task_copy;
    eprintln!(
        "Project and Workspace inventory={project_bytes} external Store inventory={store_bytes} \
         included telemetry={telemetry_bytes} Project copy={project_copy} \
         Workspace copy={task_copy} exact budget={exact}"
    );
    let foreign = external.join("unowned");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("retained"), vec![2_u8; 1024 * 1024]).unwrap();
    let command = |root: &Path, max: u64| {
        let mut reader = machine.origin_reader(&project);
        reader
            .env("AF_TELEMETRY__DIR", root)
            .env("AF_STORAGE__MAX_BYTES", format!("{max}B"))
            .env("AF_STORAGE__MIN_FREE_BYTES", "0B");
        reader.output().unwrap()
    };

    assert_eq!(
        as_json(command(&telemetry, exact + telemetry_bytes))["origin"]["tasks"],
        json!([])
    );
    assert_eq!(
        as_json(command(&telemetry, exact))["origin"]["tasks"],
        json!([])
    );
    let workspace = origin_workspace_state(&project);
    require_snapshot_budget_refusal(
        command(&telemetry, exact - 1),
        &workspace.join("events.sqlite"),
    );

    // A telemetry subtree below an unowned Store entry still needs its own charge.
    require_snapshot_budget_refusal(command(&foreign, exact), &database);
    let foreign_bytes = af_sandbox::storage::allocated_bytes(&foreign).unwrap();
    assert_eq!(
        as_json(command(&foreign, exact + foreign_bytes))["origin"]["tasks"],
        json!([])
    );
}

#[test]
fn origin_snapshots_share_the_machine_storage_budget() {
    const MIB: u64 = 1024 * 1024;
    let machine = Machine::new();
    let (project, database) = planned_origin(&machine);
    // Make the Project copy large enough to distinguish one live reader from two, with
    // slack for allocation rounding and for the much smaller Workspace Task snapshot.
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch(
            "CREATE TABLE fixture_padding (bytes BLOB);
             INSERT INTO fixture_padding VALUES (zeroblob(3145728));
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();

    let snapshots = af_sandbox::stale::observation_root(&machine.path("tmp"));
    let baseline = af_sandbox::storage::allocated_bytes(&machine.path("state/af")).unwrap();
    let limit = format!("{}B", baseline + 4 * MIB);
    let command = || {
        let mut reader = machine.origin_reader(&project);
        reader
            .env("AF_STORAGE__MAX_BYTES", &limit)
            .env("AF_STORAGE__MIN_FREE_BYTES", "0B");
        reader
    };
    assert_eq!(
        as_json(command().output().unwrap())["origin"]["tasks"],
        json!([])
    );

    let unrelated = machine.path("state/af/fleet-cache/unrelated");
    fs::create_dir_all(&unrelated).unwrap();
    fs::write(unrelated.join("retained"), vec![1_u8; (3 * MIB) as usize]).unwrap();
    assert!(af_sandbox::storage::free_bytes(&snapshots).unwrap() > 16 * MIB);
    let refused = command().output().unwrap();
    assert!(refused.stdout.is_empty());
    require_failure(
        refused,
        "observation snapshot exceeds the available storage budget",
    );
    assert_eq!(fs::read_dir(&snapshots).unwrap().count(), 0);
    fs::remove_dir_all(unrelated).unwrap();

    require_failure(
        command()
            .env("AF_STORAGE__MIN_FREE_BYTES", format!("{}B", u64::MAX))
            .output()
            .unwrap(),
        "observation snapshot exceeds the available storage budget",
    );
    assert_eq!(fs::read_dir(&snapshots).unwrap().count(), 0);

    let registry = machine.path("state/af/stores.toml");
    let registered = fs::read(&registry).unwrap();
    fs::write(&registry, "invalid store registry").unwrap();
    require_failure(
        command().output().unwrap(),
        "cannot determine the observation storage budget",
    );
    assert_eq!(fs::read_dir(&snapshots).unwrap().count(), 0);
    fs::write(registry, registered).unwrap();

    #[cfg(debug_assertions)]
    {
        let runtime = machine.path("another-tmp");
        fs::create_dir(&runtime).unwrap();
        let other_snapshots = af_sandbox::stale::observation_root(&runtime);
        let second = || {
            command()
                .env("TMPDIR", &runtime)
                .env("AF_STORAGE__SANDBOX_ROOT", &runtime)
                .output()
                .unwrap()
        };
        let marker = machine.path("budget-snapshot-ready");
        for kill in [false, true] {
            let mut first = paused_reader(command(), &marker);
            let competing = second();
            assert!(competing.stdout.is_empty());
            require_failure(
                competing,
                &format!(
                    "cannot observe {}: observation snapshot exceeds the available storage budget",
                    database.display()
                ),
            );
            assert_eq!(fs::read_dir(&other_snapshots).unwrap().count(), 0);
            let mut child = first.0.take().unwrap();
            if kill {
                child.kill().unwrap();
                assert!(!child.wait().unwrap().success());
                assert_eq!(fs::read_dir(&snapshots).unwrap().count(), 1);
            } else {
                fs::remove_file(&marker).unwrap();
                assert!(child.wait().unwrap().success());
                assert_eq!(fs::read_dir(&snapshots).unwrap().count(), 0);
            }
            assert_eq!(as_json(second())["origin"]["tasks"], json!([]));
            assert_eq!(fs::read_dir(&snapshots).unwrap().count(), 0);
        }

        // The first runtime is also inside the configured telemetry tree. The two copies
        // fit with one charge for that retained allocation, but not with two charges.
        fs::remove_file(&marker).unwrap();
        let wider_budget = format!("{}B", baseline + 7 * MIB);
        let wider = || {
            let mut reader = command();
            reader
                .env("AF_TELEMETRY__DIR", machine.path("tmp"))
                .env("AF_STORAGE__MAX_BYTES", &wider_budget);
            reader
        };
        let mut first = paused_reader(wider(), &marker);
        assert_eq!(
            as_json(wider().env("TMPDIR", &runtime).output().unwrap())["origin"]["tasks"],
            json!([])
        );
        fs::remove_file(marker).unwrap();
        assert!(first.0.take().unwrap().wait().unwrap().success());
    }
}

#[cfg(debug_assertions)]
fn paused_reader(mut command: Command, marker: &Path) -> crate::task_interrupt::ChildGuard {
    let child = command
        .env("AF_TEST_FLEET_SNAPSHOT_PAUSE", marker)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut guard = crate::task_interrupt::ChildGuard(Some(child));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        assert!(
            Instant::now() < deadline && guard.0.as_mut().unwrap().try_wait().unwrap().is_none(),
            "first observation never held a snapshot"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    guard
}

#[test]
fn origin_refuses_unmeasurable_overlapping_telemetry() {
    const MIB: u64 = 1024 * 1024;
    let machine = Machine::new();
    let (project, database) = planned_origin(&machine);
    let telemetry = machine.path("telemetry");
    let warm = telemetry.join("warm");
    let key = warm.join("a".repeat(64)).join("b".repeat(64));
    fs::create_dir_all(&key).unwrap();
    fs::write(key.join("build"), vec![1_u8; (3 * MIB) as usize]).unwrap();
    fs::write(telemetry.join("retained"), vec![1_u8; MIB as usize]).unwrap();
    let baseline = af_sandbox::storage::allocated_bytes(&machine.path("state/af")).unwrap();
    let telemetry_bytes = af_sandbox::storage::allocated_bytes(&telemetry).unwrap();
    let command = |max_bytes| {
        let mut reader = machine.origin_reader(&project);
        reader
            .env("AF_STORAGE__BUILD_CACHE_ROOT", &warm)
            .env("AF_TELEMETRY__DIR", &telemetry)
            .env("AF_STORAGE__MAX_BYTES", format!("{max_bytes}B"))
            .env("AF_STORAGE__MIN_FREE_BYTES", "0B");
        reader
    };
    let enough = baseline + telemetry_bytes + MIB;
    assert_eq!(
        as_json(command(enough).output().unwrap())["origin"]["tasks"],
        json!([])
    );
    let denied = command(telemetry_bytes).output().unwrap();
    require_failure(
        denied,
        &format!(
            "cannot observe {}: observation snapshot exceeds the available storage budget",
            database.display()
        ),
    );

    // A nested entry outside the warm cache exceeds the storage walk's depth bound.
    // This makes part of the overlapping telemetry tree unmeasurable under any uid.
    let outside_cache = telemetry.join("outside-cache");
    let too_deep = (0..257).fold(outside_cache.clone(), |path, _| path.join("d"));
    fs::create_dir_all(&too_deep).unwrap();
    fs::write(too_deep.join("retained"), b"unmeasurable data").unwrap();
    let refused = command(enough).output().unwrap();
    assert!(refused.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains(&telemetry.display().to_string()) && stderr.contains("nested deeper"),
        "{stderr}"
    );
    require_failure(refused, "cannot determine the observation storage budget");
    let snapshots = af_sandbox::stale::observation_root(&machine.path("tmp"));
    assert_eq!(fs::read_dir(snapshots).unwrap().count(), 0);
}

#[test]
fn native_origin_observes_clean_and_live_stores_without_writing_sidecars() {
    let machine = Machine::new();
    let (repo, _, _) = crate::task_planning::setup(machine.root.path(), false, false);
    as_json(
        machine
            .af(&["task", "plan", "--file", "ticket.json", "--json"])
            .current_dir(&repo)
            .output()
            .unwrap(),
    );
    let project = project_for(&machine, &repo);
    let workspace = &project["workspaces"][0];
    let state = Path::new(workspace["state"].as_str().unwrap());
    let database = state.join("events.sqlite");
    let command = || machine.origin_reader(&project);
    let reader = || command().output().unwrap();
    let anchor = pin_planned_database(&database);
    let planned_database = sqlite_bytes(&database);
    assert_eq!(as_json(reader())["origin"]["tasks"], json!([]));
    let waiting = as_json(
        machine
            .af(&["task", "run", "--execute", "pagination-cli", "--json"])
            .current_dir(&repo)
            .output()
            .unwrap(),
    );
    assert_eq!(waiting["phase"]["reason"], "needs_plan_review");
    assert_eq!(sqlite_bytes(&database), planned_database);
    let live_files = sqlite_files(state);
    assert_eq!(live_files.len(), 3);
    let live_permissions: Vec<_> = live_files
        .iter()
        .map(|(name, _)| {
            let file = state.join(name);
            let permissions = fs::metadata(&file).unwrap().permissions();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
            (file, permissions)
        })
        .collect();
    let dir_permissions = fs::metadata(state).unwrap().permissions();
    fs::set_permissions(state, fs::Permissions::from_mode(0o555)).unwrap();
    let live = reader();
    fs::set_permissions(state, dir_permissions.clone()).unwrap();
    for (file, permissions) in live_permissions {
        fs::set_permissions(file, permissions).unwrap();
    }
    let live = as_json(live);
    let decision = &live["origin"]["tasks"][0]["decision"];
    assert_eq!(decision["plan_id"], waiting["plan_id"]);
    assert_eq!(decision["task_revision_id"], waiting["revision_id"]);
    assert_eq!(sqlite_files(state), live_files);

    let over_budget = command()
        .env("AF_STORAGE__MAX_BYTES", "1B")
        .output()
        .unwrap();
    require_failure(
        over_budget,
        "observation snapshot exceeds the available storage budget",
    );
    assert_eq!(sqlite_files(state), live_files);
    let snapshots = af_sandbox::stale::observation_root(&machine.path("tmp"));
    assert_eq!(fs::read_dir(&snapshots).unwrap().count(), 0);

    #[cfg(debug_assertions)]
    {
        use std::time::{Duration, Instant};

        let marker = machine.path("snapshot-ready");
        let mut live_reader = command()
            .env("AF_TEST_FLEET_SNAPSHOT_PAUSE", &marker)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let wait_for = |condition: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !condition() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        wait_for(&|| marker.exists());
        let runtime = fs::read_dir(&snapshots)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("af-observation-{}-", live_reader.id()))
            })
            .unwrap();
        assert_eq!(
            fs::metadata(&runtime).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let snapshot = fs::read_dir(&runtime)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("events.sqlite");
        let captured = fs::read(&snapshot).unwrap();

        // A sibling can authenticate its Project and replay while the first reader is
        // paused, and its startup sweep leaves the live copy intact.
        let sibling = as_json(reader());
        assert_eq!(sibling["origin"]["tasks"][0]["decision"], *decision);
        assert_eq!(fs::read(&snapshot).unwrap(), captured);
        live_reader.kill().unwrap();
        live_reader.wait().unwrap();
        assert!(runtime.exists());
        assert_eq!(
            as_json(reader())["origin"]["tasks"][0]["decision"],
            *decision
        );
        assert!(!runtime.exists());
        assert_eq!(sqlite_files(state), live_files);
    }

    // A writer holding SQLite's WAL write lock makes this observation explicitly
    // unavailable. Its uncommitted update must never be projected.
    anchor
        .execute_batch("COMMIT; BEGIN IMMEDIATE; DELETE FROM events;")
        .unwrap();
    let busy_files = sqlite_files(state);
    let busy = reader();
    assert!(busy.stdout.is_empty(), "{busy:?}");
    require_failure(busy, "cannot observe");
    assert_eq!(sqlite_files(state), busy_files);
    anchor.execute_batch("ROLLBACK").unwrap();
    let recovered = as_json(reader());
    assert_eq!(recovered["origin"]["tasks"][0]["decision"], *decision);
    anchor
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
    drop(anchor);

    // The index is disposable after the writer exits. The remaining committed WAL
    // must still be replayed, without rebuilding the index in the original Store.
    fs::remove_file(state.join("events.sqlite-shm")).unwrap();
    let cold_wal = sqlite_files(state);
    assert_eq!(cold_wal.len(), 2);
    assert_eq!(
        as_json(reader())["origin"]["tasks"][0]["decision"],
        *decision
    );
    assert_eq!(sqlite_files(state), cold_wal);

    let checkpoint = rusqlite::Connection::open(&database).unwrap();
    checkpoint
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(checkpoint);
    let clean_files = sqlite_files(state);
    assert_eq!(clean_files.len(), 1);
    let clean = as_json(reader());
    assert_eq!(clean["origin"]["tasks"][0]["decision"], *decision);
    assert_eq!(sqlite_files(state), clean_files);

    fs::set_permissions(&database, fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(state, fs::Permissions::from_mode(0o555)).unwrap();
    let clean_read_only = reader();
    fs::set_permissions(state, dir_permissions).unwrap();
    assert_eq!(
        as_json(clean_read_only)["origin"]["tasks"][0]["decision"],
        *decision
    );
    assert_eq!(sqlite_files(state), clean_files);
}

fn executable(file: &Path, text: &str) {
    fs::write(file, text).unwrap();
    fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn deadline_diagnostics_keep_sibling_outcomes_and_next_actions() {
    let machine = Machine::new();
    let host_key = key(&machine, "host-key");
    key(&machine, "management");
    link(
        &machine,
        "management",
        "config/af/fleet/credentials/operator",
    );
    let target = |host| json!({"host": host, "port": 22, "user": "reader", "host_key": host_key});
    let digest = format!("sha256:{}", "a".repeat(64));
    let origin = |host| json!({"target": target(host), "project_id": digest, "store_id": digest});
    machine.write(
        "lab.afcontext",
        &json!({
            "schema": "af/fleet-context@1",
            "scope": "fleet-fixture",
            "management": target("broker.invalid"),
            "origins": {
                "denied": origin("denied.invalid"),
                "offline": origin("offline.invalid"),
                "stalled": origin("stalled.invalid")
            }
        }),
    );
    for name in ["denied", "offline", "stalled"] {
        link(
            &machine,
            "management",
            &format!("config/af/fleet/project-credentials/lab/{name}"),
        );
    }
    // Stub SSH for deterministic scheduling and command outcomes. The pinned OpenSSH test
    // separately exercises authentication and the same shared-deadline supervisor.
    executable(
        &machine.path("bin/ssh"),
        r#"#!/bin/sh
for arg do
  case "$arg" in *.invalid) host=$arg ;; esac
  operation=$arg
done
case "$operation" in
  'control-api http://localhost/pools') capability=pools ;;
  'control-api http://localhost/dynamic/allocations') capability=allocations ;;
esac
if [ -n "$AF_FIXTURE_FIRST" ]; then
  if [ "$capability" != "$AF_FIXTURE_FIRST" ]; then
    while [ ! -e "first-$AF_FIXTURE_FIRST" ]; do /bin/sleep 0.05; done
  fi
  echo "$capability" >> "requests-$AF_FIXTURE_FIRST"
  : > "first-$AF_FIXTURE_FIRST"
  [ "$capability" != pools ] || exit 75
else
  case "$host" in
    denied.invalid) echo 'private-fixture-detail' >&2; exit 77 ;;
    offline.invalid) exit 75 ;;
    broker.invalid)
      if [ ! -e mixed ]; then
        case "$capability" in
          pools) echo '{"static":[],"dynamic":[],"release":"explicit","available":[]}' ;;
          allocations) echo '[]' ;;
        esac
        exit 0
      fi
      if [ "$capability" = pools ]; then echo 'private-fixture-detail'; exit 0; fi ;;
  esac
fi
echo 'private-fixture-detail' >&2
exec /bin/sleep 90
"#,
    );
    let connect = |alias| {
        machine.af(&[
            "infra",
            "connect",
            "--context",
            alias,
            "--from",
            "lab.afcontext",
            "--credential-ref",
            "operator",
        ])
    };
    let started = Instant::now();
    let failed: Vec<_> = ["pools", "allocations"]
        .into_iter()
        .map(|first| {
            let child = connect(first)
                .env("AF_FIXTURE_FIRST", first)
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            (first, child)
        })
        .collect();
    for (first, child) in failed {
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{output:?}");
        assert!(
            stderr.contains("Management pools capability: reader unavailable"),
            "{stderr}"
        );
        assert!(
            stderr.contains(
                "Management allocations capability: reader exceeded the command deadline"
            ),
            "{stderr}"
        );
        assert!(!stderr.contains("private-fixture-detail"), "{stderr}");
        assert!(
            !machine
                .path(&format!("config/af/fleet/contexts/{first}.json"))
                .exists()
        );
        let requests = fs::read_to_string(machine.path(&format!("requests-{first}"))).unwrap();
        assert_eq!(requests.lines().count(), 2);
        assert_eq!(requests.lines().next(), Some(first));
    }
    assert!(started.elapsed() < Duration::from_secs(45));

    success(connect("lab").output().unwrap());
    let fresh = machine.json(&["infra", "--json"]);
    assert_eq!(fresh["allocations"]["data"], json!([]));
    fs::write(machine.path("mixed"), "").unwrap();
    let validator = schemas::validator("fleet-briefing-v3.json");
    let started = Instant::now();
    let reads: Vec<_> = [&["infra"][..], &["cluster", "inbox"]]
        .into_iter()
        .flat_map(|args| {
            [true, false].map(|json| {
                let mut command = machine.af(args);
                if json {
                    command.arg("--json");
                }
                command
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
        })
        .collect();
    let mut output = reads.into_iter().map(|child| {
        let output = child.wait_with_output().unwrap();
        assert!(output.stderr.is_empty(), "{output:?}");
        output
    });
    let infra = as_json(output.next().unwrap());
    let infra_text = success(output.next().unwrap());
    let inbox = as_json(output.next().unwrap());
    let inbox_text = success(output.next().unwrap());
    assert!(started.elapsed() < Duration::from_secs(35));
    for brief in [&infra, &inbox] {
        schemas::valid(&validator, brief);
        assert!(serde_json::from_value::<af_contracts::fleet::BriefingV3>(brief.clone()).is_ok());
        assert!(!brief.to_string().contains("private-fixture-detail"));
    }
    let assert_read = |read: &Value, freshness, timed_out| {
        assert_eq!(read["observation"]["freshness"], freshness);
        assert_eq!(read["timed_out"], timed_out);
        assert!(read["data"].is_null());
        assert!(read["observation"]["age_ms"].is_null());
    };
    assert_read(&infra["inventory"], "invalid_response", false);
    assert_read(&infra["allocations"], "unavailable", true);
    assert_eq!(infra["next_action"]["action"], "repair");
    assert_read(&inbox["origins"][0], "denied", false);
    assert_read(&inbox["origins"][1], "unavailable", false);
    assert_read(&inbox["origins"][2], "unavailable", true);
    assert_eq!(inbox["next_action"]["action"], "ask_owner");
    for (text, timeout_row, siblings, next) in [
        (
            &infra_text,
            "fleet-fixture / allocations",
            &["fleet-fixture / pools · invalid_response"][..],
            "Next repair",
        ),
        (
            &inbox_text,
            "stalled / waiting_tasks",
            &[
                "denied / waiting_tasks · denied",
                "offline / waiting_tasks · unavailable",
            ][..],
            "Next ask_owner",
        ),
    ] {
        assert!(
            text.lines().any(|line| line.contains(timeout_row)
                && line.contains("reader exceeded the command deadline")),
            "{text}"
        );
        assert!(
            siblings.iter().all(|sibling| text.contains(sibling)),
            "{text}"
        );
        assert!(
            text.contains(next) && !text.contains("private-fixture-detail"),
            "{text}"
        );
    }
}

#[test]
fn storage_pool_inventory_survives_only_current_cache_format() {
    let machine = Machine::new();
    let host_key = key(&machine, "host-key");
    key(&machine, "management");
    link(
        &machine,
        "management",
        "config/af/fleet/credentials/operator",
    );
    machine.write(
        "lab.afcontext",
        &json!({
            "schema": "af/fleet-context@1",
            "scope": "fleet-fixture",
            "management": {
                "host": "broker.invalid", "port": 22, "user": "operator", "host_key": host_key
            }
        }),
    );
    // The real pinned OpenSSH fixture below covers authentication. Here, exit 75 makes
    // the outage deterministic and independent of the host's sshd setup.
    executable(
        &machine.path("bin/ssh"),
        r#"#!/bin/sh
for operation do :; done
case "$operation" in
  'control-api http://localhost/pools')
    [ ! -e temporary ] || exit 75
    echo '{"static":["static-builder"],"dynamic":["dynamic-worker"],"release":"explicit","available":[]}' ;;
  'control-api http://localhost/dynamic/allocations') echo '[]' ;;
  *) exit 77 ;;
esac
"#,
    );
    success(
        machine
            .af(&[
                "infra",
                "connect",
                "--context",
                "lab",
                "--from",
                "lab.afcontext",
                "--credential-ref",
                "operator",
            ])
            .output()
            .unwrap(),
    );

    let status = || machine.json(&["infra", "status", "--context", "lab", "--json"]);
    let render = || {
        success(
            machine
                .af(&["infra", "status", "--context", "lab"])
                .output()
                .unwrap(),
        )
    };
    let briefing_schema = schemas::validator("fleet-briefing-v3.json");
    let cache_schema = schemas::validator("fleet-cache-v2.json");
    let fresh = status();
    schemas::valid(&briefing_schema, &fresh);
    let typed = serde_json::from_value::<af_contracts::fleet::BriefingV3>(fresh.clone()).unwrap();
    let inventory = typed.inventory.unwrap();
    assert!(!inventory.timed_out);
    assert!(inventory.data.unwrap().pools.is_none());
    assert!(render().contains("Storage pool inventory unknown"));
    let mut missing = fresh.clone();
    missing["inventory"]["data"]
        .as_object_mut()
        .unwrap()
        .remove("pools");
    assert!(!briefing_schema.is_valid(&missing));
    assert!(serde_json::from_value::<af_contracts::fleet::BriefingV3>(missing).is_err());
    let mut missing = fresh.clone();
    missing["inventory"]
        .as_object_mut()
        .unwrap()
        .remove("timed_out");
    assert!(!briefing_schema.is_valid(&missing));
    assert!(serde_json::from_value::<af_contracts::fleet::BriefingV3>(missing).is_err());
    let mut old_briefing = fresh.clone();
    old_briefing["schema"] = json!("af/fleet-briefing@2");
    assert!(!briefing_schema.is_valid(&old_briefing));
    assert!(serde_json::from_value::<af_contracts::fleet::BriefingV3>(old_briefing).is_err());

    let cache = "state/af/fleet-cache/lab/pools.json";
    let record: Value = serde_json::from_slice(&fs::read(machine.path(cache)).unwrap()).unwrap();
    schemas::valid(&cache_schema, &record);
    assert_eq!(record["data"], fresh["inventory"]["data"]);
    fs::write(machine.path("temporary"), "").unwrap();
    let stale = status();
    assert_eq!(stale["inventory"]["observation"]["freshness"], "stale");
    assert_eq!(stale["inventory"]["data"], record["data"]);

    // Only the current-format synthetic observation has explicitly observed an empty list.
    let mut empty = record;
    empty["data"]["pools"] = json!([]);
    schemas::valid(&cache_schema, &empty);
    machine.write(cache, &empty);
    let known_empty = status();
    schemas::valid(&briefing_schema, &known_empty);
    assert_eq!(
        known_empty["inventory"]["observation"]["freshness"],
        "stale"
    );
    assert_eq!(known_empty["inventory"]["data"]["pools"], json!([]));
    let text = render();
    assert!(text.contains("No storage pools") && !text.contains("Storage pool inventory unknown"));

    // Simulate a previous release's record with the same binding, subject, and fresh timestamp.
    let mut obsolete = empty;
    obsolete["schema"] = json!("af/fleet-cache@1");
    obsolete["data"]["pools"] = json!([
        {"name":"static-builder","static_pool":true,"dynamic_pool":false,"available_gib":null},
        {"name":"dynamic-worker","static_pool":false,"dynamic_pool":true,"available_gib":null}
    ]);
    assert!(!cache_schema.is_valid(&obsolete));
    machine.write(cache, &obsolete);
    let unavailable = status();
    schemas::valid(&briefing_schema, &unavailable);
    assert_eq!(
        unavailable["inventory"]["observation"]["freshness"],
        "unavailable"
    );
    assert!(unavailable["inventory"]["data"].is_null());
    let text = render();
    assert!(
        text.contains("pools") && text.contains("unavailable"),
        "{text}"
    );
    for vm in ["static-builder", "dynamic-worker"] {
        assert!(!unavailable.to_string().contains(vm));
        assert!(!text.contains(vm), "{text}");
    }

    fs::remove_file(machine.path("temporary")).unwrap();
    let recovered = status();
    assert_eq!(recovered["inventory"]["observation"]["freshness"], "fresh");
    assert_eq!(recovered["inventory"]["data"], fresh["inventory"]["data"]);
    let refreshed: Value = serde_json::from_slice(&fs::read(machine.path(cache)).unwrap()).unwrap();
    schemas::valid(&cache_schema, &refreshed);
    assert_eq!(refreshed["data"], recovered["inventory"]["data"]);

    // Drop the existing record so simultaneous processes compete for space for one
    // observation. The broker reads themselves must all still succeed.
    let stored = machine.json(&["storage", "--json"]);
    let storage_schema = schemas::validator("storage-v6.json");
    schemas::valid(&storage_schema, &stored);
    let cache_bytes = stored["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|kind| kind["kind"] == "fleet_cache")
        .unwrap()["bytes"]
        .as_u64()
        .unwrap();
    let budget = stored["total_bytes"].as_u64().unwrap();
    assert!(cache_bytes > 0);
    fs::remove_file(machine.path(cache)).unwrap();
    let children: Vec<_> = (0..6)
        .map(|index| {
            let alias = format!("reader-{index}");
            success(
                machine
                    .af(&[
                        "infra",
                        "connect",
                        "--context",
                        &alias,
                        "--from",
                        "lab.afcontext",
                        "--credential-ref",
                        "operator",
                    ])
                    .output()
                    .unwrap(),
            );
            alias
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|alias| {
            machine
                .af(&["infra", "status", "--context", &alias, "--json"])
                .env("AF_STORAGE__MAX_BYTES", format!("{budget}B"))
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let brief = as_json(child.wait_with_output().unwrap());
        assert_eq!(brief["inventory"]["observation"]["freshness"], "fresh");
    }
    let retained = machine.json(&["storage", "--json"]);
    schemas::valid(&storage_schema, &retained);
    assert_eq!(retained["total_bytes"], budget);
    let actual: Vec<_> = retained["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "fleet_cache" && entry["bytes"] == cache_bytes)
        .collect();
    assert_eq!(actual.len(), 1);
    let dir = Path::new(actual[0]["path"].as_str().unwrap());
    let expired = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 60 * 60);
    fs::File::open(dir.join("pools.json"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(expired))
        .unwrap();
    let pruned = machine.json(&["storage", "prune", "--apply", "--json"]);
    schemas::valid(&schemas::validator("storage-prune-v5.json"), &pruned);
    assert!(
        pruned["removals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|removal| {
                removal["kind"] == "fleet_cache"
                    && removal["bytes"] == cache_bytes
                    && removal["path"] == dir.to_str().unwrap()
            })
    );
    assert!(!dir.exists());
    assert_eq!(
        machine.json(&["storage", "--json"])["total_bytes"],
        budget - cache_bytes
    );
}

fn key(machine: &Machine, name: &str) -> String {
    success(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(machine.path(name))
            .output()
            .unwrap(),
    );
    fs::read_to_string(machine.path(&format!("{name}.pub")))
        .unwrap()
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

fn link(machine: &Machine, key: &str, destination: &str) {
    let destination = machine.path(destination);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    symlink(machine.path(key), destination).unwrap();
}

fn quoted(path: &Path) -> String {
    shell_words::quote(path.to_str().unwrap()).into_owned()
}

struct Endpoint {
    address: (&'static str, u16),
    #[cfg(target_os = "macos")]
    _server: loopback::Server,
}

fn endpoint(machine: &Machine, sshd: &Path, name: &str, allowed: &[(&str, &str)]) -> Endpoint {
    let authorized = allowed
        .iter()
        .map(|(key, command)| format!("restrict,command=\"{command}\" {key}\n"))
        .collect::<String>();
    fs::write(machine.path(&format!("{name}.keys")), authorized).unwrap();
    let config = machine.path(&format!("{name}.sshd"));
    let log = machine.path(&format!("{name}.sshd.log"));
    fs::write(&config, format!(
        "HostKey {}\nAuthorizedKeysFile {}\nStrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\n",
        quoted(&machine.path("host-key")),
        quoted(&machine.path(&format!("{name}.keys")))
    )).unwrap();
    #[cfg(target_os = "macos")]
    {
        let server = loopback::Server::start(sshd, config, log);
        Endpoint {
            address: ("127.0.0.1", server.port),
            _server: server,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        executable(
            &machine.path(&format!("{name}.proxy")),
            &format!(
                "#!/bin/sh\nexec {} -i -e -f {} 2>>{}\n",
                quoted(sshd),
                quoted(&config),
                quoted(&log),
            ),
        );
        let host = match name {
            "broker" => "broker.invalid",
            "project" => "project.invalid",
            _ => unreachable!("unknown SSH fixture endpoint {name}"),
        };
        Endpoint {
            address: (host, 22),
        }
    }
}

fn ssh_fixture(machine: &Machine) {
    // Keep stdio on Linux so this also runs in gates that forbid TCP listeners.
    let transport = if cfg!(target_os = "macos") {
        "exec /usr/bin/ssh \"$@\"".to_owned()
    } else {
        format!(
            "[ -n \"$reader\" ] || exit 255\nexec /usr/bin/ssh -o \"ProxyCommand={}/$reader.proxy\" \"$@\"",
            quoted(machine.root.path())
        )
    };
    executable(
        &machine.path("bin/ssh"),
        &format!(
            r#"#!/bin/sh
for arg do
  case "$arg" in
    broker.invalid) reader=broker ;;
    project.invalid) reader=project ;;
  esac
  operation=$arg
done
case "$operation" in
  'control-api '*)
    if [ -f {first_reader} ] && [ "$operation" != "$(cat {first_reader})" ]; then
      while [ ! -e {first_request} ]; do /bin/sleep 0.05; done
    fi ;;
esac
{transport}
"#,
            first_reader = quoted(&machine.path("first-reader")),
            first_request = quoted(&machine.path("first-request")),
        ),
    );
}

// A forced command can survive the client disconnect in its own SSH session.
// Release it explicitly on both success and unwind, with a finite remote fallback.
struct PoolsHold<'a>(&'a Machine);

impl<'a> PoolsHold<'a> {
    fn start(machine: &'a Machine, seconds: u8) -> Self {
        fs::write(machine.path("hold-pools"), seconds.to_string()).unwrap();
        Self(machine)
    }
}

impl Drop for PoolsHold<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0.path("hold-pools"));
        let active = self.0.path("pools-held");
        let deadline = Instant::now() + Duration::from_secs(5);
        while active.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if !std::thread::panicking() {
            assert!(!active.exists(), "forced pools command did not release");
        }
    }
}

#[cfg(target_os = "macos")]
mod loopback {
    use std::fs::File;
    use std::io::ErrorKind;
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
    use std::os::fd::OwnedFd;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use crate::task_interrupt::ChildGuard;

    const POLL: Duration = Duration::from_millis(10);

    pub(super) struct Server {
        pub(super) port: u16,
        stop: Sender<()>,
        worker: Option<JoinHandle<()>>,
        log: PathBuf,
    }

    impl Server {
        pub(super) fn start(sshd: &Path, config: PathBuf, log: PathBuf) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let (stop, stopped) = mpsc::channel();
            let sshd = sshd.to_owned();
            let stderr = File::create(&log).unwrap();
            let worker = thread::spawn(move || {
                let mut children: Vec<ChildGuard> = Vec::new();
                while stopped.recv_timeout(POLL) == Err(RecvTimeoutError::Timeout) {
                    children.retain_mut(|child| {
                        if af_process::reap::try_wait(child.0.as_mut().unwrap())
                            .unwrap()
                            .is_some()
                        {
                            child.0 = None;
                        }
                        child.0.is_some()
                    });
                    let (stream, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == ErrorKind::WouldBlock => continue,
                        Err(error) => panic!("accepting fixture SSH connection: {error}"),
                    };
                    // Apple's inetd-mode sshd needs a connected TCP socket on both descriptors
                    // to create the audit session for its forced command.
                    // The temporary Command closes the parent's socket copies after spawn.
                    let child = af_process::spawn(
                        Command::new(&sshd)
                            .args(["-i", "-e", "-f"])
                            .arg(&config)
                            .stdin(Stdio::from(OwnedFd::from(stream.try_clone().unwrap())))
                            .stdout(Stdio::from(OwnedFd::from(stream)))
                            .stderr(Stdio::from(stderr.try_clone().unwrap())),
                    )
                    .unwrap();
                    children.push(ChildGuard(Some(child)));
                }
            });
            Self {
                port,
                stop,
                worker: Some(worker),
                log,
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.stop.send(());
            if let Err(error) = self.worker.take().unwrap().join()
                && !thread::panicking()
            {
                std::panic::resume_unwind(error);
            }
            if thread::panicking()
                && let Ok(log) = std::fs::read_to_string(&self.log)
            {
                eprintln!("{}:\n{log}", self.log.display());
            }
        }
    }

    pub(super) fn disconnected() -> (OwnedFd, (&'static str, u16)) {
        use rustix::io::{FdFlags, fcntl_setfd};
        use rustix::net::{AddressFamily, SocketType, bind, getsockname, socket};

        // Reserve the ephemeral port without listening, so no SSH handshake is possible.
        let socket = socket(AddressFamily::INET, SocketType::STREAM, None).unwrap();
        fcntl_setfd(&socket, FdFlags::CLOEXEC).unwrap();
        bind(&socket, &SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = SocketAddrV4::try_from(getsockname(&socket).unwrap()).unwrap();
        (socket, ("127.0.0.1", address.port()))
    }
}

#[test]
fn pinned_ssh_broker_and_project_origin_keep_independent_read_authority() {
    let sshd = Path::new("/usr/sbin/sshd");
    assert!(
        sshd.is_file() && Path::new("/usr/bin/ssh").is_file(),
        "OpenSSH integration fixture requires /usr/sbin/sshd and /usr/bin/ssh"
    );
    let machine = Machine::new();
    let (repo, _, _) = crate::task_planning::setup(machine.root.path(), false, false);
    let plan = machine
        .af(&["task", "plan", "--file", "ticket.json", "--json"])
        .current_dir(&repo)
        .output()
        .unwrap();
    as_json(plan);
    let project = project_for(&machine, &repo);
    let project_id = project["project_id"].as_str().unwrap();
    let workspace = &project["workspaces"][0];
    let state = Path::new(workspace["state"].as_str().unwrap());
    let store_id = format!("sha256:{}", af_telemetry::trace::store_id(state).unwrap());
    let database = state.join("events.sqlite");
    let _anchor = pin_planned_database(&database);
    let planned_database = sqlite_bytes(&database);
    let waiting = as_json(
        machine
            .af(&["task", "run", "--execute", "pagination-cli", "--json"])
            .current_dir(&repo)
            .output()
            .unwrap(),
    );
    assert_eq!(waiting["phase"]["reason"], "needs_plan_review");
    assert_eq!(sqlite_bytes(&database), planned_database);
    let native_history = sqlite_files(state);
    assert_eq!(native_history.len(), 3);

    let host_key = key(&machine, "host-key");
    let wrong_host_key = key(&machine, "wrong-host-key");
    let management_key = key(&machine, "management");
    let owner_key = key(&machine, "owner");
    link(
        &machine,
        "management",
        "config/af/fleet/credentials/operator",
    );
    link(
        &machine,
        "management",
        "config/af/fleet/project-credentials/lab/project",
    );
    machine.write(
        "pools.json",
        &json!({
            "static": ["static-builder", "static-runner"], "dynamic": ["dynamic-worker"], "release": "explicit",
            "available": [{"node":"host-a", "cpu_ids":[0,1,2,3,3], "ram_gib":8, "ssd_gib":120,
                            "static_available":{"cpu_ids":[0,1], "ram_gib":4, "ssd_gib":90}}]
        }),
    );
    machine.write("allocations.json", &json!([
        {"vmid":101,"name":"dynamic-worker","node":"host-a","power":"stopped","state":"reserved","task":"private-native-task",
         "charged":{"cpu_ids":[],"ram_gib":0,"ssd_gib":80_000_000_000_f64 / 1_073_741_824.0}},
        {"vmid":102,"name":null,"node":null,"power":null,"state":null,"task":null}
    ]));
    executable(
        &machine.path("broker"),
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$SSH_ORIGINAL_COMMAND" >> {requests}
if [ -f {first_reader} ] && [ "$SSH_ORIGINAL_COMMAND" = "$(cat {first_reader})" ]; then
  : > {first_request}
fi
case "$SSH_ORIGINAL_COMMAND" in
  'control-api http://localhost/pools')
    [ ! -e {temporary} ] || exit 75
    if [ -f {hold} ]; then
      : > {held}
      remaining=$(cat {hold})
      while [ -e {hold} ] && [ "${{remaining:-0}}" -gt 0 ]; do
        /bin/sleep 1
        remaining=$((remaining - 1))
      done
      rm -f {held}
      [ -e {hold} ] || exit 75
    fi
    exec /bin/cat {pools} ;;
  'control-api http://localhost/dynamic/allocations')
    [ ! -e {temporary_allocations} ] || exit 75
    exec /bin/cat {allocations} ;;
  *) exit 77 ;;
esac
"#,
            requests = quoted(&machine.path("requests")),
            first_reader = quoted(&machine.path("first-reader")),
            first_request = quoted(&machine.path("first-request")),
            temporary = quoted(&machine.path("temporary")),
            temporary_allocations = quoted(&machine.path("temporary-allocations")),
            hold = quoted(&machine.path("hold-pools")),
            held = quoted(&machine.path("pools-held")),
            pools = quoted(&machine.path("pools.json")),
            allocations = quoted(&machine.path("allocations.json"))
        ),
    );
    let reader = format!(
        "exec env HOME={} AF_TEST_WAKE_ACCOUNT_HOME={} XDG_CONFIG_HOME={} TMPDIR={} {} cluster origin-read --scope fleet-fixture --origin project --project {} --workspace {} --principal owner --allow-plan-decisions --state-home {}",
        quoted(&machine.path("home")),
        quoted(&machine.path("home")),
        quoted(&machine.path("config")),
        quoted(&machine.path("tmp")),
        quoted(common::af_binary()),
        project_id,
        shell_words::quote(workspace["label"].as_str().unwrap()),
        quoted(&machine.path("state")),
    );
    // The remote fixture chooses its result after auth, independently of all client arguments.
    executable(
        &machine.path("project"),
        &format!(
            r#"#!/bin/sh
[ "$SSH_ORIGINAL_COMMAND" = 'af cluster origin-read' ] || exit 77
[ ! -e {} ] || exit 75
if [ -e {} ]; then exec /bin/cat {}; fi
{reader}
"#,
            quoted(&machine.path("project-offline")),
            quoted(&machine.path("project-stale")),
            quoted(&machine.path("project-observation.json"))
        ),
    );
    let broker = endpoint(
        &machine,
        sshd,
        "broker",
        &[(&management_key, &quoted(&machine.path("broker")))],
    );
    let project = endpoint(
        &machine,
        sshd,
        "project",
        &[(&owner_key, &quoted(&machine.path("project")))],
    );
    #[cfg(target_os = "macos")]
    let (_reserved_port, disconnected) = loopback::disconnected();
    #[cfg(not(target_os = "macos"))]
    let disconnected = ("offline.invalid", 22);
    ssh_fixture(&machine);
    let user = success(Command::new("id").arg("-un").output().unwrap());
    let target = |(host, port): (&str, u16), pin: &str| json!({"host":host,"port":port,"user":user.trim(),"host_key":pin});
    let origin = |address, id: &str| {
        json!({
            "target":target(address, &host_key), "project_id":project_id, "store_id":id
        })
    };
    let descriptor = json!({
        "schema":"af/fleet-context@1","scope":"fleet-fixture","cluster":"absent",
        "management":target(broker.address,&host_key),
        "origins":{"project":origin(project.address,&store_id)}
    });
    let validator = schemas::validator("fleet-briefing-v3.json");
    schemas::valid(&schemas::validator("fleet-context-v1.json"), &descriptor);
    machine.write("lab.afcontext", &descriptor);
    let connect = |alias, file| {
        machine
            .af(&[
                "infra",
                "connect",
                "--context",
                alias,
                "--from",
                file,
                "--credential-ref",
                "operator",
            ])
            .output()
            .unwrap()
    };
    let mut wrong = descriptor.clone();
    wrong["management"]["host_key"] = json!(wrong_host_key);
    machine.write("wrong.afcontext", &wrong);
    require_failure(
        connect("bad", "wrong.afcontext"),
        "server identity mismatch",
    );
    assert!(!machine.path("config/af/fleet/contexts/bad.json").exists());
    assert!(!machine.path("requests").exists());
    let pools_operation = "control-api http://localhost/pools";
    let allocations_operation = "control-api http://localhost/dynamic/allocations";
    fs::write(machine.path("first-reader"), allocations_operation).unwrap();
    let first = {
        let _hold = PoolsHold::start(&machine, 22);
        success(connect("lab", "lab.afcontext"))
    };
    let requests = fs::read_to_string(machine.path("requests")).unwrap();
    assert_eq!(
        requests.lines().collect::<Vec<_>>(),
        [allocations_operation, pools_operation]
    );
    fs::remove_file(machine.path("first-reader")).unwrap();
    fs::remove_file(machine.path("first-request")).unwrap();
    assert!(
        first.contains("pinned management reader is authenticated"),
        "{first}"
    );
    let binding_path = machine.path("config/af/fleet/contexts/lab.json");
    let bytes = fs::read(&binding_path).unwrap();
    schemas::valid(
        &schemas::validator("fleet-binding-v1.json"),
        &serde_json::from_slice(&bytes).unwrap(),
    );
    let bound_at = fs::metadata(&binding_path).unwrap().modified().unwrap();
    success(connect("lab", "lab.afcontext"));
    assert_eq!(
        fs::metadata(&binding_path).unwrap().modified().unwrap(),
        bound_at
    );
    let mut different = descriptor.clone();
    different["scope"] = json!("different-scope");
    machine.write("collision.afcontext", &different);
    require_failure(
        connect("lab", "collision.afcontext"),
        "already has a different binding",
    );
    assert_eq!(fs::read(binding_path).unwrap(), bytes);
    fs::write(machine.path("temporary-allocations"), "").unwrap();
    require_failure(
        connect("incomplete", "lab.afcontext"),
        "Management allocations capability: reader unavailable",
    );
    assert!(
        !machine
            .path("config/af/fleet/contexts/incomplete.json")
            .exists()
    );
    fs::remove_file(machine.path("temporary-allocations")).unwrap();

    let brief = machine.json(&["infra", "--json"]);
    schemas::valid(&validator, &brief);
    assert_eq!(brief["context"], "lab");
    assert_eq!(brief["discovery"], "ready");
    assert!(brief["desired_footprint"].is_null());
    let status = machine.json(&["infra", "status", "--context", "lab", "--json"]);
    schemas::valid(&validator, &status);
    assert_eq!(status["next_action"]["action"], "run");
    let inventory = &status["inventory"]["data"];
    assert_eq!(inventory["hosts"].as_array().unwrap().len(), 1);
    let host = &inventory["hosts"][0];
    assert_eq!(host["available"]["cpu_ids"], json!([0, 1, 2, 3]));
    assert_eq!(host["available"]["ram_gib"].as_f64(), Some(8.0));
    assert_eq!(host["available"]["ssd_gib"].as_f64(), Some(120.0));
    assert_eq!(host["static_available"]["cpu_ids"], json!([0, 1]));
    assert_eq!(host["static_available"]["ram_gib"].as_f64(), Some(4.0));
    assert_eq!(host["static_available"]["ssd_gib"].as_f64(), Some(90.0));
    assert!(host["physical_totals"].is_null() && host["reservations"].is_null());
    assert!(status["inventory"]["observation"]["revision"].is_null());
    assert_eq!(inventory["pools"], Value::Null);
    let text = success(
        machine
            .af(&["infra", "status", "--context", "lab"])
            .output()
            .unwrap(),
    );
    assert!(
        text.contains("4 allocatable vCPUs")
            && text.contains("Storage pool inventory unknown")
            && text.contains("Next run"),
        "{text}"
    );
    for vm in ["static-builder", "static-runner", "dynamic-worker"] {
        assert!(!text.contains(vm), "{text}");
    }
    let allocations = machine.json(&["infra", "allocations", "--context", "lab", "--json"]);
    let vms = &allocations["allocations"]["data"];
    assert_eq!(vms[0]["name"], "dynamic-worker");
    assert_eq!(vms[0]["power"], "stopped");
    schemas::valid(&validator, &allocations);
    assert_eq!(
        vms[0]["charged"]["ssd_gib"],
        json!(80_000_000_000_f64 / 1_073_741_824.0)
    );
    assert!(vms[0]["executor"].is_null() && vms[1]["node"].is_null());
    assert!(vms[1]["charged"].is_null());
    let text = success(
        machine
            .af(&["infra", "allocations", "--context", "lab"])
            .output()
            .unwrap(),
    );
    assert!(
        text.lines().any(|line| line.contains("VM 101")
            && line.contains("power stopped")
            && line.contains("80 GB disk")),
        "{text}"
    );
    assert!(
        text.lines().any(|line| line.contains("VM 102")
            && line.contains("host unknown")
            && line.contains("charges unknown")),
        "{text}"
    );
    assert!(!allocations.to_string().contains("private-native-task"));

    let cache = machine.path("state/af/fleet-cache/lab/pools.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
    schemas::valid(&schemas::validator("fleet-cache-v2.json"), &record);
    fs::write(machine.path("temporary"), "").unwrap();
    require_failure(
        connect("incomplete", "lab.afcontext"),
        "Management pools capability: reader unavailable",
    );
    assert!(
        !machine
            .path("config/af/fleet/contexts/incomplete.json")
            .exists()
    );
    let stale = machine.json(&["infra", "status", "--context", "lab", "--json"]);
    assert_eq!(stale["inventory"]["observation"]["freshness"], "stale");
    assert_eq!(stale["inventory"]["data"], *inventory);
    assert_eq!(stale["next_action"]["action"], "wait");
    record["observed_unix_ms"] = json!(1);
    fs::write(&cache, record.to_string()).unwrap();
    let expired = machine.json(&["infra", "status", "--context", "lab", "--json"]);
    assert!(expired["inventory"]["data"].is_null());
    assert!(!cache.exists());
    fs::remove_file(machine.path("temporary")).unwrap();

    for (first, second) in [
        (pools_operation, allocations_operation),
        (allocations_operation, pools_operation),
    ] {
        let previous = fs::metadata(machine.path("requests")).unwrap().len() as usize;
        fs::write(machine.path("first-reader"), first).unwrap();
        let hold = PoolsHold::start(&machine, 40);
        let partial = machine.json(&["infra", "--context", "lab", "--json"]);
        drop(hold);
        let requests = fs::read_to_string(machine.path("requests")).unwrap();
        assert_eq!(
            requests[previous..].lines().collect::<Vec<_>>(),
            [first, second]
        );
        fs::remove_file(machine.path("first-reader")).unwrap();
        fs::remove_file(machine.path("first-request")).unwrap();
        assert_eq!(partial["discovery"], "partial", "{first}: {partial}");
        assert_eq!(
            partial["next_action"]["reason"],
            "A management reader exceeded the command deadline"
        );
        assert_eq!(
            partial["inventory"]["observation"]["freshness"],
            "unavailable"
        );
        assert!(partial["inventory"]["data"].is_null());
        assert_eq!(partial["inventory"]["timed_out"], true);
        assert_eq!(partial["allocations"]["observation"]["freshness"], "fresh");
        assert_eq!(partial["allocations"]["timed_out"], false);
        assert_eq!(partial["allocations"]["data"], *vms);
    }

    let denied = machine.json(&["cluster", "inbox", "--context", "lab", "--json"]);
    assert_eq!(denied["origins"][0]["observation"]["freshness"], "denied");
    assert!(denied["origins"][0]["data"].is_null());
    assert!(!denied.to_string().contains("pagination-cli"));
    fs::remove_file(machine.path("config/af/fleet/project-credentials/lab/project")).unwrap();
    link(
        &machine,
        "owner",
        "config/af/fleet/project-credentials/lab/project",
    );
    let work = machine.json(&["cluster", "--infra", "lab", "--json"]);
    assert_eq!(work["discovery"], "ready");
    assert_eq!(work["cluster"], "absent");
    assert_eq!(work["distributed_execution_ready"], false);
    let inbox = machine.json(&["cluster", "inbox", "--context", "lab", "--json"]);
    schemas::valid(&validator, &inbox);
    assert_eq!(inbox["next_action"]["action"], "run");
    let task = &inbox["origins"][0]["data"]["tasks"][0];
    assert_eq!(task["task_id"], "pagination-cli");
    assert_eq!(task["phase"], "waiting");
    assert_eq!(task["reason"], "needs_plan_review");
    assert_eq!(task["decision"]["plan_id"], waiting["plan_id"]);
    assert_eq!(task["decision"]["task_revision_id"], waiting["revision_id"]);
    assert!(task["decision"]["prepared_id"].is_null());
    assert_eq!(task["decision"]["developer"], "owner");
    assert!(task["placement"].is_null());
    assert!(task["observation"]["revision"].is_u64());
    let inbox_text = success(
        machine
            .af(&["cluster", "inbox", "--context", "lab"])
            .output()
            .unwrap(),
    );
    assert!(inbox_text.contains("may approve or reject plan") && inbox_text.contains("Next run"));

    // A read-only grant can list Tasks while the server withholds plan Decisions.
    let origin_reply = machine
        .af(&[
            "cluster",
            "origin-read",
            "--scope",
            "fleet-fixture",
            "--origin",
            "project",
            "--project",
            project_id,
            "--workspace",
            workspace["label"].as_str().unwrap(),
            "--principal",
            "reader",
            "--state-home",
            path(&machine.path("state")),
        ])
        .output()
        .unwrap();
    let mut wire = as_json(origin_reply);
    schemas::valid(&schemas::validator("fleet-origin-v1.json"), &wire);
    assert!(wire["origin"]["tasks"][0]["decision"].is_null());
    let current = wire["observed_unix_ms"].as_u64().unwrap();
    wire["observed_unix_ms"] = json!(current - 600_000);
    wire["origin"]["tasks"][0]["observation"]["observed_unix_ms"] =
        wire["observed_unix_ms"].clone();
    wire["origin"]["principal"] = json!("owner");
    wire["origin"]["tasks"][0]["decision"] = task["decision"].clone();
    machine.write("project-observation.json", &wire);
    fs::write(machine.path("project-stale"), "").unwrap();
    let stale = machine.json(&["cluster", "inbox", "--context", "lab", "--json"]);
    assert_eq!(stale["origins"][0]["observation"]["freshness"], "stale");
    assert!(stale["origins"][0]["data"]["tasks"][0]["decision"].is_null());
    fs::remove_file(machine.path("project-stale")).unwrap();
    fs::write(machine.path("project-offline"), "").unwrap();
    let offline = machine.json(&["cluster", "inbox", "--context", "lab", "--json"]);
    assert_eq!(
        offline["origins"][0]["observation"]["freshness"],
        "unavailable"
    );
    assert!(offline["origins"][0]["data"].is_null());
    fs::remove_file(machine.path("project-offline")).unwrap();

    let mut mixed = descriptor;
    mixed["origins"]["disconnected"] = origin(disconnected, &store_id);
    mixed["origins"]["mismatch"] = origin(project.address, &store_id);
    machine.write("mixed.afcontext", &mixed);
    success(connect("mixed", "mixed.afcontext"));
    let choices = machine.json(&["infra", "--json"]);
    assert_eq!(choices["configured_contexts"], json!(["lab", "mixed"]));
    assert!(choices["context"].is_null());
    let missing_keys = machine.json(&["cluster", "inbox", "--context", "mixed", "--json"]);
    assert!(
        missing_keys["origins"]
            .as_array()
            .unwrap()
            .iter()
            .all(|origin| {
                origin["observation"]["freshness"] == "missing_credential"
                    && origin["data"].is_null()
            })
    );

    for name in ["project", "disconnected", "mismatch"] {
        link(
            &machine,
            "owner",
            &format!("config/af/fleet/project-credentials/mixed/{name}"),
        );
    }
    let mixed = machine.json(&["cluster", "inbox", "--context", "mixed", "--json"]);
    assert_eq!(mixed["discovery"], "partial");
    assert_eq!(
        mixed["origins"][0]["observation"]["freshness"],
        "unavailable"
    );
    assert_eq!(
        mixed["origins"][1]["observation"]["freshness"],
        "identity_mismatch"
    );
    assert_eq!(mixed["origins"][2]["observation"]["freshness"], "fresh");
    assert_eq!(
        mixed["origins"][2]["data"]["tasks"][0]["task_id"],
        "pagination-cli"
    );
    assert_eq!(native_history, sqlite_files(state));
    assert!(
        !machine
            .path("state/af/fleet-cache/lab/waiting_tasks.json")
            .exists()
    );
    let requests = fs::read_to_string(machine.path("requests")).unwrap();
    assert!(requests.lines().any(|line| line.ends_with("/pools")));
    assert!(
        requests
            .lines()
            .any(|line| line.ends_with("/dynamic/allocations"))
    );
    assert!(requests.lines().all(|line| matches!(
        line,
        "control-api http://localhost/pools" | "control-api http://localhost/dynamic/allocations"
    )));
}
