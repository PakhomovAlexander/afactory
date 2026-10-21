//! The detached update check after an eligible command: scheduled with or without a terminal and
//! under `--json`, never changing what the command prints or returns, suppressed where the
//! environment or the policy says so, and scheduled once, by the af that dispatches, for a
//! command a project's pin runs; throttled by `check_every` even while the release source is
//! down or the state cannot be written, never waited for by a command while the versions lock is
//! held, and never undone by a terminal clearing the update notice; a check that another one
//! answered while it waited asks nothing, and a background install never holds the versions lock
//! across the release source nor overwrites a default the operator chose meanwhile, even one
//! rolled back to the version it read. An update notice is shown only while the default it
//! names stands: one a rollback made stale is cleared unshown, and so is one an update back to
//! the version it names superseded. A terminal command prints a notice once, to stderr and never
//! to stdout, which each terminal run here captures apart. A pinned
//! command shows the cached notice on a terminal before it dispatches, and the staging an
//! interrupted install leaves counts against the budget until the next install recovers it.
//! `af self uninstall` refuses while a background install runs, and neither a check that starts
//! after a purge nor a terminal command that finishes after it recreates anything. An update
//! notice whose clearing cannot be written stays unshown until it can. A pinned release never
//! runs while the pin that keeps it from removal cannot be recorded. Every release comes from a
//! directory source; nothing here reaches the network.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::common;
use common::{AF, Sandbox, Signer, TARGET, VERSION, err, out, write};

/// A release newer than the binary under test, as releases advance.
fn newer_release() -> String {
    let current = semver::Version::parse(VERSION).unwrap();
    semver::Version::new(current.major, current.minor + 1, 0).to_string()
}

fn cache_file(sandbox: &Sandbox) -> PathBuf {
    sandbox.path("cache/af/self/latest.toml")
}

/// The first line of every cache this release writes and reads.
const CACHE_SCHEMA: &str = "schema = \"af/self-check@2\"\n";

/// The release the cached "updated the default" notice names, when the cache holds one.
fn auto_updated_to(cache: &str) -> Option<String> {
    let document: toml::Value = toml::from_str(cache).ok()?;
    document
        .get("auto_updated")?
        .get("to")?
        .as_str()
        .map(str::to_string)
}

fn config_file(sandbox: &Sandbox) -> PathBuf {
    sandbox.path("config/af/config.toml")
}

fn check_log(sandbox: &Sandbox) -> PathBuf {
    sandbox.path("checks.log")
}

/// How many detached checks the commands so far spawned, as the debug build logs each one when
/// it spawns it: "not scheduled" is known when the command returns, without waiting.
fn scheduled(sandbox: &Sandbox) -> usize {
    std::fs::read_to_string(check_log(sandbox))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// `binary` with `args`, headless: captured pipes, no terminal.
fn headless(sandbox: &Sandbox, binary: &Path, cwd: &Path, args: &[&str]) -> Output {
    sandbox
        .command(binary)
        .args(args)
        .current_dir(cwd)
        .env("AF_TEST_SELF_CHECK_LOG", check_log(sandbox))
        .output()
        .unwrap()
}

/// Wait, within a bound, for the detached check to leave a cache `done` accepts.
fn wait_for_cache(sandbox: &Sandbox, what: &str, done: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let text = std::fs::read_to_string(cache_file(sandbox)).unwrap_or_default();
        if done(&text) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "the detached check never recorded {what}: {text:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wait, within a bound, until no detached check holds the check lock: one that wrote its cache
/// is about to exit and release it.
fn wait_for_check_to_exit(sandbox: &Sandbox) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cache_file(sandbox).with_extension("lock"))
        .unwrap();
    while rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_err() {
        assert!(
            Instant::now() < deadline,
            "the detached check never released its lock"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wait, within a bound, for the detached check to record `latest` in the cache.
fn wait_for_latest(sandbox: &Sandbox, latest: &str) -> String {
    let expected = format!("latest = \"{latest}\"");
    wait_for_cache(sandbox, latest, |text| text.contains(&expected))
}

/// When the cache says the last check ran, 0 for none.
fn checked_at(cache: &str) -> u64 {
    cache
        .lines()
        .find_map(|line| line.strip_prefix("checked_at_epoch = "))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A cache that checked just now and found `latest`, so no command is due to check again, beside
/// the check lock's file the command that scheduled that check created: a notice is read only
/// under that lock, which no notice creates.
fn fresh_cache(sandbox: &Sandbox, latest: &str) {
    let now = epoch_now();
    write(
        &cache_file(sandbox),
        &format!(
            "{CACHE_SCHEMA}checked_at_epoch = {now}\nlatest = \"{latest}\"\nchannel = \"stable\"\n"
        ),
    );
    write(&cache_file(sandbox).with_extension("lock"), "");
}

fn storage_document(output: &Output) -> serde_json::Value {
    assert!(output.status.success(), "{}", err(output));
    let document: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("stdout is not JSON ({error}): {}", out(output)));
    assert_eq!(document["schema"], "af/storage@3");
    document
}

/// `args` run by the binary under test with stderr on a terminal, read until the command exits
/// and closes it, and stdout in a file of its own: what each stream carries is seen apart.
/// Returns the exit code, what the terminal showed and what stdout wrote.
fn on_terminal(sandbox: &Sandbox, args: &[&str]) -> (u32, String, String) {
    on_terminal_in(sandbox, &sandbox.path("home"), args)
}

/// [`on_terminal`] in the directory `cwd`.
fn on_terminal_in(sandbox: &Sandbox, cwd: &Path, args: &[&str]) -> (u32, String, String) {
    program_on_terminal(sandbox, cwd, Path::new(AF), args)
}

/// [`on_terminal`] for `program` with `args` in the directory `cwd`.
fn program_on_terminal<S: AsRef<OsStr>>(
    sandbox: &Sandbox,
    cwd: &Path,
    program: &Path,
    args: &[S],
) -> (u32, String, String) {
    let streams = tempfile::tempdir().unwrap();
    let stdout = streams.path().join("stdout");
    let (code, shown) = OnTerminal::start(sandbox, cwd, &stdout, program, args).finish();
    let written = std::fs::read(&stdout).unwrap_or_else(|error| {
        panic!(
            "{} was never written ({error}); the terminal showed: {shown}",
            stdout.display()
        )
    });
    (code, shown, String::from_utf8_lossy(&written).into_owned())
}

/// A command running with stderr on a terminal and stdout redirected away from it: what stderr
/// writes there is read as it comes, so the test may act while it runs.
struct OnTerminal {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    chunks: std::sync::mpsc::Receiver<Vec<u8>>,
    /// What the terminal showed so far, read by [`OnTerminal::wait_for`].
    shown: Vec<u8>,
    what: String,
}

impl OnTerminal {
    /// Start `program` with `args` in the directory `cwd`, in the sandbox, with stdin and stderr
    /// on a terminal and stdout redirected to `stdout`: the terminal shows stderr alone.
    fn start<S: AsRef<OsStr>>(
        sandbox: &Sandbox,
        cwd: &Path,
        stdout: &Path,
        program: &Path,
        args: &[S],
    ) -> Self {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        use std::io::Read as _;

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 50,
                cols: 200,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        // The shell redirects stdout and execs `program` in its place, which keeps its process id
        // and leaves stdin and stderr on the terminal.
        let mut command = CommandBuilder::new("/bin/sh");
        command.arg("-c");
        command.arg("out=\"$1\"; shift; exec \"$@\" >\"$out\"");
        command.arg("sh");
        command.arg(stdout);
        command.arg(program);
        command.args(args);
        command.cwd(cwd);
        let template = sandbox.command(Path::new(AF));
        for (name, value) in template.get_envs() {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            }
        }
        command.env("AF_TEST_SELF_CHECK_LOG", check_log(sandbox));
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let master = pair.master;
        let mut reader = master.try_clone_reader().unwrap();
        let (sender, chunks) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(count) = reader.read(&mut chunk) {
                if count == 0 || sender.send(chunk[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        let words: Vec<&OsStr> = args.iter().map(|arg| arg.as_ref()).collect();
        let what = format!("{} {words:?}", program.display());
        Self {
            child,
            master,
            chunks,
            shown: Vec::new(),
            what,
        }
    }

    /// Read until the terminal shows `needle`, within a bound. Returns all it showed so far.
    fn wait_for(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let text = String::from_utf8_lossy(&self.shown).into_owned();
            if text.contains(needle) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "{} never showed {needle:?}: {text}",
                self.what
            );
            if let Ok(chunk) = self.chunks.recv_timeout(Duration::from_millis(50)) {
                self.shown.extend(chunk);
            }
        }
    }

    /// Type `line` and Return at the terminal, as a person answering a prompt.
    fn type_line(&mut self, line: &str) {
        use std::io::Write as _;
        let mut writer = self.master.take_writer().unwrap();
        writer.write_all(format!("{line}\r").as_bytes()).unwrap();
        writer.flush().unwrap();
    }

    /// Read until the command exits and closes the terminal, within a bound. Returns the exit
    /// code and what the terminal showed: stderr, never stdout.
    fn finish(mut self) -> (u32, String) {
        use std::sync::mpsc::RecvTimeoutError;

        let deadline = Instant::now() + Duration::from_secs(60);
        let mut shown = std::mem::take(&mut self.shown);
        loop {
            match self.chunks.recv_timeout(Duration::from_millis(50)) {
                Ok(chunk) => shown.extend(chunk),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        let _ = self.child.kill();
                        panic!(
                            "{} did not finish on a terminal: {}",
                            self.what,
                            String::from_utf8_lossy(&shown)
                        );
                    }
                }
            }
        }
        let status = self.child.wait().unwrap();
        drop(self.master);
        (
            status.exit_code(),
            String::from_utf8_lossy(&shown).into_owned(),
        )
    }
}

/// A FIFO made at `path` whose buffer is already full, held open here for reading and writing: a
/// command whose stdout is redirected into it opens it at once, and its first write waits until
/// the test reads. Returns the open FIFO, which reads without waiting, and how many filler bytes
/// come before what the command writes.
fn full_fifo(path: &Path) -> (std::fs::File, usize) {
    use std::io::Write as _;

    let made = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(made.success(), "mkfifo {path:?}");
    let mut fifo = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let flags = rustix::fs::fcntl_getfl(&fifo).unwrap();
    rustix::fs::fcntl_setfl(&fifo, flags | rustix::fs::OFlags::NONBLOCK).unwrap();
    let mut filled = 0;
    // Whole pages first, then single bytes until not one more fits.
    for size in [4096, 1] {
        let chunk = vec![b'.'; size];
        loop {
            match fifo.write(&chunk) {
                Ok(count) => filled += count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("filling {path:?}: {error}"),
            }
        }
    }
    (fifo, filled)
}

/// Append to `into` everything `fifo` holds now, without waiting.
fn read_available(fifo: &mut std::fs::File, into: &mut Vec<u8>) {
    use std::io::Read as _;

    let mut chunk = [0_u8; 4096];
    loop {
        match fifo.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => into.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("reading the FIFO: {error}"),
        }
    }
}

/// `command`'s output, headless, failing rather than waiting more than a minute for it.
fn within_a_minute(mut command: std::process::Command, what: &str) -> Output {
    use std::process::Stdio;

    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (sender, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    finished
        .recv_timeout(Duration::from_secs(60))
        .unwrap_or_else(|_| panic!("{what}"))
        .unwrap()
}

#[test]
fn a_headless_json_command_schedules_the_check_and_prints_only_its_document() {
    let newer = newer_release();
    let sandbox = Sandbox::new();
    sandbox.publish(&newer, false);
    // A receipted install, so `always` could activate here and `notify` is seen not to.
    let real = sandbox.adopt_real_binary();
    let home = sandbox.path("home");

    let first = headless(&sandbox, &real, &home, &["storage", "--json"]);
    storage_document(&first);
    assert!(err(&first).is_empty(), "{}", err(&first));
    assert_eq!(scheduled(&sandbox), 1, "a headless --json command checks");
    let cache = wait_for_latest(&sandbox, &newer);
    assert!(cache.contains("channel = \"stable\""), "{cache}");
    assert!(
        std::fs::read_to_string(sandbox.path("state/af/self.toml"))
            .unwrap()
            .contains("last_check"),
        "the check records when it ran"
    );
    assert!(sandbox.default_target().is_none(), "notify never activates");
    assert!(
        !sandbox.versions().join(&newer).exists(),
        "notify never installs"
    );

    // The cache now holds a newer release and is fresh: no second check, and neither a JSON
    // document nor a pipe carries the notice.
    let json = headless(&sandbox, &real, &home, &["storage", "--json"]);
    storage_document(&json);
    assert!(err(&json).is_empty(), "{}", err(&json));
    let piped = headless(&sandbox, &real, &home, &["storage"]);
    assert!(piped.status.success(), "{}", err(&piped));
    assert!(err(&piped).is_empty(), "{}", err(&piped));
    assert!(!out(&piped).contains("is available"), "{}", out(&piped));
    assert_eq!(scheduled(&sandbox), 1, "check_every throttles invocations");
}

#[test]
fn the_check_is_suppressed_offline_under_ci_disabled_never_and_for_exempt_commands() {
    let newer = newer_release();
    let sandbox = Sandbox::new();
    sandbox.publish(&newer, false);
    let home = sandbox.path("home");
    let storage = |env: &[(&str, &str)]| {
        let mut command = sandbox.command(Path::new(AF));
        command
            .args(["storage", "--json"])
            .current_dir(&home)
            .env("AF_TEST_SELF_CHECK_LOG", check_log(&sandbox));
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        storage_document(&output);
        assert!(err(&output).is_empty(), "{env:?}: {}", err(&output));
    };

    storage(&[("AF_SELF_OFFLINE", "1")]);
    storage(&[("CI", "true")]);
    write(&config_file(&sandbox), "[self]\nupdate_check = false\n");
    storage(&[]);
    write(&config_file(&sandbox), "[self]\nauto_update = \"never\"\n");
    storage(&[]);
    std::fs::remove_file(config_file(&sandbox)).unwrap();
    for args in [
        &["--version"][..],
        &["help", "self"],
        &["self", "status", "--json"],
        &["config", "paths"],
        &["completions", "bash"],
    ] {
        headless(&sandbox, Path::new(AF), &home, args);
    }
    assert_eq!(scheduled(&sandbox), 0, "nothing above may check");
    assert!(!cache_file(&sandbox).exists());

    // The same command with nothing suppressing it checks.
    storage(&[]);
    assert_eq!(scheduled(&sandbox), 1);
    wait_for_latest(&sandbox, &newer);
}

#[test]
fn a_cached_notice_reaches_a_terminal_but_never_a_json_run() {
    let newer = newer_release();
    let sandbox = Sandbox::new();
    fresh_cache(&sandbox, &newer);
    let notice = format!("af {newer} is available (running {VERSION})");

    // The notice goes to stderr on the terminal, exactly once, and never into stdout.
    let (code, shown, stdout) = on_terminal(&sandbox, &["storage"]);
    assert_eq!(code, 0, "{shown}");
    assert_eq!(shown.matches(notice.as_str()).count(), 1, "{shown}");
    assert!(stdout.contains("KIND"), "{stdout}");
    assert!(!stdout.contains("is available"), "{stdout}");

    let (code, shown, stdout) = on_terminal(&sandbox, &["storage", "--json"]);
    assert_eq!(code, 0, "{shown}");
    assert!(!shown.contains("is available"), "{shown}");
    assert!(!stdout.contains("is available"), "{stdout}");
    let document: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not JSON ({error}): {stdout}"));
    assert_eq!(document["schema"], "af/storage@3");

    assert_eq!(scheduled(&sandbox), 0, "a fresh cache is not refreshed");
}

#[test]
fn a_pinned_command_is_checked_once_by_the_af_that_dispatches_it() {
    let newer = newer_release();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let digest = sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    sandbox.install_pins("\"always\"");
    sandbox.publish(&newer, false);
    let lock = Sandbox::lock("0.8.1", Some(&digest));
    let repo = sandbox.pinned_repo("repo", &lock);

    let dispatched = headless(&sandbox, Path::new(AF), &repo, &["storage", "--json"]);
    assert!(dispatched.status.success(), "{}", err(&dispatched));
    let text = out(&dispatched);
    assert!(text.contains("fake af 0.8.1 storage --json"), "{text}");
    assert!(text.contains(&format!("from={VERSION}")), "{text}");
    assert_eq!(scheduled(&sandbox), 1, "the dispatching af checks");
    wait_for_latest(&sandbox, &newer);
    assert_eq!(
        std::fs::read_to_string(repo.join(".af/af.lock")).unwrap(),
        lock,
        "the check never touches the pin"
    );
    assert!(sandbox.default_target().is_none());

    // Fresh now: the next dispatch runs without a check.
    let again = headless(&sandbox, Path::new(AF), &repo, &["storage", "--json"]);
    assert!(again.status.success(), "{}", err(&again));
    assert_eq!(scheduled(&sandbox), 1);

    // A due check is not scheduled twice: an af that was dispatched to leaves it to the af that
    // dispatched it.
    std::fs::remove_file(cache_file(&sandbox)).unwrap();
    let mut command = sandbox.command(Path::new(AF));
    let output = command
        .args(["storage", "--json"])
        .current_dir(sandbox.path("home"))
        .env("AF_TEST_SELF_CHECK_LOG", check_log(&sandbox))
        .env("AF_DISPATCHED_FROM", &newer)
        .output()
        .unwrap();
    storage_document(&output);
    assert_eq!(scheduled(&sandbox), 1);
}

#[test]
fn a_pinned_release_never_runs_while_the_pin_that_keeps_it_cannot_be_recorded() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let digest = sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    sandbox.install_pins("\"always\"");
    let lock = Sandbox::lock("0.8.1", Some(&digest));
    let repo = sandbox.pinned_repo("repo", &lock);
    fresh_cache(&sandbox, VERSION);
    let state = sandbox.path("state/af/self.toml");
    let versions_lock = sandbox.versions().join(".lock");
    let dispatch = || headless(&sandbox, Path::new(AF), &repo, &["storage", "--json"]);

    // Installed on demand, pinned, and run.
    let first = dispatch();
    assert!(first.status.success(), "{}", err(&first));
    assert!(
        out(&first).contains("fake af 0.8.1 storage --json"),
        "{}",
        out(&first)
    );
    assert!(state.is_file());
    assert!(versions_lock.is_file());

    // With the versions writable but the state not (its path is a directory), and then with the
    // versions lock not to be taken (its path is a directory), the installed release is not run
    // unprotected: the command fails and says why.
    let refused = |what: &str| {
        let output = dispatch();
        assert_eq!(output.status.code(), Some(1), "{what}: {}", out(&output));
        assert!(
            err(&output).contains("the pin could not be recorded"),
            "{what}: {}",
            err(&output)
        );
        assert!(
            !out(&output).contains("fake af"),
            "{what}: {}",
            out(&output)
        );
        assert!(sandbox.versions().join("0.8.1/af").is_file(), "{what}");
    };
    std::fs::remove_file(&state).unwrap();
    std::fs::create_dir(&state).unwrap();
    refused("an unwritable state");
    std::fs::remove_dir(&state).unwrap();
    std::fs::remove_file(&versions_lock).unwrap();
    std::fs::create_dir(&versions_lock).unwrap();
    refused("a versions lock that cannot be taken");
    assert!(!state.exists(), "nothing was recorded without the lock");
    std::fs::remove_dir(&versions_lock).unwrap();

    // Both writable again: the pin is recorded, then the release runs. The lock is untouched.
    let again = dispatch();
    assert!(again.status.success(), "{}", err(&again));
    assert!(
        out(&again).contains("fake af 0.8.1 storage --json"),
        "{}",
        out(&again)
    );
    assert!(
        std::fs::read_to_string(&state)
            .unwrap()
            .contains("version = \"0.8.1\""),
        "the pin is recorded"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join(".af/af.lock")).unwrap(),
        lock
    );
    assert_eq!(scheduled(&sandbox), 0, "a fresh cache is not refreshed");
}

#[test]
fn an_unavailable_release_source_is_asked_once_per_check_every_and_keeps_the_last_result() {
    let newer = newer_release();
    let sandbox = Sandbox::new();
    // The release source is down, and the last good check, long ago, found `newer`.
    std::fs::remove_dir(sandbox.path("releases")).unwrap();
    write(
        &cache_file(&sandbox),
        &format!(
            "{CACHE_SCHEMA}checked_at_epoch = 1\nlatest = \"{newer}\"\nchannel = \"stable\"\n"
        ),
    );
    let home = sandbox.path("home");

    let first = headless(&sandbox, Path::new(AF), &home, &["storage", "--json"]);
    storage_document(&first);
    assert!(err(&first).is_empty(), "{}", err(&first));
    assert_eq!(scheduled(&sandbox), 1, "a stale cache is due a check");
    let cache = wait_for_cache(&sandbox, "the failed attempt", |text| checked_at(text) > 1);
    assert!(cache.contains(&format!("latest = \"{newer}\"")), "{cache}");
    assert!(cache.contains("channel = \"stable\""), "{cache}");
    assert!(
        !std::fs::read_to_string(sandbox.path("state/af/self.toml"))
            .unwrap_or_default()
            .contains("last_check"),
        "a failed query is not a completed check"
    );

    // Until `check_every` passes, the outage costs no further query.
    for _ in 0..2 {
        let again = headless(&sandbox, Path::new(AF), &home, &["storage", "--json"]);
        storage_document(&again);
        assert!(err(&again).is_empty(), "{}", err(&again));
    }
    assert_eq!(scheduled(&sandbox), 1, "a failed query is throttled too");
}

/// Make `version` the default as an activation leaves it: a receipted copy and the link into it.
fn default_to(sandbox: &Sandbox, version: &str) {
    let dir = sandbox.versions().join(version);
    write(&dir.join("af"), "#!/bin/sh\n");
    write(
        &dir.join("receipt.toml"),
        &format!(
            "version = \"{version}\"\ntarget = \"{TARGET}\"\nsource = \"test\"\nasset = \"{}\"\nsha256 = \"aa\"\nverified_by = \"test\"\ninstalled_at = \"2026-09-03T00:00:00Z\"\n",
            Sandbox::asset(version)
        ),
    );
    std::os::unix::fs::symlink(dir.join("af"), sandbox.path("bin/af")).unwrap();
}

/// The cache a background activation leaves once it made `to` the default over `from`: the
/// notice bound to the default as it is now, with no activation recorded in the state.
fn activated_cache(sandbox: &Sandbox, now: u64, from: &str, to: &str) -> String {
    use std::os::unix::fs::MetadataExt as _;

    let link = std::fs::symlink_metadata(sandbox.path("bin/af")).unwrap();
    format!(
        "{CACHE_SCHEMA}checked_at_epoch = {now}\nlatest = \"{to}\"\nchannel = \"stable\"\n\n\
         [auto_updated]\nfrom = \"{from}\"\nto = \"{to}\"\n\n\
         [auto_updated.default]\nversion = \"{to}\"\nactivations = 0\nlink = \"{}:{}:{}.{:09}\"\n",
        link.dev(),
        link.ino(),
        link.ctime(),
        link.ctime_nsec()
    )
}

#[test]
fn a_held_check_lock_leaves_the_update_notice_and_the_cache_for_a_later_command() {
    let newer = newer_release();
    let sandbox = Sandbox::new();
    let now = epoch_now();
    default_to(&sandbox, &newer);
    let cached = activated_cache(&sandbox, now, VERSION, &newer);
    write(&cache_file(&sandbox), &cached);
    let notice = format!("af self: updated the default from {VERSION} to {newer}");

    // A detached check holds the lock, as one about to write a newer cache would.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cache_file(&sandbox).with_extension("lock"))
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let (code, shown, stdout) = on_terminal(&sandbox, &["storage"]);
    assert_eq!(code, 0, "{shown}");
    assert!(!shown.contains("updated the default"), "{shown}");
    assert!(stdout.contains("KIND"), "{stdout}");
    assert!(!stdout.contains("updated the default"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(cache_file(&sandbox)).unwrap(),
        cached,
        "the command waits for no lock and leaves the cache to the check"
    );

    // With the lock free, the notice is shown once, on stderr alone, and cleared, and the rest of
    // the cache stays.
    drop(lock);
    let (code, shown, stdout) = on_terminal(&sandbox, &["storage"]);
    assert_eq!(code, 0, "{shown}");
    assert_eq!(shown.matches(notice.as_str()).count(), 1, "{shown}");
    assert!(stdout.contains("KIND"), "{stdout}");
    assert!(!stdout.contains("updated the default"), "{stdout}");
    let cache = std::fs::read_to_string(cache_file(&sandbox)).unwrap();
    assert!(!cache.contains("auto_updated"), "{cache}");
    assert!(cache.contains(&format!("latest = \"{newer}\"")), "{cache}");
    assert_eq!(checked_at(&cache), now, "{cache}");

    let (code, shown, stdout) = on_terminal(&sandbox, &["storage"]);
    assert_eq!(code, 0, "{shown}");
    assert!(!shown.contains("updated the default"), "{shown}");
    assert!(!stdout.contains("updated the default"), "{stdout}");

    // The availability notice waits for a held lock the same way: the check holding it may be
    // replacing the release the cache names.
    let available = format!("af {newer} is available (running {VERSION})");
    let latest_only = std::fs::read_to_string(cache_file(&sandbox)).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(cache_file(&sandbox).with_extension("lock"))
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let (code, shown, stdout) = on_terminal(&sandbox, &["storage"]);
    assert_eq!(code, 0, "{shown}");
    assert!(!shown.contains("is available"), "{shown}");
    assert!(!stdout.contains("is available"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(cache_file(&sandbox)).unwrap(),
        latest_only
    );
    drop(lock);
    let (code, shown, stdout) = on_terminal(&sandbox, &["storage"]);
    assert_eq!(code, 0, "{shown}");
    assert_eq!(shown.matches(available.as_str()).count(), 1, "{shown}");
    assert!(stdout.contains("KIND"), "{stdout}");
    assert!(!stdout.contains("is available"), "{stdout}");
    assert_eq!(scheduled(&sandbox), 0, "a fresh cache is not refreshed");
}

#[test]
fn a_delayed_check_leaves_when_an_earlier_one_has_answered() {
    let newer = newer_release();
    let current = semver::Version::parse(&newer).unwrap();
    let newest = semver::Version::new(current.major, current.minor + 1, 0).to_string();
    let sandbox = Sandbox::new();
    sandbox.publish(&newer, false);
    // The checks two commands spawned after reading the same stale cache, the second delayed
    // until the first has released the check lock.
    let refresh = || {
        sandbox
            .refresh_check(Path::new(AF))
            .current_dir(sandbox.path("home"))
            .output()
            .unwrap()
    };

    let first = refresh();
    assert!(first.status.success(), "{}", err(&first));
    let answered = std::fs::read_to_string(cache_file(&sandbox)).unwrap();
    assert!(
        answered.contains(&format!("latest = \"{newer}\"")),
        "{answered}"
    );
    // A release published before the second check runs would show in the cache had it asked.
    sandbox.publish(&newest, false);
    let second = refresh();
    assert!(second.status.success(), "{}", err(&second));
    assert_eq!(
        std::fs::read_to_string(cache_file(&sandbox)).unwrap(),
        answered,
        "the second check asked the release source again"
    );

    // A failed query answers for `check_every` as well.
    write(
        &cache_file(&sandbox),
        &format!(
            "{CACHE_SCHEMA}checked_at_epoch = 1\nlatest = \"{newer}\"\nchannel = \"stable\"\n"
        ),
    );
    let releases = sandbox.path("releases");
    let away = sandbox.path("releases-away");
    std::fs::rename(&releases, &away).unwrap();
    let _ = refresh();
    let attempted = std::fs::read_to_string(cache_file(&sandbox)).unwrap();
    assert!(checked_at(&attempted) > 1, "{attempted}");
    assert!(
        attempted.contains(&format!("latest = \"{newer}\"")),
        "{attempted}"
    );
    std::fs::rename(&away, &releases).unwrap();
    let delayed = refresh();
    assert!(delayed.status.success(), "{}", err(&delayed));
    assert_eq!(
        std::fs::read_to_string(cache_file(&sandbox)).unwrap(),
        attempted,
        "the delayed check asked the release source after the failed attempt"
    );
}

/// Wait, within a bound, for `path` to exist.
fn wait_for_path(path: &Path, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !path.exists() {
        assert!(Instant::now() < deadline, "{what} never reached {path:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_pinned_command_never_waits_for_a_background_install() {
    let newer = newer_release();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let stall = sandbox.path("stall");
    sandbox.publish_stalled(&newer, &stall);
    sandbox.sign(&newer, &signer, None);
    let real = sandbox.adopt_real_binary();
    write(&config_file(&sandbox), "[self]\nauto_update = \"always\"\n");
    let home = sandbox.path("home");

    // A headless command's check finds `newer` and installs it: fetched and verified, the
    // install stalls in the release's version proof, before it publishes.
    let first = headless(&sandbox, &real, &home, &["storage", "--json"]);
    storage_document(&first);
    assert!(err(&first).is_empty(), "{}", err(&first));
    assert_eq!(scheduled(&sandbox), 1);
    wait_for_path(&stall.join("reached"), "the background install");

    // Meanwhile a project pinned to the installed release, with its bytes, runs to the end: it
    // records its pin under the versions lock, which the stalled install does not hold.
    let digest = common::sha256_hex(&std::fs::read(AF).unwrap());
    let lock = Sandbox::lock(VERSION, Some(&digest));
    let repo = sandbox.pinned_repo("repo", &lock);
    let pinned = headless(&sandbox, &real, &repo, &["storage", "--json"]);
    storage_document(&pinned);
    assert!(err(&pinned).is_empty(), "{}", err(&pinned));
    // Only the test opens the stall, and it has not expired: the install is still stalled.
    assert!(!stall.join("open").exists());
    assert!(
        !stall.join("expired").exists(),
        "the pinned command waited for the stalled install"
    );
    assert!(
        sandbox.default_target().is_none(),
        "nothing is published yet"
    );
    assert!(
        std::fs::read_to_string(sandbox.path("state/af/self.toml"))
            .unwrap()
            .contains(".af/af.lock"),
        "the pin is recorded"
    );

    // Released, the install publishes the verified release and activates it.
    write(&stall.join("open"), "");
    wait_for_cache(&sandbox, "the activation", |text| {
        auto_updated_to(text).as_deref() == Some(newer.as_str())
    });
    assert!(!stall.join("expired").exists());
    assert_eq!(sandbox.default_target().as_deref(), Some(newer.as_str()));
    let receipt =
        std::fs::read_to_string(sandbox.versions().join(&newer).join("receipt.toml")).unwrap();
    assert!(receipt.contains("verified_by = \"minisign\""), "{receipt}");
    let leftovers: Vec<_> = std::fs::read_dir(sandbox.versions())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.') && name != ".lock")
        .collect();
    assert!(leftovers.is_empty(), "staging left behind: {leftovers:?}");
    assert_eq!(
        std::fs::read_to_string(repo.join(".af/af.lock")).unwrap(),
        lock,
        "the check never touches the pin"
    );
}

#[test]
fn a_background_activation_keeps_a_default_the_operator_chose_meanwhile() {
    let newer = newer_release();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let stall = sandbox.path("stall");
    sandbox.publish_stalled(&newer, &stall);
    sandbox.sign(&newer, &signer, None);
    sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    let real = sandbox.adopt_real_binary();
    write(&config_file(&sandbox), "[self]\nauto_update = \"always\"\n");
    let home = sandbox.path("home");

    // A headless command's check finds `newer`; its install stalls before it publishes, having
    // read the default it would replace: none.
    let first = headless(&sandbox, &real, &home, &["storage", "--json"]);
    storage_document(&first);
    assert_eq!(scheduled(&sandbox), 1);
    wait_for_path(&stall.join("reached"), "the background install");

    // Meanwhile the operator chooses a release explicitly.
    let update = sandbox
        .command(&real)
        .args(["self", "update", "--version", "0.8.1"])
        .current_dir(&home)
        .output()
        .unwrap();
    assert!(update.status.success(), "{}", err(&update));
    assert_eq!(sandbox.default_target().as_deref(), Some("0.8.1"));

    // Released, the check installs `newer` but leaves the operator's default, and announces
    // nothing it did not do.
    write(&stall.join("open"), "");
    let cache = wait_for_latest(&sandbox, &newer);
    assert!(!stall.join("expired").exists());
    assert!(!cache.contains("auto_updated"), "{cache}");
    assert_eq!(sandbox.default_target().as_deref(), Some("0.8.1"));
    assert!(sandbox.versions().join(&newer).join("af").is_file());
    let state = std::fs::read_to_string(sandbox.path("state/af/self.toml")).unwrap();
    assert!(
        !state.contains(&format!("version = \"{newer}\"")),
        "no activation of {newer} is recorded: {state}"
    );
}

#[test]
fn a_background_activation_keeps_a_default_the_operator_rolled_back_to_meanwhile() {
    let newer = newer_release();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let stall = sandbox.path("stall");
    sandbox.publish_stalled(&newer, &stall);
    sandbox.sign(&newer, &signer, None);
    sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    let real = sandbox.adopt_real_binary();
    let home = sandbox.path("home");
    std::fs::create_dir_all(&home).unwrap();
    let operator = |args: &[&str]| {
        let output = sandbox
            .command(&real)
            .args(args)
            .current_dir(&home)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {}", err(&output));
    };
    // The release under test is the default the check will read.
    operator(&["self", "install", VERSION]);
    assert_eq!(sandbox.default_target().as_deref(), Some(VERSION));
    write(&config_file(&sandbox), "[self]\nauto_update = \"always\"\n");

    // A headless command's check finds `newer`; its install stalls before it publishes, having
    // read the default it would replace.
    let first = headless(&sandbox, &real, &home, &["storage", "--json"]);
    storage_document(&first);
    assert_eq!(scheduled(&sandbox), 1);
    wait_for_path(&stall.join("reached"), "the background install");

    // Meanwhile the operator updates to 0.8.1, then rolls back: the default is the one the check
    // read again, by the operator's choice.
    operator(&["self", "update", "--version", "0.8.1"]);
    assert_eq!(sandbox.default_target().as_deref(), Some("0.8.1"));
    operator(&["self", "rollback"]);
    assert_eq!(sandbox.default_target().as_deref(), Some(VERSION));

    // Released, the check installs `newer` but keeps the rolled-back default, and announces
    // nothing it did not do.
    write(&stall.join("open"), "");
    let cache = wait_for_latest(&sandbox, &newer);
    assert!(!stall.join("expired").exists());
    assert!(!cache.contains("auto_updated"), "{cache}");
    assert_eq!(sandbox.default_target().as_deref(), Some(VERSION));
    assert!(sandbox.versions().join(&newer).join("af").is_file());
    let state = std::fs::read_to_string(sandbox.path("state/af/self.toml")).unwrap();
    assert!(
        !state.contains(&format!("version = \"{newer}\"")),
        "no activation of {newer} is recorded: {state}"
    );
}

#[test]
fn an_update_notice_the_operator_rolled_back_is_never_shown() {
    let newer = newer_release();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish(&newer, false);
    sandbox.sign(&newer, &signer, None);
    let real = sandbox.adopt_real_binary();
    let home = sandbox.path("home");
    std::fs::create_dir_all(&home).unwrap();
    let operator = |args: &[&str]| {
        let output = sandbox
            .command(&real)
            .args(args)
            .current_dir(&home)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {}", err(&output));
    };
    operator(&["self", "install", VERSION]);
    assert_eq!(sandbox.default_target().as_deref(), Some(VERSION));
    write(&config_file(&sandbox), "[self]\nauto_update = \"always\"\n");

    // A headless command's check installs `newer`, makes it the default and caches the notice.
    let first = headless(&sandbox, &real, &home, &["storage", "--json"]);
    storage_document(&first);
    assert!(err(&first).is_empty(), "{}", err(&first));
    assert_eq!(scheduled(&sandbox), 1);
    wait_for_cache(&sandbox, "the activation", |text| {
        auto_updated_to(text).as_deref() == Some(newer.as_str())
    });
    wait_for_check_to_exit(&sandbox);
    assert_eq!(sandbox.default_target().as_deref(), Some(newer.as_str()));

    // The operator rolls back before the next command on a terminal: the cached notice would
    // name a default that no longer is, so it is cleared unshown and the rest of the cache stays.
    operator(&["self", "rollback"]);
    assert_eq!(sandbox.default_target().as_deref(), Some(VERSION));
    let (code, shown, stdout) = on_terminal(&sandbox, &["storage"]);
    assert_eq!(code, 0, "{shown}");
    assert!(!shown.contains("updated the default"), "{shown}");
    assert!(!stdout.contains("updated the default"), "{stdout}");
    let cache = std::fs::read_to_string(cache_file(&sandbox)).unwrap();
    assert!(!cache.contains("auto_updated"), "{cache}");
    assert!(cache.contains(&format!("latest = \"{newer}\"")), "{cache}");
    assert_eq!(sandbox.default_target().as_deref(), Some(VERSION));
    assert_eq!(scheduled(&sandbox), 1, "a fresh cache is not refreshed");
}

#[test]
fn a_pinned_command_on_a_terminal_shows_the_cached_notice_before_it_dispatches() {
    let newer = newer_release();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let digest = sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    // On a terminal the default would ask first; this test is about the notice.
    sandbox.install_pins("\"always\"");
    let lock = Sandbox::lock("0.8.1", Some(&digest));
    let repo = sandbox.pinned_repo("repo", &lock);
    fresh_cache(&sandbox, &newer);
    let notice = format!("af {newer} is available (running {VERSION})");

    // The notice reaches stderr on the terminal once; the pinned release's output, stdout alone.
    let (code, shown, stdout) = on_terminal_in(&sandbox, &repo, &["storage"]);
    assert_eq!(code, 0, "{shown}");
    assert!(stdout.contains("fake af 0.8.1 storage"), "{stdout}");
    assert!(!stdout.contains("is available"), "{stdout}");
    assert_eq!(shown.matches(notice.as_str()).count(), 1, "{shown}");
    assert!(!shown.contains("fake af"), "{shown}");

    // Never into a JSON run or a pipe, dispatched or not.
    let (code, shown, stdout) = on_terminal_in(&sandbox, &repo, &["storage", "--json"]);
    assert_eq!(code, 0, "{shown}");
    assert!(stdout.contains("fake af 0.8.1 storage --json"), "{stdout}");
    assert!(!stdout.contains("is available"), "{stdout}");
    assert!(!shown.contains("is available"), "{shown}");
    let piped = headless(&sandbox, Path::new(AF), &repo, &["storage"]);
    assert!(piped.status.success(), "{}", err(&piped));
    assert!(!err(&piped).contains("is available"), "{}", err(&piped));
    assert!(!out(&piped).contains("is available"), "{}", out(&piped));
    assert_eq!(scheduled(&sandbox), 0, "a fresh cache is not refreshed");
    assert_eq!(
        std::fs::read_to_string(repo.join(".af/af.lock")).unwrap(),
        lock
    );
}

#[test]
fn an_interrupted_installs_staging_counts_until_the_next_install_recovers_it() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    // No check runs here: the cache is fresh.
    fresh_cache(&sandbox, VERSION);
    let home = sandbox.path("home");
    let versions = sandbox.versions();
    let staging = |pid: u32| {
        let dir = versions.join(format!(".0.8.1.{pid}"));
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir.join("af"), &"x".repeat(64 * 1024));
        dir
    };
    let sleeper = || {
        std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap()
    };
    // An installer interrupted after it began staging: killed, so its process is gone.
    let mut killed = sleeper();
    let abandoned = staging(killed.id());
    killed.kill().unwrap();
    killed.wait().unwrap();
    // `install.sh` staging now: it takes no lock, but its process runs.
    let mut running = sleeper();
    let live = staging(running.id());

    let storage = headless(&sandbox, Path::new(AF), &home, &["storage", "--json"]);
    let document = storage_document(&storage);
    let entry = |dir: &Path| {
        let name = dir.file_name().unwrap().to_str().unwrap().to_string();
        document["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|found| {
                found["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(&name))
            })
            .unwrap_or_else(|| panic!("{name} is not counted: {document}"))
            .clone()
    };
    let (abandoned_entry, live_entry) = (entry(&abandoned), entry(&live));
    assert_eq!(abandoned_entry["kind"], "version");
    assert!(abandoned_entry["bytes"].as_u64().unwrap() > 0);
    assert!(abandoned_entry.get("in_use").is_none(), "{abandoned_entry}");
    assert_eq!(live_entry["in_use"], "staged by a running installer");

    // The next install recovers what the killed one left, and only that.
    let install = headless(
        &sandbox,
        Path::new(AF),
        &home,
        &["self", "install", "0.8.1"],
    );
    assert!(install.status.success(), "{}", err(&install));
    assert!(sandbox.versions().join("0.8.1/af").is_file());
    assert!(!abandoned.exists(), "the abandoned staging is recovered");
    assert!(
        live.join("af").is_file(),
        "a live installer's staging is kept"
    );
    running.kill().unwrap();
    running.wait().unwrap();
}

#[test]
fn an_update_notice_an_update_back_to_its_version_superseded_is_never_shown() {
    let newer = newer_release();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish(&newer, false);
    sandbox.sign(&newer, &signer, None);
    let real = sandbox.adopt_real_binary();
    let home = sandbox.path("home");
    std::fs::create_dir_all(&home).unwrap();
    let operator = |args: &[&str]| {
        let output = sandbox
            .command(&real)
            .args(args)
            .current_dir(&home)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {}", err(&output));
    };
    operator(&["self", "install", VERSION]);
    assert_eq!(sandbox.default_target().as_deref(), Some(VERSION));
    write(&config_file(&sandbox), "[self]\nauto_update = \"always\"\n");

    // A headless command's check updates the default to `newer` and caches the notice.
    let first = headless(&sandbox, &real, &home, &["storage", "--json"]);
    storage_document(&first);
    assert!(err(&first).is_empty(), "{}", err(&first));
    assert_eq!(scheduled(&sandbox), 1);
    wait_for_cache(&sandbox, "the activation", |text| {
        auto_updated_to(text).as_deref() == Some(newer.as_str())
    });
    wait_for_check_to_exit(&sandbox);
    assert_eq!(sandbox.default_target().as_deref(), Some(newer.as_str()));

    // Before the next command on a terminal, the operator rolls back, then updates to `newer`
    // again: the default is the version the notice names, but by the operator's later choice,
    // not by the activation the notice announces. It is cleared unshown.
    operator(&["self", "rollback"]);
    assert_eq!(sandbox.default_target().as_deref(), Some(VERSION));
    operator(&["self", "update", "--version", newer.as_str()]);
    assert_eq!(sandbox.default_target().as_deref(), Some(newer.as_str()));
    let (code, shown, stdout) = on_terminal(&sandbox, &["storage"]);
    assert_eq!(code, 0, "{shown}");
    assert!(!shown.contains("updated the default"), "{shown}");
    assert!(!stdout.contains("updated the default"), "{stdout}");
    let cache = std::fs::read_to_string(cache_file(&sandbox)).unwrap();
    assert_eq!(auto_updated_to(&cache), None, "{cache}");
    assert!(cache.contains(&format!("latest = \"{newer}\"")), "{cache}");
    assert_eq!(scheduled(&sandbox), 1, "a fresh cache is not refreshed");
}

#[test]
fn an_uninstall_refuses_while_a_background_install_runs_and_a_later_check_recreates_nothing() {
    let newer = newer_release();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let stall = sandbox.path("stall");
    sandbox.publish_stalled(&newer, &stall);
    sandbox.sign(&newer, &signer, None);
    let real = sandbox.adopt_real_binary();
    write(&config_file(&sandbox), "[self]\nauto_update = \"always\"\n");
    let home = sandbox.path("home");
    let uninstall = |args: &[&str]| {
        sandbox
            .command(&real)
            .args(["self", "uninstall"])
            .args(args)
            .current_dir(&home)
            .output()
            .unwrap()
    };

    // A headless command's check finds `newer`, fetches and verifies it, and stalls before it
    // publishes.
    let first = headless(&sandbox, &real, &home, &["storage", "--json"]);
    storage_document(&first);
    assert_eq!(scheduled(&sandbox), 1);
    wait_for_path(&stall.join("reached"), "the background install");

    // Uninstall, with and without --purge, refuses at once rather than wait for the release
    // source, and removes nothing.
    for purge in [false, true] {
        let args: &[&str] = if purge { &["--purge"] } else { &[] };
        let refused = uninstall(args);
        assert!(!refused.status.success(), "{args:?}: {}", out(&refused));
        assert!(
            err(&refused).contains("a background update check is running now"),
            "{args:?}: {}",
            err(&refused)
        );
        assert!(real.is_file(), "{args:?}");
        assert!(config_file(&sandbox).is_file(), "{args:?}");
        assert!(sandbox.path("state/af/self.toml").is_file(), "{args:?}");
    }
    assert!(!stall.join("expired").exists());

    // Released, the check publishes and activates `newer`, and leaves.
    write(&stall.join("open"), "");
    wait_for_cache(&sandbox, "the activation", |text| {
        auto_updated_to(text).as_deref() == Some(newer.as_str())
    });
    wait_for_check_to_exit(&sandbox);
    assert_eq!(sandbox.default_target().as_deref(), Some(newer.as_str()));

    // Now the uninstall and its purge go through.
    let purged = uninstall(&["--purge"]);
    assert!(purged.status.success(), "{}", err(&purged));
    for gone in ["bin/af", "data/af", "state/af", "cache/af", "config/af"] {
        assert!(
            std::fs::symlink_metadata(sandbox.path(gone)).is_err(),
            "{gone} is left"
        );
    }

    // A check scheduled before the uninstall that starts only now finds no check lock to take,
    // and recreates nothing.
    let late = sandbox
        .command(Path::new(AF))
        .args(["self", "refresh-check"])
        .current_dir(&home)
        .output()
        .unwrap();
    assert!(late.status.success(), "{}", err(&late));
    for gone in ["data/af", "state/af", "cache/af"] {
        assert!(
            std::fs::symlink_metadata(sandbox.path(gone)).is_err(),
            "the late check recreated {gone}"
        );
    }
}

#[test]
fn a_check_whose_state_cannot_be_written_is_still_throttled_by_check_every() {
    let newer = newer_release();
    let sandbox = Sandbox::new();
    sandbox.publish(&newer, false);
    let home = sandbox.path("home");
    // The cache can be written, but the state cannot be replaced: its path is a directory.
    let state = sandbox.path("state/af/self.toml");
    std::fs::create_dir_all(&state).unwrap();

    let first = headless(&sandbox, Path::new(AF), &home, &["storage", "--json"]);
    storage_document(&first);
    assert!(err(&first).is_empty(), "{}", err(&first));
    assert_eq!(scheduled(&sandbox), 1, "a missing cache is due a check");
    // The query answered and the state's write failed: the attempt and its answer are cached.
    let cache = wait_for_latest(&sandbox, &newer);
    assert!(checked_at(&cache) > 0, "{cache}");
    assert!(!cache.contains("auto_updated"), "{cache}");
    wait_for_check_to_exit(&sandbox);
    assert!(state.is_dir(), "the state was replaced");

    // Until `check_every` passes, no further command asks the release source.
    for _ in 0..2 {
        let again = headless(&sandbox, Path::new(AF), &home, &["storage", "--json"]);
        storage_document(&again);
        assert!(err(&again).is_empty(), "{}", err(&again));
    }
    assert_eq!(
        scheduled(&sandbox),
        1,
        "a check whose state could not be written is throttled too"
    );
}

#[test]
fn a_command_never_waits_for_the_versions_lock_to_schedule_its_check() {
    let newer = newer_release();
    let sandbox = Sandbox::new();
    sandbox.publish(&newer, false);
    let real = sandbox.adopt_real_binary();
    let home = sandbox.path("home");
    // `af self prune` holds the versions lock for as long as its removals take.
    let prune = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(sandbox.versions().join(".lock"))
        .unwrap();
    rustix::fs::flock(&prune, rustix::fs::FlockOperation::LockExclusive).unwrap();

    // A due headless --json command prints its document and finishes without waiting: it leaves
    // the check to a later command.
    let mut command = sandbox.command(&real);
    command
        .args(["storage", "--json"])
        .current_dir(&home)
        .env("AF_TEST_SELF_CHECK_LOG", check_log(&sandbox));
    let held = within_a_minute(command, "the command waited for the versions lock");
    storage_document(&held);
    assert!(err(&held).is_empty(), "{}", err(&held));
    assert_eq!(scheduled(&sandbox), 0, "no check while the lock is held");
    assert!(!cache_file(&sandbox).with_extension("lock").exists());

    // Released, the next command schedules the check, which records what it found.
    drop(prune);
    let next = headless(&sandbox, &real, &home, &["storage", "--json"]);
    storage_document(&next);
    assert!(err(&next).is_empty(), "{}", err(&next));
    assert_eq!(scheduled(&sandbox), 1);
    wait_for_latest(&sandbox, &newer);
}

#[test]
fn a_terminal_command_that_finishes_after_a_purge_shows_and_recreates_nothing() {
    let newer = newer_release();
    let sandbox = Sandbox::new();
    sandbox.publish(&newer, false);
    let real = sandbox.adopt_real_binary();
    let home = sandbox.path("home");
    // A stale cache naming a newer release, beside its check lock's file: a command finishing
    // now would show the notice and schedule a check.
    write(
        &cache_file(&sandbox),
        &format!(
            "{CACHE_SCHEMA}checked_at_epoch = 1\nlatest = \"{newer}\"\nchannel = \"stable\"\n"
        ),
    );
    write(&cache_file(&sandbox).with_extension("lock"), "");
    let fifo_path = sandbox.path("stdout.fifo");
    let (mut fifo, filled) = full_fifo(&fifo_path);

    // An eligible command starts with stderr on a terminal and the full FIFO as its stdout: it
    // cannot write its result, nor reach the notice and the check after it, until the test reads.
    let mut terminal = OnTerminal::start(&sandbox, &home, &fifo_path, &real, &["storage"]);

    // Meanwhile the operator uninstalls with --purge, and it goes through.
    let purged = sandbox
        .command(&real)
        .args(["self", "uninstall", "--purge"])
        .current_dir(&home)
        .output()
        .unwrap();
    assert!(purged.status.success(), "{}", err(&purged));
    let managed = ["bin/af", "data/af", "state/af", "cache/af", "config/af"];
    for gone in managed {
        assert!(
            std::fs::symlink_metadata(sandbox.path(gone)).is_err(),
            "{gone} is left"
        );
    }

    // Read now, the command writes its result and finishes, after the purge.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut stdout = Vec::new();
    loop {
        let exited = terminal.child.try_wait().unwrap().is_some();
        read_available(&mut fifo, &mut stdout);
        if exited {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the command never finished writing its result"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let (code, shown) = terminal.finish();
    assert_eq!(code, 0, "{shown}");
    assert!(stdout.len() > filled, "the command wrote no result");
    let result = String::from_utf8_lossy(&stdout[filled..]);
    assert!(result.contains("KIND"), "{result}");
    assert!(!result.contains("is available"), "{result}");
    assert!(!shown.contains("is available"), "{shown}");
    assert_eq!(
        scheduled(&sandbox),
        0,
        "a purged installation is never checked"
    );
    for gone in managed {
        assert!(
            std::fs::symlink_metadata(sandbox.path(gone)).is_err(),
            "the command recreated {gone}"
        );
    }
}

#[test]
fn an_update_notice_whose_clearing_fails_stays_unshown_on_a_terminal_until_it_can_be_cleared() {
    let newer = newer_release();
    let sandbox = Sandbox::new();
    let now = epoch_now();
    default_to(&sandbox, &newer);
    let cached = activated_cache(&sandbox, now, VERSION, &newer);
    write(&cache_file(&sandbox), &cached);
    write(&cache_file(&sandbox).with_extension("lock"), "");
    let notice = format!("af self: updated the default from {VERSION} to {newer}");
    let home = sandbox.path("home");
    // The cache stays readable, but a command cannot replace it: the shell makes a directory of
    // the temporary that the af it execs, with its process id, writes the replacement to.
    let stem = cache_file(&sandbox).with_extension("");
    let unreplaceable = || {
        program_on_terminal(
            &sandbox,
            &home,
            Path::new("/bin/sh"),
            &[
                OsStr::new("-c"),
                OsStr::new("mkdir \"$1.tmp.$$\" && shift && exec \"$@\""),
                OsStr::new("sh"),
                stem.as_os_str(),
                OsStr::new(AF),
                OsStr::new("storage"),
            ],
        )
    };
    // Silent on both streams while the notice cannot be cleared.
    for _ in 0..2 {
        let (code, shown, stdout) = unreplaceable();
        assert_eq!(code, 0, "{shown}");
        assert!(!shown.contains("updated the default"), "{shown}");
        assert!(stdout.contains("KIND"), "{stdout}");
        assert!(!stdout.contains("updated the default"), "{stdout}");
        assert_eq!(
            std::fs::read_to_string(cache_file(&sandbox)).unwrap(),
            cached,
            "the notice is kept for a later command"
        );
    }

    // Replaceable again: the next command on a terminal shows the notice exactly once, on stderr
    // and never in stdout, and clears it, and the rest of the cache stays.
    for entry in std::fs::read_dir(cache_file(&sandbox).parent().unwrap()).unwrap() {
        let entry = entry.unwrap();
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("latest.tmp.")
        {
            std::fs::remove_dir(entry.path()).unwrap();
        }
    }
    let (code, shown, stdout) = on_terminal(&sandbox, &["storage"]);
    assert_eq!(code, 0, "{shown}");
    assert_eq!(shown.matches(notice.as_str()).count(), 1, "{shown}");
    assert!(stdout.contains("KIND"), "{stdout}");
    assert!(!stdout.contains("updated the default"), "{stdout}");
    let cache = std::fs::read_to_string(cache_file(&sandbox)).unwrap();
    assert_eq!(auto_updated_to(&cache), None, "{cache}");
    assert!(cache.contains(&format!("latest = \"{newer}\"")), "{cache}");
    assert_eq!(checked_at(&cache), now, "{cache}");
    let (code, shown, stdout) = on_terminal(&sandbox, &["storage"]);
    assert_eq!(code, 0, "{shown}");
    assert!(!shown.contains("updated the default"), "{shown}");
    assert!(!stdout.contains("updated the default"), "{stdout}");
    assert_eq!(scheduled(&sandbox), 0, "a fresh cache is not refreshed");
}

#[test]
fn under_ask_a_terminal_is_asked_before_a_pin_is_installed_and_declining_runs_this_af() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let digest = sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    let repo = sandbox.pinned_repo("repo", &Sandbox::lock("0.8.1", Some(&digest)));
    let lock = std::fs::canonicalize(&repo).unwrap().join(".af/af.lock");
    fresh_cache(&sandbox, VERSION);
    let streams = tempfile::tempdir().unwrap();
    let stdout = streams.path().join("stdout");
    let ask = |answer: &str| {
        let mut terminal = OnTerminal::start(&sandbox, &repo, &stdout, Path::new(AF), &["storage"]);
        let prompt = terminal.wait_for("[y/N]");
        terminal.type_line(answer);
        let (code, shown) = terminal.finish();
        let written = String::from_utf8_lossy(&std::fs::read(&stdout).unwrap()).into_owned();
        (prompt, code, shown, written)
    };

    // The prompt names the version, the lock, and that the release is older than this af.
    let (prompt, code, shown, written) = ask("n");
    assert!(
        prompt.contains(&format!(
            "{} pins af 0.8.1, which is not installed",
            lock.display()
        )),
        "{prompt}"
    );
    assert!(
        prompt.contains(&format!("OLDER than the running af {VERSION}")),
        "{prompt}"
    );
    // Declined: nothing installed, and this af runs the command after naming the step.
    assert_eq!(code, 0, "{shown}");
    assert!(
        shown.contains("not confirmed at the prompt") && shown.contains("af self install 0.8.1"),
        "{shown}"
    );
    assert!(written.contains("KIND"), "{written}");
    assert!(!written.contains("fake af"), "{written}");
    assert!(!sandbox.versions().join("0.8.1").exists());

    // Confirmed: installed against the lock and the signature, then run.
    let (_, code, shown, written) = ask("y");
    assert_eq!(code, 0, "{shown}");
    assert!(
        written.contains("fake af 0.8.1 storage"),
        "{written}\n{shown}"
    );
    let receipt = std::fs::read_to_string(sandbox.versions().join("0.8.1/receipt.toml")).unwrap();
    assert!(
        receipt.contains("verified_by = \"lock+signature\""),
        "{receipt}"
    );
}
