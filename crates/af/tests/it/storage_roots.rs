//! The roots af keeps state under are resolved once, so a home reached through a symbolic link
//! (macOS `/tmp`, a home on another volume) is measured, swept and purged like any other: no
//! `skipped … is a symlink` line in a listing, no warning on a Task command, and `af self
//! uninstall --purge` removes within the resolved root. A registered Store a test left half
//! made is a note of `af storage --verbose`, never a warning after every run.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A home whose every directory is reached through `link`, a symbolic link to `real`.
struct LinkedHome {
    _root: tempfile::TempDir,
    real: PathBuf,
    link: PathBuf,
}

impl LinkedHome {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().canonicalize().unwrap().join("real");
        std::fs::create_dir_all(real.join("home")).unwrap();
        let link = root.path().canonicalize().unwrap().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        Self {
            _root: root,
            real,
            link,
        }
    }

    fn env(&self, command: &mut Command) {
        let home = self.link.join("home");
        command
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_BIN_HOME", home.join(".local/bin"))
            .env("AF_SELF_OFFLINE", "1")
            .env("NO_COLOR", "1")
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("AF_PROVIDERS_FILE");
    }

    /// `af` with this home, the private one every test `af` gets replaced by the linked one.
    fn af(&self, dir: &Path, args: &[&str]) -> Output {
        let mut command = crate::common::af();
        self.env(&mut command);
        command.args(args).current_dir(dir).output().unwrap()
    }
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn git(repo: &Path, args: &[&str]) {
    let done = Command::new("git")
        .current_dir(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(done.status.success(), "git {args:?}: {done:?}");
}

/// Nothing about a link, a skipped path or a measurement on either stream.
fn quiet(what: &str, output: &Output) {
    let both = format!("{}{}", out(output), err(output));
    for word in ["symlink", "skipped", "warning", "could not be measured"] {
        assert!(!both.contains(word), "{what} says {word:?}:\n{both}");
    }
}

#[test]
fn a_home_through_a_symlink_is_measured_without_a_warning() {
    let home = LinkedHome::new();
    let repo = home.real.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# Repo\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    // The Project Store lands under the linked state root.
    let shown = home.af(&repo, &["project", "show"]);
    assert!(shown.status.success(), "{}", err(&shown));
    let listed = home.af(&repo, &["storage"]);
    assert!(listed.status.success(), "{}", err(&listed));
    quiet("af storage", &listed);
    assert!(
        out(&listed)
            .lines()
            .any(|line| line.starts_with("Project Stores") && !line.contains("  0  ")),
        "{}",
        out(&listed)
    );
    for args in [
        &["task", "list"][..],
        &["storage", "prune", "--apply"],
        &["project", "show"],
    ] {
        let ran = home.af(&repo, args);
        assert!(ran.status.success(), "{args:?}: {}", err(&ran));
        quiet(&format!("af {}", args.join(" ")), &ran);
    }

    // A registered Store a test left half made: counted once in the listing, named only with
    // --verbose, and no warning from the sweep.
    let half = home.real.join("half/state");
    std::fs::create_dir_all(&half).unwrap();
    let conn = rusqlite::Connection::open(half.join("events.sqlite")).unwrap();
    conn.execute_batch("CREATE TABLE unrelated (x);").unwrap();
    drop(conn);
    let registry = home.real.join("home/.local/state/af/stores.toml");
    let existing = std::fs::read_to_string(&registry).unwrap_or_else(|_| "version = 2\n".into());
    std::fs::write(
        &registry,
        format!(
            "{existing}\n[[store]]\npath = \"{}\"\nkind = \"task\"\nfirst_use_unix_ms = 1\n\
             last_use_unix_ms = 1\n",
            half.display()
        ),
    )
    .unwrap();
    let listed = out(&home.af(&repo, &["storage"]));
    assert!(
        listed.contains(
            "1 path could not be measured and is left alone; af storage --verbose names it"
        ),
        "{listed}"
    );
    assert!(!listed.contains(half.to_str().unwrap()), "{listed}");
    let verbose = out(&home.af(&repo, &["storage", "--verbose"]));
    assert!(
        verbose.contains(&format!("skipped {}: ", half.display())),
        "{verbose}"
    );
    let swept = home.af(&repo, &["storage", "prune", "--apply"]);
    assert!(swept.status.success(), "{}", err(&swept));
    assert!(!err(&swept).contains("warning"), "{}", err(&swept));
}

#[test]
fn uninstall_purge_removes_within_the_resolved_root() {
    let sandbox = crate::common::Sandbox::new();
    let real = sandbox.adopt_real_binary();
    let root = sandbox.path("").canonicalize().unwrap();
    let link = root.parent().unwrap().join(format!(
        "{}-link",
        root.file_name().unwrap().to_string_lossy()
    ));
    std::os::unix::fs::symlink(&root, &link).unwrap();
    let state = sandbox.path("state/af");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("marker"), "kept until purged").unwrap();
    let purged = sandbox
        .command(&real)
        .args(["self", "uninstall", "--purge"])
        .env("HOME", link.join("home"))
        .env("XDG_CONFIG_HOME", link.join("config"))
        .env("XDG_DATA_HOME", link.join("data"))
        .env("XDG_STATE_HOME", link.join("state"))
        .env("XDG_CACHE_HOME", link.join("cache"))
        .env("XDG_BIN_HOME", link.join("bin"))
        .env("AF_SELF_OFFLINE", "1")
        .current_dir(sandbox.path("home"))
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&link);
    assert!(purged.status.success(), "{}", err(&purged));
    assert!(!err(&purged).contains("symlink"), "{}", err(&purged));
    for gone in ["data/af", "state/af", "cache/af", "config/af"] {
        assert!(
            std::fs::symlink_metadata(sandbox.path(gone)).is_err(),
            "{gone} is left: {}",
            err(&purged)
        );
    }
}
