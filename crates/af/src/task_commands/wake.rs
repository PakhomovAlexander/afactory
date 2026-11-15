//! Explicit owner routes and finite token-free delivery. No Task runner is reachable here.
use super::*;
use af_contracts::task::wake::{delivery::*, *};
use af_store::store::wake::{CatchUp, WakeRegistration, WakeTraversalStage, delivery::*};
use clap::Subcommand;
use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::time::{Duration, Instant};

#[derive(Debug, Subcommand)]
pub(crate) enum WakeCommand {
    /// Show implementation capabilities; this never probes a live host or authenticates
    Capabilities,
    /// Explicit owner follow-up grant for one original interactive session and exact Task
    #[command(
        override_usage = "af task wake grant [OPTIONS] --host <HOST> --session <SESSION>\n       --executable <EXECUTABLE> --grant-file <GRANT_FILE> <TASK_ID>"
    )]
    Grant {
        /// Task whose exact opening the grant authorizes
        task_id: String,
        /// Original interactive host; Claude automatic wake remains unavailable
        #[arg(long, value_parser = ["pi", "codex", "claude"])]
        host: String,
        /// Original interactive session ID to bind to this grant
        #[arg(long)]
        session: String,
        /// Canonical installed native CLI (Codex) or interactive host interpreter (Pi)
        #[arg(long)]
        executable: PathBuf,
        /// Canonical installed production Pi extension
        #[arg(long)]
        extension: Option<PathBuf>,
        /// Canonical owner-selected local Codex daemon Unix socket
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Absent file in an owner-private directory outside the repository
        #[arg(long)]
        grant_file: PathBuf,
        /// Grant lifetime in seconds, from 1 to 604800
        #[arg(long, default_value_t = 86400, value_parser = clap::value_parser!(u64).range(1..=604800))]
        expires_secs: u64,
    },
    /// Catch up an existing Task from opening under an explicit owner route
    Register {
        /// Task to register for wake observations from its opening
        task_id: String,
        /// Owner-private grant file for this exact Store, Task and original session
        #[arg(long)]
        route: PathBuf,
    },
    /// Show pending, blocked, expired, historical source and independent receipt stages
    Inspect,
    /// Revoke a grant; no fallback session or endpoint is selected
    Revoke {
        /// Owner-private grant file naming the route to revoke
        #[arg(long)]
        route: PathBuf,
    },
    /// Finite foreground extraction/delivery; install no service and never respawn a Task
    Drain {
        /// Maximum source visits, transport claims and native route visits, from 1 to 128
        #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..=128))]
        max_batches: u32,
        /// Foreground driver wall-clock limit in seconds, from 1 to 300
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
    },
    /// Production Pi host pull: authenticated parent, exact session, one durable native intent
    PiNext {
        /// Owner-private grant file for this original Pi session
        #[arg(long)]
        route: PathBuf,
        /// Actual interactive Pi session ID, which must match the grant
        #[arg(long)]
        session: String,
    },
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |t| t.as_millis() as u64)
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn hash(value: serde_json::Value) -> Result<String, String> {
    af_store::content_id(&value).map_err(err)
}
fn random() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(err)?;
    hash(json!({"domain":"af/wake-nonce/1","nonce":bytes}))
}
fn private(path: &Path, directory: bool) -> Result<(), String> {
    let info = std::fs::symlink_metadata(path).map_err(err)?;
    if path.canonicalize().map_err(err)? != path
        || info.file_type().is_symlink()
        || info.is_dir() != directory
        || info.uid() != nix::unistd::geteuid().as_raw()
        || info.permissions().mode() & 0o077 != 0
        || (!directory && (!info.is_file() || info.nlink() != 1))
    {
        return Err("Wake authority requires a canonical owner-private path without links".into());
    }
    Ok(())
}
fn grant(path: &Path) -> Result<WakeGrantV1, String> {
    private(path.parent().ok_or("Missing grant directory")?, true)?;
    private(path, false)?;
    decode_bounded(&af_store::local_fs::read_bounded(path, MAX_WAKE_BYTES as u64).map_err(err)?)
}
fn digest_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let meta = std::fs::symlink_metadata(path).map_err(err)?;
    if !meta.is_file()
        || path.canonicalize().map_err(err)? != path
        || meta.permissions().mode() & 0o022 != 0
        || meta.len() > 256 * 1024 * 1024
        || (meta.uid() != 0 && meta.uid() != nix::unistd::geteuid().as_raw())
    {
        return Err("Wake installation must be canonical, bounded and owner-controlled".into());
    }
    let bytes = af_store::local_fs::read_bounded(path, 256 * 1024 * 1024).map_err(err)?;
    Ok(format!(
        "sha256:{}",
        af_contracts::hex::encode(&Sha256::digest(&bytes))
    ))
}
fn installation(route: &WakeRouteV1) -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Err("Wake peer provenance currently requires Linux".into());
    }
    route.validate()?;
    let i = &route.installation;
    if digest_file(Path::new(&i.executable))? != i.executable_digest
        || hash(serde_json::to_value(i).map_err(err)?)? != route.installation_id
    {
        return Err("Wake installation changed".into());
    }
    if let Some(extension) = &i.extension
        && Some(digest_file(Path::new(extension))?) != i.extension_digest
    {
        return Err("Wake Pi extension changed".into());
    }
    if let Some(socket) = &i.socket {
        use std::os::unix::fs::FileTypeExt;
        let path = Path::new(socket);
        private(path.parent().ok_or("Missing socket parent")?, true)?;
        // Linux pathname access is protected by this canonical owner-private directory.
        // Codex creates mode-0777 sockets there; the socket must still be owner-owned,
        // pinned by device/inode, and authenticated by SO_PEERCRED before native intent.
        let m = std::fs::symlink_metadata(path).map_err(err)?;
        if path.canonicalize().map_err(err)? != path
            || !m.file_type().is_socket()
            || m.uid() != nix::unistd::geteuid().as_raw()
            || Some(m.dev()) != i.socket_device
            || Some(m.ino()) != i.socket_inode
        {
            return Err("Wake endpoint substituted or unsafe".into());
        }
    }
    Ok(())
}
fn open(inspect: &crate::cli::TaskInspectArgs) -> Result<(PathBuf, Cas, EventStore), String> {
    if !cfg!(target_os = "linux") {
        return Err("Task wake routes currently require Linux".into());
    }
    let (_, state) = state_path(&inspect.repo, inspect.state.as_deref())?;
    require_store(&state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(err)?;
    let store = EventStore::open(state.join("events.sqlite")).map_err(err)?;
    Ok((state, cas, store))
}
fn session_store(create: bool) -> Result<EventStore, String> {
    // Existing inventory accounts this normal Task Store and retains its remainder.
    // It owns no business Task and can never dispatch work.
    let identity = hash(json!({"domain":"af/wake-machine-session-store/1"}))?;
    let path = crate::config::state_home()?.join("af/task/local").join(
        identity
            .strip_prefix("sha256:")
            .ok_or("Invalid machine wake identity")?,
    );
    let store = if create {
        crate::storage::owned::prepare(&path, false)?;
        Cas::open(path.join("cas")).map_err(err)?;
        let store = EventStore::open(path.join("events.sqlite")).map_err(err)?;
        store.admit_wake_incarnation().map_err(err)?;
        store
    } else {
        let store = EventStore::open_read_only(path.join("events.sqlite")).map_err(err)?;
        store.wake_incarnation().map_err(err)?;
        store
    };
    // SQLite FULL persists rows, not the Store's directory ancestry. Existence is not
    // durability: retry the complete publication chain even after an interrupted grant,
    // and before using a preexisting route. No binding or usable route escapes this barrier.
    private(&path, true)?;
    publish_session_store(&path)?;
    Ok(store)
}
fn publish_session_store(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let objects = path.join("cas/objects");
    private(&objects, true)?;
    for directory in objects.ancestors() {
        let sync = || -> Result<(), String> {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
                .open(directory)
                .map_err(err)?;
            // Debug fixtures can fail any particular barrier, including a preexisting
            // ancestor. Release builds expose neither failure nor recording controls.
            #[cfg(debug_assertions)]
            if std::env::var_os("AF_TEST_WAKE_FAIL_DIRECTORY_SYNC").as_deref()
                == Some(directory.as_os_str())
            {
                return Err("injected machine wake directory sync failure".into());
            }
            nix::unistd::fsync(&file).map_err(err)?;
            #[cfg(debug_assertions)]
            if let Some(record) = std::env::var_os("AF_TEST_WAKE_DIRECTORY_SYNCS") {
                let mut file = af_store::local_fs::private_file_options()
                    .append(true)
                    .create(true)
                    .custom_flags(nix::libc::O_NOFOLLOW)
                    .open(record)
                    .map_err(err)?;
                writeln!(file, "{}", directory.display()).map_err(err)?;
            }
            Ok(())
        };
        sync().map_err(|e| {
            format!(
                "syncing machine wake directory {}: {e}",
                directory.display()
            )
        })?;
    }
    Ok(())
}
fn check_session_store(route: &WakeRouteV1) -> Result<(), String> {
    session_store(false)?
        .check_wake_session_store(
            &session_key(route).map_err(err)?,
            &route.task.store_incarnation,
        )
        .map_err(err)
}
fn task_identity(
    store: &EventStore,
    cas: &Cas,
    id: &str,
) -> Result<(WakeTaskV1, WakeBoundaryV1), String> {
    let task = store
        .task_projection(cas, id)
        .map_err(err)?
        .ok_or("Unknown wake Task")?;
    let run = af_store::store::task::task_run_id(id).map_err(err)?;
    let opening = store
        .replay_window(&run, 0, 1)
        .map_err(err)?
        .pop()
        .ok_or("Missing Task opening")?;
    let prefix = store
        .replay_window(&run, task.next_sequence - 1, 1)
        .map_err(err)?
        .pop()
        .ok_or("Missing Task prefix")?;
    Ok((
        WakeTaskV1 {
            store_incarnation: store.wake_incarnation().map_err(err)?,
            task_id: id.into(),
            task_run_id: run,
            opening_event_id: opening.event_id,
        },
        WakeBoundaryV1 {
            sequence: prefix.sequence,
            event_id: prefix.event_id,
        },
    ))
}
pub(super) fn register(
    id: &str,
    inspect: &crate::cli::TaskInspectArgs,
    file: &Path,
    launch: bool,
) -> Result<WakeRegistration, String> {
    let (_, cas, store) = open(inspect)?;
    let grant = grant(file)?;
    let clock = now();
    let route = store.authenticate_wake_grant(&grant, clock).map_err(err)?;
    installation(&route)?;
    check_session_store(&route)?;
    let (task, prefix) = task_identity(&store, &cas, id)?;
    if route.task != task {
        return Err("Wake route belongs to another Store or Task opening".into());
    }
    let mut subscription = WakeSubscriptionV1 {
        schema: WAKE_SUBSCRIPTION_V1.into(),
        subscription_id: random()?,
        registration_key: random()?,
        configuration_digest: random()?,
        task,
        recipient_ref: route.route_id,
        registered_unix_ms: clock,
        expires_unix_ms: route.expires_unix_ms,
        max_observations: 128,
    };
    subscription.configuration_digest = af_store::store::wake::configuration_digest(&subscription)?;
    store
        .register_wake(
            &cas,
            &WakeRegistration {
                schema: "af/task-wake-registration@1".into(),
                subscription,
                policy: CatchUp::FromOpening,
                registered_prefix: prefix,
                launch_intent: launch,
            },
        )
        .map_err(err)
}

pub(super) fn child_arguments(args: impl Iterator<Item = OsString>) -> Vec<OsString> {
    let mut result = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if arg == "--wake-route" {
            skip = true;
            continue;
        }
        if arg == "--detach" || arg.to_string_lossy().starts_with("--wake-route=") {
            continue;
        }
        result.push(arg);
    }
    result
}
/// A mutable Workspace label cannot select a different Store after the parent registered.
/// Ordinary detach keeps its existing selectors; notification-enabled detach pins this path.
pub(super) fn pin_child_store(args: Vec<OsString>, state: &Path) -> Vec<OsString> {
    let mut result = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if arg == "--state" || arg == "--workspace" {
            skip = true;
            continue;
        }
        if arg.to_string_lossy().starts_with("--state=")
            || arg.to_string_lossy().starts_with("--workspace=")
        {
            continue;
        }
        result.push(arg);
    }
    result.push("--state".into());
    result.push(state.as_os_str().to_owned());
    result
}
/// Fixed trusted handler; arbitrary descriptions, diagnostics and next_action never enter it.
pub(super) fn message(packet: &WakeEnvelopeV1) -> String {
    format!(
        "AF Task wake metadata. Task: {}; Store: {}; receipt: {}. Inspect current Task evidence and report it under this session's existing authority. This observation may be historical, superseded or source-collected. Do not infer current completion from this wake. This wake authorizes no Task execution, resume, login, budget increase or publication.",
        packet.observation.task.task_id,
        packet.observation.task.store_incarnation,
        packet.delivery_id
    )
}
fn codex_queue_command(
    route: &WakeRouteV1,
    packet: &WakeEnvelopeV1,
) -> Result<std::process::Command, String> {
    let mut command = std::process::Command::new(&route.installation.executable);
    command.args([
        "queue",
        "--thread",
        &route.session,
        "--message",
        &message(packet),
        "--remote",
        &format!(
            "unix://{}",
            route.installation.socket.as_ref().ok_or("Missing socket")?
        ),
    ]);
    command.current_dir("/").env_remove(runs::DETACHED_ENV);
    Ok(command)
}
#[cfg(any(target_os = "linux", test))]
fn check_native_wake_peer(uid: u32) -> Result<(), String> {
    if uid != nix::unistd::geteuid().as_raw() {
        return Err("Wrong native wake peer".into());
    }
    Ok(())
}
fn codex(store: &EventStore, route: &WakeRouteV1, remaining: Duration) -> Result<(), String> {
    if remaining.is_zero() {
        return Err("Wake driver wall limit reached before native admission".into());
    }
    let Some(claim) = store.reserve_wake_native(route, now()).map_err(err)? else {
        return Ok(());
    };
    let preflight = || -> Result<bool, String> {
        #[cfg(debug_assertions)]
        runs::observation_barrier("wake-native-preflight")?;
        if !store
            .check_reserved_wake_native(route, &claim, now())
            .map_err(err)?
        {
            return Ok(false);
        }
        installation(route)?;
        check_session_store(route)?;
        // Authenticate the selected local peer before committing native intent. The queue
        // CLI reconnects to the same pinned socket; hostile same-uid owners are out of scope.
        #[cfg(target_os = "linux")]
        {
            let socket = rustix::net::socket_with(
                rustix::net::AddressFamily::UNIX,
                rustix::net::SocketType::STREAM,
                rustix::net::SocketFlags::NONBLOCK | rustix::net::SocketFlags::CLOEXEC,
                None,
            )
            .map_err(err)?;
            let address = rustix::net::SocketAddrUnix::new(
                route
                    .installation
                    .socket
                    .as_ref()
                    .ok_or("Missing native socket")?,
            )
            .map_err(err)?;
            rustix::net::connect(&socket, &address).map_err(err)?;
            let credentials = rustix::net::sockopt::socket_peercred(&socket).map_err(err)?;
            check_native_wake_peer(credentials.uid.as_raw())?;
        }
        Ok(true)
    };
    match preflight() {
        Ok(true) => {}
        Ok(false) => return Ok(()),
        Err(reason) => {
            store.defer_wake_native(&claim, now()).map_err(err)?;
            return Err(reason);
        }
    }
    let Some(record) = store
        .take_reserved_wake_native(route, &claim, now())
        .map_err(err)?
    else {
        return Ok(());
    };
    let mut command = codex_queue_command(route, &record.envelope)?;
    let output = af_process::run_supervised_captured_bounded(
        &mut command,
        None,
        remaining.min(Duration::from_secs(5)),
        af_process::ExitPolicy::KillProcessGroup,
        None,
        4096,
    );
    // Queue output has no established turn/report or idempotency association. Even exit 0
    // remains admission_unknown. Never log native output, which may contain private data.
    if !output.status.is_ok_and(|s| s.success()) {
        eprintln!("wake: native request uncertain; inspect receipts; no automatic resend");
    }
    Ok(())
}
type RecorderSnapshot = BTreeMap<PathBuf, Vec<u8>>;
fn recorder_snapshot(state: &Path, id: &str) -> Result<RecorderSnapshot, String> {
    let directory = state.join("runs");
    if !directory.exists() {
        return Ok(BTreeMap::new());
    }
    private(&directory, true)?;
    let mut snapshot = BTreeMap::new();
    let mut bytes = 0;
    for entry in std::fs::read_dir(&directory).map_err(err)? {
        let entry = entry.map_err(err)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err("Invalid recorder path".into());
        };
        // Task IDs may contain hyphens. Find the complete filename boundary before
        // deciding whether its run number or contents belong to this Task.
        let Some((stem, kind)) = name.split_once('.') else {
            continue;
        };
        if kind != "json" && kind != "cancel.json" {
            continue;
        }
        let Some((task_id, _)) = stem.rsplit_once('-') else {
            continue;
        };
        if task_id != id {
            continue;
        }
        if runs::run_of(id, name).is_none() {
            return Err("Invalid wake recorder identity".into());
        }
        private(&entry.path(), false)?;
        let raw = af_store::local_fs::read_bounded(&entry.path(), 65536).map_err(err)?;
        bytes += raw.len();
        if snapshot.len() >= 4096 || bytes > 16 * 1024 * 1024 {
            return Err("Wake recorder snapshot exceeds bounds".into());
        }
        snapshot.insert(entry.path(), raw);
    }
    Ok(snapshot)
}
fn recorder_facts(
    snapshot: &RecorderSnapshot,
    id: &str,
) -> Result<Vec<af_runtime::wake::RecordedRunFacts>, String> {
    use af_contracts::task::agent_loop::{TaskCancelRequestV1, TaskRunProcessV1};
    let mut processes = BTreeMap::new();
    let mut cancellations = BTreeMap::new();
    for (path, raw) in snapshot {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid recorder name")?;
        if name.ends_with(".cancel.json") {
            let c: TaskCancelRequestV1 = serde_json::from_slice(raw).map_err(err)?;
            c.validate()?;
            if c.task_id != id || name != format!("{id}-{}.cancel.json", c.run) {
                return Err("Foreign cancellation recorder".into());
            }
            cancellations.insert(c.run, c);
        } else {
            let p: TaskRunProcessV1 = serde_json::from_slice(raw).map_err(err)?;
            p.validate()?;
            if p.task_id != id || name != format!("{id}-{}.json", p.run) {
                return Err("Foreign process recorder".into());
            }
            processes.insert(p.run, p);
        }
    }
    cancellations
        .into_iter()
        .map(|(run, cancellation)| {
            let process = processes
                .remove(&run)
                .ok_or("Cancellation without original process recorder")?;
            Ok(af_runtime::wake::RecordedRunFacts {
                process,
                cancellation: Some(cancellation),
            })
        })
        .collect()
}
fn reconcile_registered(
    store: &EventStore,
    cas: &Cas,
    state: &Path,
    r: &WakeRegistration,
) -> Result<bool, af_store::StoreError> {
    let snapshot = recorder_snapshot(state, &r.subscription.task.task_id)
        .map_err(af_store::StoreError::Conflict)?;
    let facts = recorder_facts(&snapshot, &r.subscription.task.task_id)
        .map_err(af_store::StoreError::Conflict)?;
    af_runtime::wake::journal::reconcile_with_run_facts_observed(
        store,
        cas,
        &r.subscription.subscription_id,
        now(),
        &facts,
        || {
            #[cfg(debug_assertions)]
            runs::observation_barrier("wake-recorder-check")
                .map_err(af_store::StoreError::Conflict)?;
            let current = recorder_snapshot(state, &r.subscription.task.task_id)
                .map_err(af_store::StoreError::Conflict)?;
            let current_facts = recorder_facts(&current, &r.subscription.task.task_id)
                .map_err(af_store::StoreError::Conflict)?;
            af_runtime::wake::validate_current_run_facts(
                store,
                cas,
                &r.subscription,
                &current_facts,
            )?;
            if current != snapshot {
                return Err(af_store::StoreError::WakeReconcileChanged);
            }
            Ok(())
        },
        |point| {
            #[cfg(debug_assertions)]
            runs::observation_barrier(match point {
                af_runtime::wake::ReconcilePoint::FinalStateCaptured => "wake-final-state-captured",
                af_runtime::wake::ReconcilePoint::SourceObserved => "wake-source-observed",
                af_runtime::wake::ReconcilePoint::BeforeCommit => "wake-before-commit",
            })
            .map_err(af_store::StoreError::Conflict)?;
            let _ = point;
            Ok(())
        },
    )
}
fn print(value: &impl Serialize) -> Result<i32, String> {
    println!("{}", serde_json::to_string(value).map_err(err)?);
    Ok(0)
}

fn print_pi_next(
    session: &str,
    record: Option<&af_contracts::task::wake::delivery::WakeDeliveryV1>,
) -> Result<i32, String> {
    print(
        &json!({"schema":"af/task-wake-pi-next@1","session":session,"message":record.map(|r|message(&r.envelope)),
        "delivery_id":record.map(|r|&r.envelope.delivery_id),"admission":"unknown_if_packet_returned","turn_started":null,"reported":null}),
    )
}

pub(crate) fn execute(
    command: WakeCommand,
    inspect: &crate::cli::TaskInspectArgs,
) -> Result<i32, String> {
    if matches!(command, WakeCommand::Capabilities) {
        return print(
            &json!({"schema":"af/task-wake-capabilities@1","platform":"linux","driver":"explicit_finite_foreground",
            "pi":"implemented_original_interactive_follow_up_live_certification_pending",
            "codex":"implemented_existing_cli_queue_live_certification_pending","claude":"unavailable_live_tests_deferred",
            "native_idempotency":false,"turn_start_evidence":false,"report_evidence":false}),
        );
    }
    let (state, cas, store) = open(inspect)?;
    match command {
        WakeCommand::Capabilities => unreachable!(),
        WakeCommand::Grant {
            task_id,
            host,
            session,
            executable,
            extension,
            socket,
            grant_file,
            expires_secs,
        } => {
            if host == "claude" {
                return Err("Claude automatic wake unavailable; original-session live tests deferred; no login attempted".into());
            }
            let repo = inspect.repo.canonicalize().map_err(err)?;
            let parent = grant_file.parent().ok_or("Missing grant directory")?;
            private(parent, true)?;
            if parent.starts_with(&repo) || parent.starts_with(&state) {
                return Err("Wake grant must live outside repository and Store".into());
            }
            store.admit_wake_incarnation().map_err(err)?;
            let (task, _) = task_identity(&store, &cas, &task_id)?;
            let socket_meta = socket
                .as_deref()
                .map(std::fs::symlink_metadata)
                .transpose()
                .map_err(err)?;
            let i = WakeInstallationV1 {
                executable: executable.to_string_lossy().into(),
                executable_digest: digest_file(&executable)?,
                extension: extension.as_ref().map(|p| p.to_string_lossy().into()),
                extension_digest: extension.as_deref().map(digest_file).transpose()?,
                socket: socket.as_ref().map(|p| p.to_string_lossy().into()),
                socket_device: socket_meta.as_ref().map(|m| m.dev()),
                socket_inode: socket_meta.as_ref().map(|m| m.ino()),
            };
            let capability = random()?;
            let mut route = WakeRouteV1 {
                schema: "af/task-wake-route@1".into(),
                route_id: random()?,
                installation_id: hash(serde_json::to_value(&i).map_err(err)?)?,
                capability_digest: capability_digest(&capability).map_err(err)?,
                task,
                host: if host == "pi" {
                    WakeHostV1::Pi
                } else {
                    WakeHostV1::Codex
                },
                session,
                mode: "follow_up".into(),
                expires_unix_ms: now() + expires_secs * 1000,
                installation: i,
            };
            route.route_id = route_identity(&route).map_err(err)?;
            installation(&route)?;
            let grant = WakeGrantV1 {
                schema: "af/task-wake-grant@1".into(),
                route_id: route.route_id.clone(),
                capability,
            };
            // Persist private file before installing usable route. A crash can leave an inert
            // grant file, never an active secretless route. No overwrite or implicit recovery.
            let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(err)?;
            temp.write_all(&serde_json::to_vec(&grant).map_err(err)?)
                .map_err(err)?;
            temp.as_file().sync_all().map_err(err)?;
            temp.persist_noclobber(&grant_file).map_err(err)?;
            std::fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(err)?;
            session_store(true)?
                .bind_wake_session_store(
                    &session_key(&route).map_err(err)?,
                    &route.task.store_incarnation,
                )
                .map_err(err)?;
            store.install_wake_route(&route).map_err(err)?;
            print(
                &json!({"route_id":route.route_id,"expires_unix_ms":route.expires_unix_ms,"grant_file":grant_file,"live_certification":"pending"}),
            )
        }
        WakeCommand::Register { task_id, route } => {
            print(&register(&task_id, inspect, &route, false)?)
        }
        WakeCommand::Inspect => {
            let registrations = store.wake_registrations().map_err(err)?;
            let journals = registrations
                .iter()
                .map(|r| store.inspect_wake(&r.subscription.subscription_id))
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)?;
            print(
                &json!({"schema":"af/task-wake-status@1","driver":"no_service_installed_run_drain_explicitly",
                "observed_unix_ms":now(),"subscriptions":journals,"deliveries":store.wake_deliveries().map_err(err)?,
                "uncertain_session_fences":store.wake_uncertainty().map_err(err)?}),
            )
        }
        WakeCommand::Revoke { route } => {
            store
                .revoke_wake_route(&grant(&route)?, now())
                .map_err(err)?;
            print(&json!({"revoked":true}))
        }
        WakeCommand::Drain {
            max_batches,
            timeout,
        } => {
            let start = Instant::now();
            let wall = Duration::from_secs(timeout);
            store.maintain_wakes(now()).map_err(err)?;
            let mut pages = 0;
            let registrations = store.wake_registrations().map_err(err)?;
            let mut extraction = registrations
                .iter()
                .map(|r| r.subscription.subscription_id.clone())
                .collect::<BTreeSet<_>>();
            while pages < max_batches && start.elapsed() < wall {
                let Some(id) = store
                    .next_wake_target(WakeTraversalStage::Extraction, &extraction)
                    .map_err(err)?
                else {
                    break;
                };
                // Every visit consumes a page slot, including caught-up/expired sources.
                // The durable position advances before visiting, so bounded restarts are fair.
                pages += 1;
                let r = registrations
                    .iter()
                    .find(|r| r.subscription.subscription_id == id)
                    .ok_or("Unknown wake extraction target")?;
                let journal = store.inspect_wake(&id).map_err(err)?;
                if r.subscription.expires_unix_ms <= now()
                    || journal.closure.is_some()
                    || journal.expired
                    || !journal.source_available
                    || reconcile_registered(&store, &cas, &state, r).map_err(err)?
                {
                    extraction.remove(&id);
                }
            }
            let mut deliveries = 0;
            'outer: for r in &registrations {
                for envelope in store
                    .inspect_wake(&r.subscription.subscription_id)
                    .map_err(err)?
                    .envelopes
                {
                    if deliveries >= max_batches || start.elapsed() >= wall {
                        break 'outer;
                    }
                    let Some(claim) = store.claim_wake(&envelope, now()).map_err(err)? else {
                        continue;
                    };
                    deliveries += 1;
                    let admission = store
                        .wake_route(&claim.route_id, now())
                        .map_err(err)
                        .and_then(|route| {
                            installation(&route)?;
                            Ok(route)
                        });
                    match admission {
                        Ok(_route) => {
                            store.accept_wake(&claim, now()).map_err(err)?;
                        }
                        Err(_) => {
                            store.backoff_wake(&claim, now()).map_err(err)?;
                        }
                    }
                }
            }
            // Recover an already accepted inbox after driver loss BEFORE native intent.
            // A retained native intent itself is never dispatched again.
            let mut native_batches = 0;
            let mut routes = registrations
                .iter()
                .map(|r| r.subscription.recipient_ref.clone())
                .collect::<BTreeSet<_>>();
            while native_batches < max_batches && start.elapsed() < wall {
                let Some(id) = store
                    .next_wake_target(WakeTraversalStage::Native, &routes)
                    .map_err(err)?
                else {
                    break;
                };
                routes.remove(&id);
                // Empty, revoked, removed and expired routes still consume a bounded visit.
                // None can permanently starve later routes, even across separate invocations.
                native_batches += 1;
                let Ok(route) = store.wake_route(&id, now()) else {
                    continue;
                };
                if route.host != WakeHostV1::Codex {
                    continue;
                }
                if let Err(reason) = codex(&store, &route, wall.saturating_sub(start.elapsed())) {
                    eprintln!("wake: blocked: {reason}");
                }
            }
            print(
                &json!({"pages":pages,"deliveries":deliveries,"native_batches":native_batches,"driver":"foreground_drain_ended","pending_may_remain":true}),
            )
        }
        WakeCommand::PiNext { route, session } => {
            let route = store
                .authenticate_wake_grant(&grant(&route)?, now())
                .map_err(err)?;
            installation(&route)?;
            check_session_store(&route)?;
            if route.host != WakeHostV1::Pi || route.session != session {
                return Err("Wrong Pi original session".into());
            }
            // A protected capability plus authenticated Linux parent provenance, rather than
            // an environment session string. Host extension checks its actual session/mode.
            let parent = nix::unistd::getppid();
            let executable =
                std::fs::read_link(format!("/proc/{}/exe", parent.as_raw())).map_err(err)?;
            if executable != Path::new(&route.installation.executable)
                || digest_file(&executable)? != route.installation.executable_digest
            {
                return Err("Wrong Pi receiver peer".into());
            }
            // The explicitly attached host hook is also a bounded observation driver:
            // one registered source page and one local transport per poll. Persist the
            // visit across all registrations on this route, including caught-up sources
            // and packets in backoff. Restart cannot strand an earlier unclaimed packet
            // behind a later registration whose ID sorts first. Native order is unchanged.
            #[cfg(debug_assertions)]
            runs::observation_barrier("wake-pi-selection")?;
            if let Some(r) = store.next_wake_pi_target(&route, now()).map_err(err)? {
                let journal = store
                    .inspect_wake(&r.subscription.subscription_id)
                    .map_err(err)?;
                if journal.closure.is_none() && !journal.expired {
                    if journal.source_available {
                        match reconcile_registered(&store, &cas, &state, &r) {
                            Ok(_) => {}
                            // Only known pre-native observation/CAS contention is deferred.
                            // End this poll before transport or native intent; the attachment's
                            // existing finite polling allowance owns any later attempt.
                            Err(af_store::StoreError::WakeReconcileDeferred) => {
                                if store.wake_route(&route.route_id, now()).map_err(err)? != route {
                                    return Err("Pi wake route changed".into());
                                }
                                installation(&route)?;
                                check_session_store(&route)?;
                                return print_pi_next(&session, None);
                            }
                            Err(error) => return Err(err(error)),
                        }
                    }
                    for packet in store
                        .inspect_wake(&r.subscription.subscription_id)
                        .map_err(err)?
                        .envelopes
                    {
                        if let Some(claim) = store.claim_wake(&packet, now()).map_err(err)? {
                            store.accept_wake(&claim, now()).map_err(err)?;
                            break;
                        }
                    }
                }
            }
            let record = store.take_wake_native(&route, now()).map_err(err)?;
            print_pi_next(&session, record.as_ref())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_recorder_snapshot_selects_exact_task_before_private_bounded_validation() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("runs");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Unrelated paths must never be opened or validated as this Task's recorder.
        for name in [
            "build-fix-1.json",
            "build-fix-1.cancel.json",
            "build-1-2.json",
        ] {
            std::fs::write(directory.join(name), "unrelated").unwrap();
            std::fs::set_permissions(directory.join(name), std::fs::Permissions::from_mode(0o644))
                .unwrap();
        }
        assert!(recorder_snapshot(root.path(), "build").unwrap().is_empty());
        for name in [
            "build-0.json",
            "build-01.cancel.json",
            "build-x.json",
            "build-18446744073709551616.json",
        ] {
            std::fs::write(directory.join(name), "invalid selected number").unwrap();
            assert!(
                recorder_snapshot(root.path(), "build")
                    .unwrap_err()
                    .contains("recorder identity")
            );
            std::fs::remove_file(directory.join(name)).unwrap();
        }
        std::fs::write(directory.join("build-1.json"), "corrupt selected process").unwrap();
        std::fs::set_permissions(
            directory.join("build-1.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let snapshot = recorder_snapshot(root.path(), "build").unwrap();
        assert_eq!(snapshot.len(), 1);
        assert!(recorder_facts(&snapshot, "build").is_err());
    }

    #[test]
    fn wake_prelease_process_is_validated_without_inventing_lifecycle_authority() {
        use af_contracts::task::agent_loop::*;
        let mut process = TaskRunProcessV1 {
            schema: TASK_RUN_PROCESS_V1.into(),
            task_id: "build".into(),
            run: 1,
            pid: 123,
            host: "fixture".into(),
            writer: "not-yet-leased".into(),
            started_unix_ms: 1,
            detached: false,
            log: None,
        };
        let path = PathBuf::from("build-1.json");
        let snapshot =
            |p: &TaskRunProcessV1| BTreeMap::from([(path.clone(), serde_json::to_vec(p).unwrap())]);
        process.validate().unwrap();
        assert!(
            recorder_facts(&snapshot(&process), "build")
                .unwrap()
                .is_empty()
        );
        process.task_id = "another-task".into();
        assert!(recorder_facts(&snapshot(&process), "build").is_err());
        process.task_id = "build".into();
        process.run = 2;
        assert!(recorder_facts(&snapshot(&process), "build").is_err());
    }

    #[test]
    fn wake_codex_foreign_peer_refuses_even_with_an_admitted_endpoint() {
        let owner = nix::unistd::geteuid().as_raw();
        assert!(check_native_wake_peer(owner).is_ok());
        assert_eq!(
            check_native_wake_peer(owner.wrapping_add(1)),
            Err("Wrong native wake peer".into())
        );
    }

    #[test]
    fn wake_child_never_inherits_route_or_registers_again() {
        for route in [
            vec!["--wake-route", "/private/grant"],
            vec!["--wake-route=/private/grant"],
        ] {
            let mut args = vec!["task", "run", "example", "--execute", "--detach"];
            args.extend(route);
            assert_eq!(
                child_arguments(args.into_iter().map(OsString::from)),
                ["task", "run", "example", "--execute"].map(OsString::from)
            );
        }
        let pinned = pin_child_store(
            ["task", "run", "example", "--workspace", "mutable-label"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            Path::new("/owner/exact-store"),
        );
        assert_eq!(
            pinned,
            ["task", "run", "example", "--state", "/owner/exact-store"].map(OsString::from)
        );
    }

    #[test]
    fn wake_codex_native_protocol_is_recorded_without_login_resume_or_sdk() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("native-queue-standin");
        std::fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let task = WakeTaskV1 {
            store_incarnation: digest.clone(),
            task_id: "example".into(),
            task_run_id: format!("task:{digest}"),
            opening_event_id: "0123456789abcdef0123456789".into(),
        };
        let boundary = WakeBoundaryV1 {
            sequence: 1,
            event_id: task.opening_event_id.clone(),
        };
        let packet = WakeEnvelopeV1 {
            schema: WAKE_ENVELOPE_V1.into(),
            subscription_id: digest.clone(),
            delivery_id: digest.clone(),
            payload_digest: digest.clone(),
            expires_unix_ms: 1000,
            observation: WakeObservationV1 {
                schema: WAKE_OBSERVATION_V1.into(),
                task: task.clone(),
                observation_id: digest.clone(),
                causal: boundary.clone(),
                checked_prefix: boundary,
                revision_id: digest.clone(),
                episode: 1,
                kind: WakeKindV1::Completed,
                state: WakeStateV1 {
                    phase: af_contracts::task::state::TaskStatePhaseV1::Finished,
                    outcome: Some(af_contracts::task::state::TaskOutcomeV1::Verified),
                    cause: None,
                    waiting_reason: None,
                },
                evidence: WakeEvidenceV1::History {},
                observed_unix_ms: 100,
                result_id: Some(digest.clone()),
            },
        };
        let route = WakeRouteV1 {
            schema: "af/task-wake-route@1".into(),
            route_id: digest.clone(),
            installation_id: digest.clone(),
            capability_digest: digest.clone(),
            task,
            host: WakeHostV1::Codex,
            session: "original-thread".into(),
            mode: "follow_up".into(),
            expires_unix_ms: 1000,
            installation: WakeInstallationV1 {
                executable: binary.to_string_lossy().into(),
                executable_digest: digest,
                extension: None,
                extension_digest: None,
                socket: Some("/owner/native.sock".into()),
                socket_device: Some(1),
                socket_inode: Some(2),
            },
        };
        let mut command = codex_queue_command(&route, &packet).unwrap();
        let output = af_process::run_supervised_captured_bounded(
            &mut command,
            None,
            Duration::from_secs(5),
            af_process::ExitPolicy::KillProcessGroup,
            None,
            4096,
        );
        assert!(output.status.unwrap().success());
        let text = String::from_utf8(output.stdout).unwrap();
        let argv: Vec<_> = text.lines().collect();
        assert_eq!(
            &argv[..4],
            ["queue", "--thread", "original-thread", "--message"]
        );
        assert_eq!(argv.len(), 7);
        assert_eq!(argv[4], message(&packet));
        assert_eq!(&argv[5..], ["--remote", "unix:///owner/native.sock"]);
        assert!(!output.truncation.is_truncated());
    }
}
