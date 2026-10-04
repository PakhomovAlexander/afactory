//! An independent owner for one native login and its inherited auth-context flock.
//!
//! The ordinary af owner can disappear without running Drop. Its dedicated stdin pipe then
//! closes; this guard kills and reaps the native process group before releasing that same lock.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use super::super::{
    ProviderKind, ProviderSpec, bind_directory, bound_directory_is_current, locate_cli,
    sanitized_path, set_nonblocking, stop_probe, validate_private_auth_directory,
};

const MAX_INPUT: usize = 16 * 1024;
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_STDERR: usize = 64 * 1024;
const MAX_LIFETIME: Duration = Duration::from_secs(900);
const POLL: Duration = Duration::from_millis(5);

/// Private guard exit codes, never native exit codes or bytes on either protocol stream.
/// These categories carry no native text and can never confer successful authentication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AuthenticationFailure {
    InvalidTokenResponse,
    ProxyConfiguration,
    TlsConfiguration,
    Rejected,
    Transport,
}

impl AuthenticationFailure {
    fn exit_code(self) -> i32 {
        match self {
            Self::InvalidTokenResponse => 70,
            Self::ProxyConfiguration => 71,
            Self::TlsConfiguration => 72,
            Self::Rejected => 73,
            Self::Transport => 74,
        }
    }

    pub(super) fn from_exit_code(code: i32) -> Option<Self> {
        match code {
            70 => Some(Self::InvalidTokenResponse),
            71 => Some(Self::ProxyConfiguration),
            72 => Some(Self::TlsConfiguration),
            73 => Some(Self::Rejected),
            74 => Some(Self::Transport),
            _ => None,
        }
    }
}

/// Pinned Codex 0.159.2 cli/src/login.rs, login/src/device_code_auth.rs and the
/// login/src/server.rs token exchange. Classify one anchored diagnostic only; never search
/// a response body, credential, URL or log line for a familiar substring. Uncharacterized or
/// multiple diagnostic lines fail closed. Dynamic suffixes are not inspected or returned.
fn classify_stderr(kind: ProviderKind, bytes: &[u8]) -> Option<AuthenticationFailure> {
    if kind != ProviderKind::Codex || bytes.len() > MAX_STDERR {
        return None;
    }
    let line = bytes.strip_suffix(b"\n")?;
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.iter().any(|byte| byte.is_ascii_control()) {
        return None;
    }
    let line = line.strip_prefix(b"Error logging in with device code: ")?;
    let line = line
        .strip_prefix(b"device code exchange failed: ")
        .unwrap_or(line);
    let prefix = |value: &[u8]| {
        line.strip_prefix(value)
            .is_some_and(|tail| tail.is_empty() || matches!(tail[0], b' ' | b':'))
    };
    if prefix(b"OAuth token response is invalid") {
        Some(AuthenticationFailure::InvalidTokenResponse)
    } else if prefix(b"Failed to configure outbound proxy selected for auth") {
        Some(AuthenticationFailure::ProxyConfiguration)
    } else if [
        b"Failed to build HTTP client with explicit TLS configuration:".as_slice(),
        b"Failed to read CA certificate file ",
        b"Failed to load CA certificates from ",
        b"Failed to parse certificate #",
        b"Failed to build HTTP client ",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
    {
        Some(AuthenticationFailure::TlsConfiguration)
    } else if let Some(tail) = line.strip_prefix(b"token endpoint returned status ") {
        (tail.len() >= 3
            && tail[..3].iter().all(u8::is_ascii_digit)
            && (tail.len() == 3 || matches!(tail[3], b' ' | b':')))
        .then_some(AuthenticationFailure::Rejected)
    } else if prefix(b"error sending request") {
        Some(AuthenticationFailure::Transport)
    } else {
        None
    }
}

#[derive(Default)]
struct PrivateStderr(Vec<u8>);

impl Drop for PrivateStderr {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Called before argument parsing, self dispatch, or any ordinary logging. The only inherited
/// stderr here is an already-locked file capability, never native output or OAuth material.
pub(crate) fn supervise_early(argv: &[String]) -> Option<i32> {
    if argv.get(1).map(String::as_str) != Some("provider")
        || argv.get(2).map(String::as_str) != Some("auth")
        || argv.get(3).map(String::as_str) != Some("supervise")
    {
        return None;
    }
    // Move the capability out of stderr first, even when the internal invocation is malformed.
    let inherited = duplicate_cloexec(std::io::stderr());
    let Ok(null) = File::options().write(true).open("/dev/null") else {
        return Some(6);
    };
    if nix::unistd::dup2_stderr(&null).is_err() {
        return Some(6);
    }
    let result = (|| {
        let inherited = inherited?;
        if argv.len() != 8 || argv[4] != "--kind" || argv[6] != "--auth-dir" {
            return Err(());
        }
        let kind = ProviderKind::parse(&argv[5]).map_err(|_| ())?;
        let auth_dir = Path::new(&argv[7]);
        if !auth_dir.is_absolute() {
            return Err(());
        }
        validate_private_auth_directory(auth_dir).map_err(|_| ())?;
        let directory = bind_directory(auth_dir, "auth directory").map_err(|_| ())?;
        retain_inherited_lock(&inherited, kind, auth_dir)?;
        let mut input = duplicate_cloexec(std::io::stdin())?;
        let mut output = duplicate_cloexec(std::io::stdout())?;
        for pipe in [&input, &output] {
            if !super::anonymous_pipe(pipe) {
                return Err(());
            }
        }
        let spec = ProviderSpec {
            id: "native-auth-guard".into(),
            kind,
            auth_dir: Some(auth_dir.to_path_buf()),
            explicit_selector: true,
            registry_declared: false,
            source: "private-auth-guard".into(),
        };
        let program = locate_cli(&spec).map_err(|_| ())?;
        let mut command = Command::new(program);
        super::configure_private_login_environment(&mut command, &spec, &sanitized_path());
        command.env("NO_COLOR", "1").env("TERM", "dumb");
        match kind {
            ProviderKind::Codex => {
                command.args(["login", "--device-auth"]);
            }
            ProviderKind::Claude => {
                command.args(["auth", "login", "--claudeai"]);
            }
        }
        relay(&mut command, kind, &mut input, &mut output, || {
            bound_directory_is_current(auth_dir, &directory).unwrap_or(false)
        })
    })();
    Some(result.unwrap_or(6))
}

fn duplicate_cloexec(fd: impl std::os::fd::AsFd) -> Result<File, ()> {
    rustix::io::fcntl_dupfd_cloexec(fd, 3)
        .map(File::from)
        .map_err(|_| ())
}

fn retain_inherited_lock(inherited: &File, kind: ProviderKind, auth: &Path) -> Result<(), ()> {
    use rustix::fs::{Mode, OFlags, open};
    let path = auth.join(format!(".af-{}-setup.lock", kind.name()));
    let named = std::fs::symlink_metadata(&path).map_err(|_| ())?;
    let held = inherited.metadata().map_err(|_| ())?;
    if !named.is_file()
        || named.file_type().is_symlink()
        || !held.is_file()
        || named.dev() != held.dev()
        || named.ino() != held.ino()
        || held.uid() != nix::unistd::geteuid().as_raw()
        || held.mode() & 0o077 != 0
        || held.nlink() != 1
    {
        return Err(());
    }
    let independent: File = open(
        &path,
        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| ())?
    .into();
    let independent_metadata = independent.metadata().map_err(|_| ())?;
    if independent_metadata.dev() != held.dev() || independent_metadata.ino() != held.ino() {
        return Err(());
    }
    // A freshly opened/unlocked file cannot manufacture the inherited grant. Another OFD
    // must be blocked, while the transferred OFD already owns this exact exclusive lock.
    match fs2::FileExt::try_lock_exclusive(&independent) {
        Err(error) if error.kind() == ErrorKind::WouldBlock => {}
        _ => return Err(()),
    }
    fs2::FileExt::try_lock_exclusive(inherited).map_err(|_| ())
}

struct NativeChild {
    child: Child,
    watch: review_process::ExitWatch,
    exit: Option<ExitStatus>,
}

impl Drop for NativeChild {
    fn drop(&mut self) {
        if self.exit.is_none() {
            stop_probe(&mut self.child);
        }
    }
}

fn relay(
    command: &mut Command,
    kind: ProviderKind,
    parent_input: &mut File,
    parent_output: &mut File,
    context_current: impl Fn() -> bool,
) -> Result<i32, ()> {
    set_nonblocking(parent_input)
        .and_then(|()| set_nonblocking(parent_output))
        .map_err(|_| ())?;
    let mut to_native = Vec::new();
    let mut input_total = 0;
    if receive(parent_input, &mut to_native, &mut input_total, MAX_INPUT)? {
        return Err(());
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = review_process::spawn(command).map_err(|_| ())?;
    let mut native = NativeChild {
        watch: review_process::ExitWatch::new(child.id()),
        child,
        exit: None,
    };
    let mut input = native.child.stdin.take().ok_or(())?;
    let mut output = native.child.stdout.take().ok_or(())?;
    let mut stderr = native.child.stderr.take().ok_or(())?;
    set_nonblocking(&input)
        .and_then(|()| set_nonblocking(&output))
        .and_then(|()| set_nonblocking(&stderr))
        .map_err(|_| ())?;
    let deadline = Instant::now() + MAX_LIFETIME;
    let mut to_parent = Vec::new();
    let mut output_total = 0;
    let mut private_stderr = PrivateStderr::default();
    let mut stderr_total = 0;
    loop {
        if Instant::now() >= deadline || !context_current() {
            return Err(());
        }
        // Check owner death before any more native input/output. EOF is independent of whether
        // the official CLI notices its own stdin closure or is waiting for a browser callback.
        if receive(parent_input, &mut to_native, &mut input_total, MAX_INPUT)? {
            return Err(());
        }
        flush(&mut input, &mut to_native)?;
        // Drain both pipes on every iteration. Native diagnostics never enter to_parent;
        // an oversized stream terminates and reaps the group before the lock is released.
        let stderr_eof = receive(
            &mut stderr,
            &mut private_stderr.0,
            &mut stderr_total,
            MAX_STDERR,
        )?;
        let output_eof = receive(&mut output, &mut to_parent, &mut output_total, MAX_OUTPUT)?;
        flush(parent_output, &mut to_parent)?;
        if native.exit.is_none() {
            native.exit =
                review_process::try_reap_killing_group(&mut native.child, &mut native.watch)
                    .map_err(|_| ())?;
        }
        if let Some(status) = native.exit
            && output_eof
            && stderr_eof
            && to_parent.is_empty()
        {
            return Ok(if status.success() {
                0
            } else {
                classify_stderr(kind, &private_stderr.0)
                    .map(AuthenticationFailure::exit_code)
                    .unwrap_or(6)
            });
        }
        std::thread::sleep(POLL);
    }
}

/// Returns true only for EOF. The cumulative bound remains after pending bytes are forwarded.
fn receive(
    input: &mut impl Read,
    pending: &mut Vec<u8>,
    total: &mut usize,
    maximum: usize,
) -> Result<bool, ()> {
    let mut chunk = [0_u8; 4096];
    loop {
        match input.read(&mut chunk) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                *total = total.saturating_add(count);
                if *total > maximum {
                    return Err(());
                }
                pending.extend_from_slice(&chunk[..count]);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(()),
        }
    }
}

fn flush(output: &mut impl Write, pending: &mut Vec<u8>) -> Result<(), ()> {
    while !pending.is_empty() {
        match output.write(pending) {
            Ok(0) => return Err(()),
            Ok(count) => {
                pending.drain(..count);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::OpenOptionsExt;

    fn pipes() -> (File, File) {
        let (read, write) = std::io::pipe().unwrap();
        (
            std::os::fd::OwnedFd::from(read).into(),
            std::os::fd::OwnedFd::from(write).into(),
        )
    }

    fn lock(root: &Path) -> File {
        File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join(".af-codex-setup.lock"))
            .unwrap()
    }

    #[test]
    fn native_failure_categories_are_exact_anchored_and_never_return_native_tails() {
        use AuthenticationFailure::*;
        for (diagnostic, expected) in [
            (
                "OAuth token response is invalid: access_token=POISON",
                InvalidTokenResponse,
            ),
            (
                "Failed to configure outbound proxy selected for auth: https://POISON",
                ProxyConfiguration,
            ),
            (
                "Failed to build HTTP client with explicit TLS configuration: POISON",
                TlsConfiguration,
            ),
            (
                "Failed to read CA certificate file /POISON",
                TlsConfiguration,
            ),
            (
                "Failed to load CA certificates from /POISON",
                TlsConfiguration,
            ),
            ("Failed to parse certificate #1: POISON", TlsConfiguration),
            ("Failed to build HTTP client POISON", TlsConfiguration),
            (
                "token endpoint returned status 401 Unauthorized: access_token=POISON",
                Rejected,
            ),
            ("token endpoint returned status 500", Rejected),
            ("error sending request for url (https://POISON)", Transport),
        ] {
            for wrapper in [
                "Error logging in with device code: ",
                "Error logging in with device code: device code exchange failed: ",
            ] {
                let stderr = format!("{wrapper}{diagnostic}\n");
                assert_eq!(
                    classify_stderr(ProviderKind::Codex, stderr.as_bytes()),
                    Some(expected)
                );
                assert_eq!(
                    classify_stderr(ProviderKind::Claude, stderr.as_bytes()),
                    None
                );
                assert!(!format!("{expected:?}").contains("POISON"));
                assert_eq!(
                    AuthenticationFailure::from_exit_code(expected.exit_code()),
                    Some(expected)
                );
            }
        }
    }

    #[test]
    fn unknown_truncated_or_conflicting_diagnostics_do_not_guess() {
        for stderr in [
            "",
            "access_token=OAuth token response is invalid",
            "prefix OAuth token response is invalid",
            " OAuth token response is invalid",
            "OAuth token response is invalidish",
            "OAuth token response is inva",
            "Error logging in: OAuth token response is invalid",
            "device code exchange failed: Error logging in with device code: OAuth token response is invalid",
            "token endpoint returned status 40",
            "token endpoint returned status 4011",
            "token endpoint returned status 401POISON",
            "token endpoint returned status abc",
            "error sending requests",
            "authentication_invalid_token_response",
            "AF_GUARD_FAILURE=70",
            "\x1b[31mOAuth token response is invalid",
            "OAuth token response is invalid\nerror sending request for url (POISON)\n",
            "OAuth token response is invalid\nOAuth token response is invalid\n",
            "OAuth token response is invalid\naccess_token=POISON\n",
            "\nOAuth token response is invalid\n",
        ] {
            assert_eq!(
                classify_stderr(ProviderKind::Codex, stderr.as_bytes()),
                None
            );
        }
        assert_eq!(
            classify_stderr(
                ProviderKind::Codex,
                format!("OAuth token response is invalid{}", "x".repeat(MAX_STDERR)).as_bytes()
            ),
            None
        );
        for code in [0, 1, 6, 69, 75, 255] {
            assert_eq!(AuthenticationFailure::from_exit_code(code), None);
        }
    }

    /// Read the guard's stdout concurrently so both native pipes exceed kernel pipe capacity.
    /// A bounded cancellation path makes a drain regression fail without hanging the test.
    fn relay_fixture(script: &str) -> (Result<i32, ()>, Vec<u8>) {
        let (mut guard_input, _parent_input) = pipes();
        let (mut parent_output, mut guard_output) = pipes();
        let mut command = Command::new("/usr/bin/python3");
        command.env_clear().args(["-c", script]);
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancelled = std::sync::Arc::clone(&cancelled);
        let thread = std::thread::spawn(move || {
            relay(
                &mut command,
                ProviderKind::Codex,
                &mut guard_input,
                &mut guard_output,
                || !worker_cancelled.load(std::sync::atomic::Ordering::Acquire),
            )
        });
        set_nonblocking(&parent_output).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stdout = Vec::new();
        let mut total = 0;
        while !thread.is_finished() && Instant::now() < deadline {
            receive(&mut parent_output, &mut stdout, &mut total, MAX_OUTPUT).unwrap();
            std::thread::sleep(POLL);
        }
        let finished = thread.is_finished();
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        let result = thread.join().unwrap();
        receive(&mut parent_output, &mut stdout, &mut total, MAX_OUTPUT).unwrap();
        assert!(finished, "guard did not drain both native pipes");
        (result, stdout)
    }

    #[test]
    fn stderr_is_drained_privately_and_never_mixed_with_stdout() {
        let (result, stdout) = relay_fixture(
            "import os\nfor _ in range(15):\n os.write(2,b'POISON'*640)\n os.write(1,b'public'*640)\nraise SystemExit(1)",
        );
        assert_eq!(result, Ok(6));
        assert_eq!(stdout, b"public".repeat(640 * 15));
    }

    #[test]
    fn guard_synthesizes_failure_codes_only_from_failed_native_diagnostics() {
        for native_code in [1, 70, 71, 72, 73, 74] {
            let (result, stdout) = relay_fixture(&format!(
                "import os\nos.write(2,b'POISON')\nraise SystemExit({native_code})"
            ));
            assert_eq!(result, Ok(6));
            assert!(stdout.is_empty());
        }
        for (native_code, guard_code) in [(0, 0), (1, 70)] {
            let (result, stdout) = relay_fixture(&format!(
                "import os\nos.write(2,b'Error logging in with device code: device code exchange failed: OAuth token response is invalid: access_token=POISON\\n')\nraise SystemExit({native_code})"
            ));
            assert_eq!(result, Ok(guard_code));
            assert!(stdout.is_empty());
        }
    }

    #[test]
    fn oversized_stderr_cancels_and_reaps_native_before_returning() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("native-pid");
        let script = format!(
            "import os,time\nopen({:?},'w').write(str(os.getpid()))\nos.write(2,b'POISON'*{})\ntime.sleep(60)",
            marker.to_str().unwrap(),
            MAX_STDERR
        );
        let (result, stdout) = relay_fixture(&script);
        assert_eq!(result, Err(()));
        assert!(stdout.is_empty());
        let pid: i32 = std::fs::read_to_string(marker).unwrap().parse().unwrap();
        assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err());
    }

    #[test]
    fn guard_requires_an_already_held_exact_open_file_description() {
        let root = tempfile::tempdir().unwrap();
        let owner = lock(root.path());
        assert!(retain_inherited_lock(&owner, ProviderKind::Codex, root.path()).is_err());
        fs2::FileExt::lock_exclusive(&owner).unwrap();
        let inherited = duplicate_cloexec(&owner).unwrap();
        assert!(retain_inherited_lock(&inherited, ProviderKind::Codex, root.path()).is_ok());
        let independent = File::options()
            .read(true)
            .write(true)
            .open(root.path().join(".af-codex-setup.lock"))
            .unwrap();
        assert!(retain_inherited_lock(&independent, ProviderKind::Codex, root.path()).is_err());
        drop(owner);
        assert_eq!(
            fs2::FileExt::try_lock_exclusive(&independent)
                .unwrap_err()
                .kind(),
            ErrorKind::WouldBlock
        );
        drop(inherited);
        fs2::FileExt::try_lock_exclusive(&independent).unwrap();
    }

    #[test]
    fn owner_eof_reaps_a_native_that_ignores_stdin_before_releasing_the_lock() {
        let root = tempfile::tempdir().unwrap();
        let owner = lock(root.path());
        fs2::FileExt::lock_exclusive(&owner).unwrap();
        let inherited = duplicate_cloexec(&owner).unwrap();
        let independent = File::options()
            .read(true)
            .write(true)
            .open(root.path().join(".af-codex-setup.lock"))
            .unwrap();
        let marker = root.path().join("native-pid");
        let (mut guard_input, parent_input) = pipes();
        let (mut parent_output, mut guard_output) = pipes();
        let mut command = Command::new("/bin/sh");
        command
            .env_clear()
            .args([
                "-c",
                "printf '%s' \"$$\" > \"$1\"; printf ready; while :; do /bin/sleep 60; done",
                "fixture",
            ])
            .arg(&marker);
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancelled = std::sync::Arc::clone(&cancelled);
        let thread = std::thread::spawn(move || {
            let result = relay(
                &mut command,
                ProviderKind::Codex,
                &mut guard_input,
                &mut guard_output,
                || !worker_cancelled.load(std::sync::atomic::Ordering::Acquire),
            );
            drop(inherited);
            result
        });
        drop(owner);
        set_nonblocking(&parent_output).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut ready = Vec::new();
        let mut total = 0;
        while ready.len() < 5 {
            receive(&mut parent_output, &mut ready, &mut total, 64).unwrap();
            assert!(Instant::now() < deadline, "synthetic native did not start");
            std::thread::sleep(POLL);
        }
        assert_eq!(
            fs2::FileExt::try_lock_exclusive(&independent)
                .unwrap_err()
                .kind(),
            ErrorKind::WouldBlock
        );
        let pid = std::fs::read_to_string(marker)
            .unwrap()
            .parse::<i32>()
            .unwrap();
        drop(parent_input);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        let stopped_on_eof = thread.is_finished();
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        assert!(thread.join().unwrap().is_err());
        assert!(stopped_on_eof, "guard did not observe owner EOF");
        assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err());
        fs2::FileExt::try_lock_exclusive(&independent).unwrap();
    }

    #[test]
    fn a_dead_owner_cannot_start_a_native_login() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("unexpected-start");
        let (mut guard_input, parent_input) = pipes();
        let (_parent_output, mut guard_output) = pipes();
        drop(parent_input);
        let mut command = Command::new("/bin/sh");
        command
            .env_clear()
            .args(["-c", "printf unexpected > \"$1\"", "fixture"])
            .arg(&marker);
        assert!(
            relay(
                &mut command,
                ProviderKind::Codex,
                &mut guard_input,
                &mut guard_output,
                || true
            )
            .is_err()
        );
        assert!(!marker.exists());
    }
}
