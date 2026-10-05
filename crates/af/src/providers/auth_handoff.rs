//! Explicit private host handoff. Ordinary streams and durable state carry no login material.
//!
//! The host is the permission and private-recipient boundary. Its private inherited pipes must
//! outlive an agent turn; losing the host or this owner interrupts, never resurrects, a login.

use super::*;
use std::os::unix::fs::FileTypeExt;

use serde::{Deserialize, Serialize};

mod adapter;
mod guard;
pub(crate) use guard::supervise_early;

const SCHEMA: &str = "af/provider-auth@1";
const HOST_SCHEMA: &str = "af/provider-auth-host@1";
const STATE_FILE: &str = "session.json";
const COMMIT_LOCK: &str = "commit.lock";
const MAX_MESSAGE: usize = 16 * 1024;
const POLL: Duration = Duration::from_millis(25);

/// Login needs the same operator-selected network route/certificate policy as terminal
/// setup. Never carry ambient API keys, browser hooks, display state or project config.
fn configure_private_login_environment(
    command: &mut Command,
    spec: &ProviderSpec,
    path: &std::ffi::OsStr,
) {
    configure_probe_environment(command, spec, path);
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum State {
    AwaitingPermission,
    Starting,
    AwaitingUser,
    Completing,
    AuthenticatedUnverified,
    Cancelled,
    CancellationRequested,
    Expired,
    Interrupted,
    PermissionDenied,
    PrivateRouteUnavailable,
    Unsupported,
    ProviderCliMissing,
    AuthenticationFailed,
    AuthenticationInvalidTokenResponse,
    AuthenticationProxyConfigurationFailed,
    AuthenticationTlsConfigurationFailed,
    AuthenticationRejected,
    AuthenticationTransportFailed,
    InvalidChallenge,
    InvalidResponse,
    ContextChanged,
    RegistryConflict,
}

impl State {
    fn active(self) -> bool {
        matches!(
            self,
            Self::AwaitingPermission | Self::Starting | Self::AwaitingUser | Self::Completing
        )
    }

    fn exit_code(self) -> i32 {
        match self {
            Self::AuthenticatedUnverified | Self::CancellationRequested => EXIT_OK,
            Self::RegistryConflict => EXIT_REGISTRY_CONFLICT,
            Self::ProviderCliMissing => EXIT_PROVIDER_CLI_MISSING,
            Self::AuthenticationFailed
            | Self::AuthenticationInvalidTokenResponse
            | Self::AuthenticationProxyConfigurationFailed
            | Self::AuthenticationTlsConfigurationFailed
            | Self::AuthenticationRejected
            | Self::AuthenticationTransportFailed
            | Self::InvalidChallenge
            | Self::InvalidResponse
            | Self::ContextChanged => EXIT_AUTHENTICATION_FAILED,
            _ => EXIT_HUMAN_ACTION_REQUIRED,
        }
    }
}

/// Only closed states and opaque correlation references are serializable. No native text.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Session {
    schema: String,
    recovery_id: String,
    provider: String,
    kind: String,
    context_id: String,
    state: State,
    created_at: u64,
    host_deadline: u64,
    requester_ref: Option<String>,
    coordinator_ref: Option<String>,
    challenge_delivered: bool,
    response_consumed: bool,
    registered: bool,
}

impl Session {
    fn document(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": SCHEMA,
            "recovery_id": self.recovery_id,
            "provider": self.provider,
            "kind": self.kind,
            "state": self.state,
            "created_at": self.created_at,
            "host_deadline": self.host_deadline,
            "challenge_delivered": self.challenge_delivered,
            "registered": self.registered,
            "verified": false,
            "continuation": "not_authorized_by_login",
            "exit_code": self.state.exit_code(),
        })
    }

    fn emit(&self) -> i32 {
        println!("{}", self.document());
        self.state.exit_code()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Permission {
    schema: String,
    recovery_id: String,
    provider: String,
    context_id: String,
    requester_ref: String,
    coordinator_ref: String,
    approved: bool,
    private_delivery: bool,
    expires_at: u64,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Response {
    Delivered {
        recovery_id: String,
        requester_ref: String,
    },
    Code {
        recovery_id: String,
        requester_ref: String,
        code: String,
    },
    Cancel {
        recovery_id: String,
        requester_ref: String,
    },
}

struct Storage {
    path: PathBuf,
    directory: File,
    deadline: std::cell::Cell<Option<Instant>>,
}

impl Storage {
    fn open(auth: &Path, kind: ProviderKind, create: bool) -> Result<Self, String> {
        let path = auth.join(format!(".af-{}-auth-recovery", kind.name()));
        if create {
            create_private_directory_tree(&path, "auth recovery state", true)?;
        }
        let directory = bind_directory(&path, "auth recovery state")?;
        let metadata = directory
            .metadata()
            .map_err(|_| "cannot inspect auth recovery state")?;
        if metadata.mode() & 0o077 != 0 {
            return Err("auth recovery state must be private (0700)".into());
        }
        Ok(Self {
            path,
            directory,
            deadline: std::cell::Cell::new(None),
        })
    }

    fn current(&self) -> Result<(), String> {
        if !bound_directory_is_current(&self.path, &self.directory).unwrap_or(false) {
            return Err("auth recovery state directory changed".into());
        }
        Ok(())
    }

    fn read(&self) -> Result<Option<Session>, String> {
        use rustix::fs::{Mode, OFlags, openat};
        self.current()?;
        let file = match openat(
            &self.directory,
            STATE_FILE,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(file) => File::from(file),
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(_) => return Err("cannot open auth recovery state safely".into()),
        };
        let metadata = file
            .metadata()
            .map_err(|_| "cannot inspect auth recovery state file")?;
        if !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != nix::unistd::geteuid().as_raw()
            || metadata.len() > MAX_MESSAGE as u64
        {
            return Err("auth recovery state file is unsafe".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_MESSAGE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read auth recovery state")?;
        let session: Session =
            serde_json::from_slice(&bytes).map_err(|_| "invalid auth recovery state")?;
        if session.schema != SCHEMA
            || !opaque(&session.recovery_id)
            || !opaque(&session.context_id)
            || session.requester_ref.as_deref().is_some_and(|s| !opaque(s))
            || session
                .coordinator_ref
                .as_deref()
                .is_some_and(|s| !opaque(s))
        {
            return Err("invalid auth recovery state identity".into());
        }
        self.current()?;
        Ok(Some(session))
    }

    fn save(&self, session: &Session) -> Result<(), String> {
        self.current()?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.path)
            .map_err(|_| "cannot prepare auth recovery state")?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| "cannot secure auth recovery state")?;
        serde_json::to_writer(&mut temporary, session)
            .map_err(|_| "cannot encode auth recovery state")?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| "cannot sync auth recovery state")?;
        self.current()?;
        temporary
            .persist(self.path.join(STATE_FILE))
            .map_err(|_| "cannot publish auth recovery state")?;
        self.directory
            .sync_all()
            .map_err(|_| "cannot sync auth recovery directory")?;
        self.current()
    }

    fn cancelled(&self, recovery_id: &str) -> bool {
        self.path.join(format!("cancel-{recovery_id}")).exists()
    }

    /// Serializes cancellation with the owner's final registry publication. A cancel either
    /// lands before the owner's last check or reads the published result, never between them.
    fn commit_lock(&self) -> Result<File, String> {
        self.current()?;
        let file = open_lock_at(&self.directory, std::ffi::OsStr::new(COMMIT_LOCK))
            .map_err(|_| "cannot open auth recovery commit lock")?;
        fs2::FileExt::lock_exclusive(&file).map_err(|_| "cannot lock auth recovery commit")?;
        self.current()?;
        Ok(file)
    }
}

/// Uses exactly the lock used by terminal setup, but never queues another login behind it.
fn try_context_lock(kind: ProviderKind, auth: &Path) -> Result<Option<BoundDirectoryLock>, String> {
    let directory = bind_directory(auth, "auth directory")?;
    let name = format!(".af-{}-setup.lock", kind.name());
    let file = open_lock_at(&directory, std::ffi::OsStr::new(&name))
        .map_err(|_| "cannot open auth context lock")?;
    let metadata = file
        .metadata()
        .map_err(|_| "cannot inspect auth context lock")?;
    if !metadata.is_file()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err("auth context lock is unsafe".into());
    }
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => {
            let lock = BoundDirectoryLock {
                _lock: file,
                directory,
            };
            lock.ensure_directory_current(auth, "auth directory")?;
            Ok(Some(lock))
        }
        Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(None),
        Err(_) => Err("cannot lock auth context".into()),
    }
}

fn opaque(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn random_id() -> Result<String, String> {
    let mut bytes = [0; 32];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| "cannot obtain recovery entropy")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn context_id(kind: ProviderKind, auth: &Path) -> Result<String, String> {
    let metadata =
        fs::symlink_metadata(auth).map_err(|_| "cannot inspect auth context identity")?;
    let mut hash = Sha256::new();
    hash.update(b"af/provider-auth-context@1\0");
    hash.update(kind.name());
    hash.update(auth.as_os_str().as_encoded_bytes());
    hash.update(metadata.dev().to_le_bytes());
    hash.update(metadata.ino().to_le_bytes());
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn resolve(id: &str, kind: &str, auth: &Path, create: bool) -> Result<ProviderSpec, String> {
    validate_explicit_id(id)?;
    let kind = ProviderKind::parse(kind)?;
    let auth = resolve_auth_dir(kind, Some(auth), create)?;
    Ok(ProviderSpec {
        id: id.into(),
        kind,
        auth_dir: Some(auth),
        explicit_selector: true,
        registry_declared: false,
        source: "private_host_handoff".into(),
    })
}

fn matches_context(session: &Session, spec: &ProviderSpec) -> Result<(), String> {
    let auth = spec.auth_dir.as_deref().expect("explicit auth context");
    if session.provider != spec.id
        || session.kind != spec.kind.name()
        || session.context_id != context_id(spec.kind, auth)?
    {
        return Err("recovery session belongs to a different Provider or auth context".into());
    }
    Ok(())
}

pub fn status(id: &str, kind: &str, auth: &Path) -> Result<i32, String> {
    let spec = resolve(id, kind, auth, false)?;
    let auth = spec.auth_dir.as_deref().expect("explicit auth context");
    let storage = Storage::open(auth, spec.kind, false)?;
    let owner_absent = try_context_lock(spec.kind, auth)?;
    let mut session = storage.read()?.ok_or("no auth recovery session")?;
    matches_context(&session, &spec)?;
    if session.state.active() && owner_absent.is_some() {
        session.state = State::Interrupted;
    }
    Ok(session.emit())
}

pub fn cancel(id: &str, kind: &str, auth: &Path, recovery_id: &str) -> Result<i32, String> {
    if !opaque(recovery_id) {
        return Err("invalid recovery ID".into());
    }
    let spec = resolve(id, kind, auth, false)?;
    let auth = spec.auth_dir.as_deref().expect("explicit auth context");
    let storage = Storage::open(auth, spec.kind, false)?;
    let _commit = storage.commit_lock()?;
    let mut session = storage.read()?.ok_or("no auth recovery session")?;
    matches_context(&session, &spec)?;
    if session.recovery_id != recovery_id {
        return Err("stale recovery ID".into());
    }
    if !session.state.active() {
        return Ok(session.emit());
    }
    // No owner remains to observe a marker; report the session exactly as status does.
    if try_context_lock(spec.kind, auth)?.is_some() {
        session.state = State::Interrupted;
        return Ok(session.emit());
    }
    storage.current()?;
    use rustix::fs::{Mode, OFlags, openat};
    let name = format!("cancel-{recovery_id}");
    let file = openat(
        &storage.directory,
        name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    );
    match file {
        Ok(file) => File::from(file)
            .sync_all()
            .map_err(|_| "cannot sync auth cancellation")?,
        Err(rustix::io::Errno::EXIST) => {}
        Err(_) => return Err("cannot request auth cancellation".into()),
    }
    storage
        .directory
        .sync_all()
        .map_err(|_| "cannot sync auth cancellation directory")?;
    session.state = State::CancellationRequested;
    Ok(session.emit())
}

/// Two explicit inherited anonymous pipe capabilities, never ordinary standard streams.
/// The host creates them before spawn and passes only their descriptor numbers in argv.
struct Host {
    input_pipe: File,
    output_pipe: File,
    input: Vec<u8>,
}

// Linux exposes kernel-authored descriptor provenance. On macOS there is no equivalent
// supported proof here yet; fail closed rather than accepting a named FIFO as anonymous IPC.
#[cfg(target_os = "linux")]
fn anonymous_pipe(file: &File) -> bool {
    use std::os::fd::AsRawFd;
    fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned))
        .is_some_and(|target| {
            target
                .strip_prefix("pipe:[")
                .and_then(|s| s.strip_suffix(']'))
                .is_some_and(|inode| !inode.is_empty() && inode.bytes().all(|b| b.is_ascii_digit()))
        })
}

#[cfg(target_os = "macos")]
fn anonymous_pipe(_file: &File) -> bool {
    false
}

fn pipe_metadata(fd: impl std::os::fd::AsFd) -> std::io::Result<fs::Metadata> {
    // std Metadata provides the same unsigned device/inode interface on Linux and macOS.
    let duplicate = rustix::io::fcntl_dupfd_cloexec(fd, 3)?;
    File::from(duplicate).metadata()
}

impl Host {
    fn connect(read_fd: u32, write_fd: u32) -> Result<Self, State> {
        if read_fd <= 2
            || write_fd <= 2
            || read_fd == write_fd
            || read_fd > i32::MAX as u32
            || write_fd > i32::MAX as u32
        {
            return Err(State::PrivateRouteUnavailable);
        }
        // Opening /dev/fd duplicates an explicitly granted descriptor without unsafe code.
        // Validate the opened object, not pathname text: files, terminals and sockets are refused.
        let input_pipe = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(format!("/dev/fd/{read_fd}"))
            .map_err(|_| State::PrivateRouteUnavailable)?;
        let output_pipe = OpenOptions::new()
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(format!("/dev/fd/{write_fd}"))
            .map_err(|_| State::PrivateRouteUnavailable)?;
        let input = input_pipe
            .metadata()
            .map_err(|_| State::PrivateRouteUnavailable)?;
        let output = output_pipe
            .metadata()
            .map_err(|_| State::PrivateRouteUnavailable)?;
        if !input.file_type().is_fifo()
            || !output.file_type().is_fifo()
            || input.uid() != nix::unistd::geteuid().as_raw()
            || output.uid() != nix::unistd::geteuid().as_raw()
            || input.mode() & 0o077 != 0
            || output.mode() & 0o077 != 0
            || (input.dev() == output.dev() && input.ino() == output.ino())
        {
            return Err(State::PrivateRouteUnavailable);
        }
        if !anonymous_pipe(&input_pipe) || !anonymous_pipe(&output_pipe) {
            return Err(State::PrivateRouteUnavailable);
        }
        let standard = [
            pipe_metadata(std::io::stdin()),
            pipe_metadata(std::io::stdout()),
            pipe_metadata(std::io::stderr()),
        ];
        for standard in standard {
            let standard = standard.map_err(|_| State::PrivateRouteUnavailable)?;
            if [(&input, &input_pipe), (&output, &output_pipe)]
                .iter()
                .any(|(metadata, _)| {
                    metadata.dev() == standard.dev() && metadata.ino() == standard.ino()
                })
            {
                return Err(State::PrivateRouteUnavailable);
            }
        }
        // Consume the passed capabilities. Only these CLOEXEC duplicates remain, so native
        // provider children cannot read host permissions or impersonate private responses.
        nix::unistd::close(read_fd as i32).map_err(|_| State::PrivateRouteUnavailable)?;
        nix::unistd::close(write_fd as i32).map_err(|_| State::PrivateRouteUnavailable)?;
        Ok(Self {
            input_pipe,
            output_pipe,
            input: Vec::new(),
        })
    }

    fn send(&mut self, value: &serde_json::Value) -> Result<(), State> {
        let mut data = serde_json::to_vec(value).map_err(|_| State::PrivateRouteUnavailable)?;
        data.push(b'\n');
        if data.len() > MAX_MESSAGE {
            return Err(State::InvalidChallenge);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut written = 0;
        while written < data.len() {
            match self.output_pipe.write(&data[written..]) {
                Ok(0) => return Err(State::PrivateRouteUnavailable),
                Ok(count) => written += count,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(POLL)
                }
                Err(_) => return Err(State::PrivateRouteUnavailable),
            }
        }
        data.fill(0);
        Ok(())
    }

    fn read<T: for<'de> Deserialize<'de>>(&mut self) -> Result<Option<T>, State> {
        if let Some(end) = self.input.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.input.drain(..=end).collect();
            return serde_json::from_slice(&line)
                .map(Some)
                .map_err(|_| State::InvalidResponse);
        }
        let mut bytes = [0; 2048];
        match self.input_pipe.read(&mut bytes) {
            Ok(0) => Err(State::PrivateRouteUnavailable),
            Ok(count) => {
                self.input.extend_from_slice(&bytes[..count]);
                if self.input.len() > MAX_MESSAGE {
                    return Err(State::InvalidResponse);
                }
                // At most one buffered, bounded frame is consumed per poll.
                if self.input.contains(&b'\n') {
                    self.read()
                } else {
                    Ok(None)
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) =>
            {
                Ok(None)
            }
            Err(_) => Err(State::PrivateRouteUnavailable),
        }
    }
}

pub fn begin(
    id: &str,
    kind: &str,
    auth: &Path,
    read_fd: u32,
    write_fd: u32,
    timeout_secs: u64,
) -> Result<i32, String> {
    if !(30..=900).contains(&timeout_secs) {
        return Err("auth timeout must be between 30 and 900 seconds".into());
    }
    let spec = resolve(id, kind, auth, true)?;
    let auth = spec.auth_dir.as_deref().expect("explicit auth context");
    let storage = Storage::open(auth, spec.kind, true)?;
    storage
        .deadline
        .set(Some(Instant::now() + Duration::from_secs(timeout_secs)));
    let Some(lock) = try_context_lock(spec.kind, auth)? else {
        let session = storage
            .read()?
            .ok_or("auth context is busy with another setup")?;
        matches_context(&session, &spec)?;
        if !session.state.active() {
            return Err("auth context is busy with another setup".into());
        }
        return Ok(session.emit());
    };
    let registry = registry_path()?.ok_or("no provider registry path is available")?;
    let created_at = now();
    let mut session = Session {
        schema: SCHEMA.into(),
        recovery_id: random_id()?,
        provider: id.into(),
        kind: kind.into(),
        context_id: context_id(spec.kind, auth)?,
        state: State::AwaitingPermission,
        created_at,
        host_deadline: created_at + timeout_secs,
        requester_ref: None,
        coordinator_ref: None,
        challenge_delivered: false,
        response_consumed: false,
        registered: false,
    };
    storage.save(&session)?;
    {
        let mut check = || {
            control(&spec, &lock, &storage, &session)
                .map_err(|_| "auth recovery stopped while awaiting registry".to_string())
        };
        let registry_lock = registry_lock_controlled(&registry, Some(&mut check));
        let _registry_lock = match registry_lock {
            Ok(lock) => lock,
            Err(_) => {
                session.state = control(&spec, &lock, &storage, &session)
                    .err()
                    .unwrap_or(State::RegistryConflict);
                storage.save(&session)?;
                return Ok(session.emit());
            }
        };
        match inspect_registration(&registry, id, spec.kind, auth)? {
            Registration::Exact => session.registered = true,
            Registration::Absent => {}
            Registration::Conflict(_) => {
                session.state = State::RegistryConflict;
                storage.save(&session)?;
                return Ok(session.emit());
            }
        }
    }
    storage.save(&session)?;
    // No native process exists until a private host grants this exact one-time permission.
    let result = own(
        &spec,
        &registry,
        &lock,
        &storage,
        &mut session,
        (read_fd, write_fd),
    );
    session.state = result.unwrap_or_else(|state| state);
    storage.save(&session)?;
    Ok(session.emit())
}

fn own(
    spec: &ProviderSpec,
    registry: &Path,
    lock: &BoundDirectoryLock,
    storage: &Storage,
    session: &mut Session,
    pipes: (u32, u32),
) -> Result<State, State> {
    let mut host = Host::connect(pipes.0, pipes.1)?;
    host.send(&serde_json::json!({
        "schema": HOST_SCHEMA, "action":"request_permission", "recovery_id":session.recovery_id,
        "provider":session.provider, "kind":session.kind, "context_id":session.context_id,
        "host_deadline":session.host_deadline, "purpose":"provider_login_only",
    }))?;
    let permission: Permission = loop {
        control(spec, lock, storage, session)?;
        if let Some(permission) = host.read()? {
            break permission;
        }
        thread::sleep(POLL);
    };
    if permission.schema != HOST_SCHEMA
        || permission.recovery_id != session.recovery_id
        || permission.provider != spec.id
        || permission.context_id != session.context_id
        || !opaque(&permission.requester_ref)
        || !opaque(&permission.coordinator_ref)
        || !permission.approved
        || !permission.private_delivery
        || permission.expires_at <= now()
        || permission.expires_at > session.host_deadline
    {
        return Err(State::PermissionDenied);
    }
    session.requester_ref = Some(permission.requester_ref);
    session.coordinator_ref = Some(permission.coordinator_ref);
    session.host_deadline = permission.expires_at;
    let grant_deadline =
        Instant::now() + Duration::from_secs(permission.expires_at.saturating_sub(now()));
    storage.deadline.set(Some(
        storage
            .deadline
            .get()
            .map_or(grant_deadline, |deadline| deadline.min(grant_deadline)),
    ));
    session.state = State::Starting;
    storage.save(session).map_err(|_| State::ContextChanged)?;
    control(spec, lock, storage, session)?;
    let mut native = adapter::NativeLogin::start(spec, lock).map_err(native_failure)?;
    let mut challenge_sent = false;
    let mut completed = false;
    let mut queued_code: Option<String> = None;
    loop {
        control(spec, lock, storage, session)?;
        if !completed && let Some(event) = native.poll().map_err(native_failure)? {
            match event {
                adapter::Event::Challenge(challenge) => {
                    if challenge_sent {
                        return Err(State::InvalidChallenge);
                    }
                    let mode = match challenge.mode {
                        adapter::Mode::DeviceCode => "device_code",
                        adapter::Mode::CodeOrCallback => "code_or_callback",
                    };
                    host.send(&serde_json::json!({
                        "schema":HOST_SCHEMA, "action":"challenge", "recovery_id":session.recovery_id,
                        "provider":session.provider, "requester_ref":session.requester_ref,
                        "mode":mode, "url":challenge.url, "user_code":challenge.user_code,
                        "host_deadline":session.host_deadline,
                    }))?;
                    challenge_sent = true;
                    session.state = State::AwaitingUser;
                    storage.save(session).map_err(|_| State::ContextChanged)?;
                }
                adapter::Event::Completed => {
                    if !challenge_sent {
                        return Err(State::InvalidResponse);
                    }
                    completed = true;
                }
            }
        }
        // A browser callback can complete Claude while the host is queuing the user's code.
        // Completion with an acknowledged delivery finishes before any queued code is read.
        if completed && session.challenge_delivered {
            drop(native);
            return finish(spec, registry, lock, storage, session, &mut host);
        }
        // A received code reaches the native login only after this fresh poll found it running.
        if let Some(code) = queued_code.take() {
            native
                .submit_code(&code)
                .map_err(|_| State::InvalidResponse)?;
        }
        if let Some(response) = host.read::<Response>()? {
            let (recovery_id, requester_ref) = match &response {
                Response::Delivered {
                    recovery_id,
                    requester_ref,
                }
                | Response::Code {
                    recovery_id,
                    requester_ref,
                    ..
                }
                | Response::Cancel {
                    recovery_id,
                    requester_ref,
                } => (recovery_id, requester_ref),
            };
            if recovery_id != &session.recovery_id
                || Some(requester_ref) != session.requester_ref.as_ref()
            {
                return Err(State::InvalidResponse);
            }
            match response {
                Response::Delivered { .. } if challenge_sent && !session.challenge_delivered => {
                    session.challenge_delivered = true
                }
                Response::Code { code, .. }
                    if session.challenge_delivered && !session.response_consumed =>
                {
                    session.response_consumed = true;
                    session.state = State::Completing;
                    queued_code = Some(code);
                }
                Response::Cancel { .. } => return Err(State::Cancelled),
                _ => return Err(State::InvalidResponse),
            }
            storage.save(session).map_err(|_| State::ContextChanged)?;
        }
        thread::sleep(POLL);
    }
}

fn finish(
    spec: &ProviderSpec,
    registry: &Path,
    lock: &BoundDirectoryLock,
    storage: &Storage,
    session: &mut Session,
    host: &mut Host,
) -> Result<State, State> {
    // Native login completion is never model-usability evidence or execution authority.
    control(spec, lock, storage, session)?;
    if !authentication_ready(spec).map_err(|_| State::AuthenticationFailed)? {
        return Err(State::AuthenticationFailed);
    }
    control(spec, lock, storage, session)?;
    let auth = spec.auth_dir.as_deref().expect("explicit auth context");
    let mut check = || {
        control(spec, lock, storage, session)
            .map_err(|_| "auth recovery stopped while awaiting registry".to_string())
    };
    let registry_lock = registry_lock_controlled(registry, Some(&mut check));
    let _registry_lock = match registry_lock {
        Ok(lock) => lock,
        Err(_) => {
            control(spec, lock, storage, session)?;
            return Err(State::RegistryConflict);
        }
    };
    // From this last check to the durable result, a cancel waits and then reads that result.
    let commit = storage.commit_lock().map_err(|_| State::ContextChanged)?;
    control(spec, lock, storage, session)?;
    match inspect_registration(registry, &spec.id, spec.kind, auth)
        .map_err(|_| State::RegistryConflict)?
    {
        Registration::Exact => {}
        Registration::Absent if !session.registered => {}
        Registration::Absent | Registration::Conflict(_) => {
            // The operator removed or rebound an existing Provider during the browser wait.
            // This older login permission cannot recreate or replace that binding.
            session.registered = false;
            return Err(State::RegistryConflict);
        }
    }
    match add_to_registry_locked(registry, &spec.id, spec.kind, auth, true)
        .map_err(|_| State::RegistryConflict)?
    {
        RegistryAdd::Added(_) | RegistryAdd::AlreadyPresent => session.registered = true,
        RegistryAdd::Conflict(_) => return Err(State::RegistryConflict),
    }
    session.state = State::AuthenticatedUnverified;
    storage.save(session).map_err(|_| State::ContextChanged)?;
    drop(commit);
    // Durable result precedes notification. A disconnect cannot erase completion.
    let _ = host.send(&serde_json::json!({
        "schema":HOST_SCHEMA, "action":"setup_completed", "recovery_id":session.recovery_id,
        "coordinator_ref":session.coordinator_ref, "status":session.document(),
    }));
    Ok(State::AuthenticatedUnverified)
}

fn native_failure(failure: adapter::Failure) -> State {
    match failure {
        adapter::Failure::Unsupported => State::Unsupported,
        adapter::Failure::ProviderCliMissing => State::ProviderCliMissing,
        adapter::Failure::InvalidChallenge | adapter::Failure::OutputLimit => {
            State::InvalidChallenge
        }
        adapter::Failure::Failed => State::AuthenticationFailed,
        adapter::Failure::Authentication(failure) => match failure {
            guard::AuthenticationFailure::InvalidTokenResponse => {
                State::AuthenticationInvalidTokenResponse
            }
            guard::AuthenticationFailure::ProxyConfiguration => {
                State::AuthenticationProxyConfigurationFailed
            }
            guard::AuthenticationFailure::TlsConfiguration => {
                State::AuthenticationTlsConfigurationFailed
            }
            guard::AuthenticationFailure::Rejected => State::AuthenticationRejected,
            guard::AuthenticationFailure::Transport => State::AuthenticationTransportFailed,
        },
    }
}

fn control(
    spec: &ProviderSpec,
    lock: &BoundDirectoryLock,
    storage: &Storage,
    session: &Session,
) -> Result<(), State> {
    let auth = spec.auth_dir.as_deref().expect("explicit auth context");
    lock.ensure_directory_current(auth, "auth directory")
        .map_err(|_| State::ContextChanged)?;
    storage.current().map_err(|_| State::ContextChanged)?;
    if crate::interrupt::received().is_some() || storage.cancelled(&session.recovery_id) {
        return Err(State::Cancelled);
    }
    if now() >= session.host_deadline
        || storage
            .deadline
            .get()
            .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Err(State::Expired);
    }
    Ok(())
}
