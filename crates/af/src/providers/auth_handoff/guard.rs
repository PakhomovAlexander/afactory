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
const MAX_LIFETIME: Duration = Duration::from_secs(900);
const POLL: Duration = Duration::from_millis(5);

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
                command.args(["app-server", "--stdio"]);
            }
            ProviderKind::Claude => {
                command.args(["auth", "login", "--claudeai"]);
            }
        }
        relay(&mut command, &mut input, &mut output, || {
            bound_directory_is_current(auth_dir, &directory).unwrap_or(false)
        })
    })();
    Some(if matches!(result, Ok(status) if status.success()) {
        0
    } else {
        6
    })
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
    parent_input: &mut File,
    parent_output: &mut File,
    context_current: impl Fn() -> bool,
) -> Result<ExitStatus, ()> {
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
        .stderr(Stdio::null())
        .process_group(0);
    let child = review_process::spawn(command).map_err(|_| ())?;
    let mut native = NativeChild {
        watch: review_process::ExitWatch::new(child.id()),
        child,
        exit: None,
    };
    let mut input = native.child.stdin.take().ok_or(())?;
    let mut output = native.child.stdout.take().ok_or(())?;
    set_nonblocking(&input)
        .and_then(|()| set_nonblocking(&output))
        .map_err(|_| ())?;
    let deadline = Instant::now() + MAX_LIFETIME;
    let mut to_parent = Vec::new();
    let mut output_total = 0;
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
        let output_eof = receive(&mut output, &mut to_parent, &mut output_total, MAX_OUTPUT)?;
        flush(parent_output, &mut to_parent)?;
        if native.exit.is_none() {
            native.exit =
                review_process::try_reap_killing_group(&mut native.child, &mut native.watch)
                    .map_err(|_| ())?;
        }
        if let Some(status) = native.exit
            && output_eof
            && to_parent.is_empty()
        {
            return Ok(status);
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
            let result = relay(&mut command, &mut guard_input, &mut guard_output, || {
                !worker_cancelled.load(std::sync::atomic::Ordering::Acquire)
            });
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
        assert!(relay(&mut command, &mut guard_input, &mut guard_output, || true).is_err());
        assert!(!marker.exists());
    }
}
