//! Provider registry bootstrap is owned by the CLI: a fresh installation should not require
//! hand-authoring the machine-local TOML before an explicit Worker binding can be admitted.

use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};

use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;

/// A directory whose name is not UTF-8, or `None` where the filesystem refuses to hold one.
/// The constraint is the filesystem's own encoding rule, not the platform: APFS rejects with
/// `EILSEQ` the byte that ext4 stores without complaint. Skipping there keeps a Mac from failing
/// this against a limitation of its disk; Linux CI, which the release also runs, still exercises
/// every assertion.
fn non_utf8_dir(root: &Path, name: &[u8]) -> Option<std::path::PathBuf> {
    let path = root.join(std::ffi::OsString::from_vec(name.to_vec()));
    std::fs::create_dir(&path).ok().map(|()| path)
}

fn af(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn fake_provider(root: &Path, name: &str, script: &str) -> std::path::PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join(name);
    std::fs::write(&path, script).unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    bin
}

/// Run a command with all three standard streams on a real pseudo-terminal.
///
/// `af provider setup --login` refuses to start an official Provider login unless it owns an
/// interactive terminal, so the interactive path can only be exercised by giving it one; an
/// environment flag that bypassed the check would weaken exactly the boundary under test. A PTY
/// merges stdout and stderr by construction, which is why stream placement is asserted on the
/// piped, non-interactive paths in `provider_onboarding.rs` instead.
fn in_terminal(
    program: &std::ffi::OsStr,
    args: &[&std::ffi::OsStr],
    env: &[(&str, &std::ffi::OsStr)],
) -> (u32, String) {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Read as _;

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 140,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(program);
    for argument in args {
        command.arg(argument);
    }
    command.env_clear();
    command.cwd("/");
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let drained = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        bytes
    });
    let status = child.wait().unwrap();
    drop(pair.master);
    let text = String::from_utf8_lossy(&drained.join().unwrap()).into_owned();
    (status.exit_code(), text)
}

/// `af provider setup … --login` at an interactive terminal.
fn setup_in_terminal(
    home: &Path,
    bin: &Path,
    args: &[&str],
    extra_env: &[(&str, &std::ffi::OsStr)],
) -> (u32, String) {
    use std::ffi::OsStr;

    let config = home.join("config");
    let mut arguments: Vec<&OsStr> = vec![OsStr::new("provider"), OsStr::new("setup")];
    arguments.extend(args.iter().map(|argument| OsStr::new(*argument)));
    arguments.push(OsStr::new("--login"));
    let mut env: Vec<(&str, &OsStr)> = vec![
        ("HOME", home.as_os_str()),
        ("XDG_CONFIG_HOME", config.as_os_str()),
        ("PATH", bin.as_os_str()),
        ("AF_SELF_OFFLINE", OsStr::new("1")),
    ];
    env.extend_from_slice(extra_env);
    in_terminal(OsStr::new(env!("CARGO_BIN_EXE_af")), &arguments, &env)
}

#[test]
fn status_leaves_out_a_logged_out_default_context_once_its_kind_is_registered() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    let bin = fake_provider(
        home,
        "claude",
        r#"#!/bin/sh
# Like the real CLI: CLAUDE_CONFIG_DIR, or ~/.claude without it.
if [ "$1" = auth ] && [ "$2" = status ] && [ "$3" = --json ]; then
  if [ -f "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/logged-in" ]; then
    printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}'
    exit 0
  fi
  printf '%s\n' '{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}'
  exit 1
fi
exit 64
"#,
    );
    fake_provider(
        home,
        "codex",
        r#"#!/bin/sh
# Like codex-cli: CODEX_HOME, or ~/.codex without it; login status goes to stderr.
if [ "$1" = login ] && [ "$2" = status ]; then
  printf '%s\n' 'Not logged in' >&2
  exit 1
fi
exit 64
"#,
    );
    // The Claude CLI's default context, `~/.claude`, and a registered one elsewhere; the Codex
    // CLI's default context, `~/.codex`, with no login and no Codex Provider registered.
    let default = home.join(".claude");
    let auth = home.join("claude-main");
    for dir in [&default, &auth, &home.join(".codex")] {
        std::fs::create_dir(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(auth.join("logged-in"), "").unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_af"))
            .args(args)
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("AF_SELF_OFFLINE", "1")
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .output()
            .unwrap()
    };
    let status = || {
        let output = run(&["provider", "status"]);
        assert!(output.status.success(), "{}", stderr(&output));
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    // Before a Claude Provider is registered, the logged-out default context is how setup starts.
    let before = status();
    assert!(before.contains("claude-ambient"), "{before}");
    let added = run(&[
        "provider",
        "add",
        "claude-main",
        "--kind",
        "claude",
        "--auth-dir",
        auth.to_str().unwrap(),
    ]);
    assert!(added.status.success(), "{}", stderr(&added));
    // Once one is, the default directory with no login is one the operator does not use.
    let after = status();
    assert!(after.contains("claude-main"), "{after}");
    assert!(!after.contains("claude-ambient"), "{after}");
    // A Provider of the other kind hides nothing: Codex's logged-out default context stays.
    let codex = after
        .lines()
        .find(|line| line.starts_with("codex-ambient "))
        .unwrap_or_else(|| panic!("codex-ambient is listed: {after}"));
    assert!(codex.contains("not authenticated"), "{after}");
    // A login there brings it back: it is a login the operator may register.
    std::fs::write(default.join("logged-in"), "").unwrap();
    let again = status();
    assert!(again.contains("claude-ambient"), "{again}");
}

#[test]
fn setup_owns_claude_login_registration_and_idempotent_recheck() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("claude auth");
    let bin = fake_provider(
        root.path(),
        "claude",
        r#"#!/bin/sh
if [ "$1" = auth ] && [ "$2" = status ] && [ "$3" = --json ]; then
  if [ -f "$CLAUDE_CONFIG_DIR/logged-in" ]; then
    printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}'
    exit 0
  fi
  printf '%s\n' '{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}'
  exit 1
fi
if [ "$1" = auth ] && [ "$2" = login ] && [ "$3" = --claudeai ]; then
  printf '%s\n' "$*" >> "$CLAUDE_CONFIG_DIR/login-log"
  pwd > "$CLAUDE_CONFIG_DIR/login-cwd"
  printf '%s\n' "${LEAK_ME-unset}" > "$CLAUDE_CONFIG_DIR/login-leak"
  : > "$CLAUDE_CONFIG_DIR/logged-in"
  exit 0
fi
exit 64
"#,
    );
    let invoke = || {
        setup_in_terminal(
            root.path(),
            &bin,
            &[
                "claude-main",
                "--kind",
                "claude",
                "--auth-dir",
                auth.to_str().unwrap(),
            ],
            &[("LEAK_ME", std::ffi::OsStr::new("must-not-reach-login"))],
        )
    };

    let (code, output) = invoke();
    assert_eq!(code, 0, "{output}");
    // The warning is the operator's only chance to hear it before the Provider CLI owns the
    // screen, so it is part of the contract, not incidental prose.
    assert!(
        output.contains("OAuth URL") && output.contains("authorization code"),
        "{output}"
    );
    assert!(
        output.contains("never relay an OAuth URL or code through chat"),
        "{output}"
    );
    assert!(auth.join("logged-in").is_file());
    assert_eq!(
        std::fs::read_to_string(auth.join("login-log")).unwrap(),
        "auth login --claudeai\n"
    );
    assert_eq!(
        std::fs::read_to_string(auth.join("login-cwd")).unwrap(),
        "/\n"
    );
    assert_eq!(
        std::fs::read_to_string(auth.join("login-leak")).unwrap(),
        "unset\n"
    );
    let registry = std::fs::read_to_string(root.path().join("config/af/providers.toml")).unwrap();
    assert!(registry.contains("claude-main"), "{registry}");
    assert!(registry.contains(auth.canonicalize().unwrap().to_str().unwrap()));

    let (code, output) = invoke();
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("is authenticated and registered"),
        "{output}"
    );
    // An already-authenticated context is registered, and re-checked, without a second login.
    assert!(!output.contains("OAuth URL"), "{output}");
    assert_eq!(
        std::fs::read_to_string(auth.join("login-log")).unwrap(),
        "auth login --claudeai\n"
    );
}

/// The login child must land in the auth directory `af` was told to use, not an ambient one.
#[test]
fn interactive_login_selects_exactly_the_named_auth_directory() {
    let root = tempfile::tempdir().unwrap();
    let ambient = root.path().join("ambient-claude");
    let chosen = root.path().join("chosen-claude");
    std::fs::create_dir(&ambient).unwrap();
    let bin = fake_provider(
        root.path(),
        "claude",
        r#"#!/bin/sh
if [ "$1" = auth ] && [ "$2" = status ] && [ "$3" = --json ]; then
  if [ -f "$CLAUDE_CONFIG_DIR/logged-in" ]; then
    printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}'
    exit 0
  fi
  printf '%s\n' '{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}'
  exit 1
fi
if [ "$1" = auth ] && [ "$2" = login ]; then
  printf '%s\n' "$CLAUDE_CONFIG_DIR" > "$CLAUDE_CONFIG_DIR/login-dir"
  : > "$CLAUDE_CONFIG_DIR/logged-in"
  exit 0
fi
exit 64
"#,
    );
    let (code, output) = setup_in_terminal(
        root.path(),
        &bin,
        &[
            "claude-main",
            "--kind",
            "claude",
            "--auth-dir",
            chosen.to_str().unwrap(),
        ],
        &[("CLAUDE_CONFIG_DIR", ambient.as_os_str())],
    );
    assert_eq!(code, 0, "{output}");
    assert_eq!(
        std::fs::read_to_string(chosen.join("login-dir")).unwrap(),
        format!("{}\n", chosen.canonicalize().unwrap().display())
    );
    assert!(
        !ambient.join("logged-in").exists(),
        "login reached the ambient directory instead of the named one"
    );
    let registry = std::fs::read_to_string(root.path().join("config/af/providers.toml")).unwrap();
    assert!(
        registry.contains(chosen.canonicalize().unwrap().to_str().unwrap()),
        "{registry}"
    );
}

#[test]
fn setup_owns_codex_login_and_reads_its_stderr_status() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("codex-home");
    let bin = fake_provider(
        root.path(),
        "codex",
        r#"#!/bin/sh
if [ "$1" = login ] && [ "$2" = status ]; then
  if [ -f "$CODEX_HOME/logged-in" ]; then
    printf '%s\n' 'Logged in using ChatGPT' >&2
    exit 0
  fi
  printf '%s\n' 'Not logged in' >&2
  exit 1
fi
if [ "$1" = login ] && [ -z "$2" ]; then
  : > "$CODEX_HOME/logged-in"
  exit 0
fi
exit 64
"#,
    );
    let (code, output) = setup_in_terminal(
        root.path(),
        &bin,
        &[
            "codex-main",
            "--kind",
            "codex",
            "--auth-dir",
            auth.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(code, 0, "{output}");
    assert!(auth.join("logged-in").is_file());
    let registry = std::fs::read_to_string(root.path().join("config/af/providers.toml")).unwrap();
    assert!(registry.contains("codex-main"), "{registry}");
}

#[test]
fn setup_repairs_auth_directory_and_lock_modes_under_a_restrictive_umask() {
    use std::ffi::OsStr;

    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("nested/codex-home");
    let bin = fake_provider(
        root.path(),
        "codex",
        r#"#!/bin/sh
if [ "$1" = login ] && [ "$2" = status ]; then
  if [ -f "$CODEX_HOME/logged-in" ]; then
    printf '%s\n' 'Logged in using ChatGPT' >&2
    exit 0
  fi
  printf '%s\n' 'Not logged in' >&2
  exit 1
fi
if [ "$1" = login ]; then
  : > "$CODEX_HOME/logged-in"
  exit 0
fi
exit 64
"#,
    );
    let config = root.path().join("config");
    let (code, output) = in_terminal(
        OsStr::new("/bin/sh"),
        &[
            OsStr::new("-c"),
            OsStr::new(
                "umask 0777; PATH=\"$3\" exec \"$1\" provider setup codex-main --kind codex --auth-dir \"$2\" --login",
            ),
            OsStr::new("sh"),
            OsStr::new(env!("CARGO_BIN_EXE_af")),
            auth.as_os_str(),
            bin.as_os_str(),
        ],
        &[
            ("HOME", root.path().as_os_str()),
            ("XDG_CONFIG_HOME", config.as_os_str()),
            ("AF_SELF_OFFLINE", OsStr::new("1")),
        ],
    );
    assert_eq!(code, 0, "{output}");
    assert_eq!(
        std::fs::metadata(&auth).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(auth.join(".af-codex-setup.lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn setup_never_registers_a_failed_login() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("claude-auth");
    let bin = fake_provider(
        root.path(),
        "claude",
        r#"#!/bin/sh
if [ "$1" = auth ] && [ "$2" = status ]; then
  printf '%s\n' '{"loggedIn":false}'
  exit 1
fi
if [ "$1" = auth ] && [ "$2" = login ]; then
  exit 1
fi
exit 64
"#,
    );
    let (code, output) = setup_in_terminal(
        root.path(),
        &bin,
        &[
            "claude-main",
            "--kind",
            "claude",
            "--auth-dir",
            auth.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(code, 6, "{output}");
    assert!(output.contains("no provider was registered"), "{output}");
    assert!(!root.path().join("config/af/providers.toml").exists());
}

#[test]
fn explicit_registration_rejects_an_auth_directory_writable_by_other_users() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("shared-auth");
    std::fs::create_dir(&auth).unwrap();
    let mut permissions = std::fs::metadata(&auth).unwrap().permissions();
    permissions.set_mode(0o777);
    std::fs::set_permissions(&auth, permissions).unwrap();

    let output = af(
        root.path(),
        &[
            "provider",
            "setup",
            "codex-main",
            "--kind",
            "codex",
            "--auth-dir",
            auth.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("writable by another user"),
        "{}",
        stderr(&output)
    );
    assert!(!root.path().join("config/af/providers.toml").exists());

    let added = af(
        root.path(),
        &[
            "provider",
            "add",
            "codex-main",
            "--kind",
            "codex",
            "--auth-dir",
            auth.to_str().unwrap(),
        ],
    );
    assert!(!added.status.success());
    assert!(
        stderr(&added).contains("writable by another user"),
        "{}",
        stderr(&added)
    );
    assert!(!root.path().join("config/af/providers.toml").exists());
}

#[test]
fn status_revalidates_auth_directory_safety_after_registration() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("codex-auth");
    std::fs::create_dir(&auth).unwrap();
    let mut permissions = std::fs::metadata(&auth).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&auth, permissions).unwrap();
    let added = af(
        root.path(),
        &[
            "provider",
            "add",
            "codex-main",
            "--kind",
            "codex",
            "--auth-dir",
            auth.to_str().unwrap(),
        ],
    );
    assert!(added.status.success(), "{}", stderr(&added));

    let mut permissions = std::fs::metadata(&auth).unwrap().permissions();
    permissions.set_mode(0o777);
    std::fs::set_permissions(&auth, permissions).unwrap();
    let status = af(root.path(), &["provider", "status"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(stdout.contains("codex-main"), "{stdout}");
    assert!(stdout.contains("unavailable"), "{stdout}");
    assert!(stdout.contains("writable by another user"), "{stdout}");
}

#[test]
fn setup_rejects_a_symlinked_auth_directory() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("real-auth");
    let auth = root.path().join("linked-auth");
    std::fs::create_dir(&target).unwrap();
    symlink(&target, &auth).unwrap();

    let output = af(
        root.path(),
        &[
            "provider",
            "setup",
            "codex-main",
            "--kind",
            "codex",
            "--auth-dir",
            auth.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("must not be a symlink"),
        "{}",
        stderr(&output)
    );
    assert!(!root.path().join("config/af/providers.toml").exists());
}

#[test]
fn setup_rejects_a_non_utf8_auth_directory_before_creating_it() {
    let root = tempfile::tempdir().unwrap();
    let auth = root
        .path()
        .join(std::ffi::OsString::from_vec(b"codex-auth-\xff".to_vec()));
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .args(["provider", "setup", "codex-main", "--kind", "codex"])
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("CODEX_HOME", &auth)
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("not valid UTF-8 and cannot be stored in TOML"),
        "{}",
        stderr(&output)
    );
    assert!(
        !auth.exists(),
        "setup created an unpublishable auth directory"
    );
    assert!(!root.path().join("config/af/providers.toml").exists());
}

#[test]
fn setup_rejects_a_utf8_alias_to_a_non_utf8_parent_before_creating_the_leaf() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let Some(target) = non_utf8_dir(root.path(), b"auth-parent-\xff") else {
        return;
    };
    let alias = root.path().join("printable-auth-parent");
    let auth = alias.join("codex-auth");
    symlink(&target, &alias).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .args(["provider", "setup", "codex-main", "--kind", "codex"])
        .arg("--auth-dir")
        .arg(&auth)
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("resolves through a non-UTF-8 path"),
        "{}",
        stderr(&output)
    );
    assert!(!target.join("codex-auth").exists());
}

#[test]
fn setup_rejects_authenticated_output_from_a_failed_status_command() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("codex-auth");
    std::fs::create_dir(&auth).unwrap();
    let bin = fake_provider(
        root.path(),
        "codex",
        r#"#!/bin/sh
if [ "$1" = login ] && [ "$2" = status ]; then
  printf '%s\n' 'Logged in using ChatGPT' >&2
  exit 1
fi
exit 64
"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .args([
            "provider",
            "setup",
            "codex-main",
            "--kind",
            "codex",
            "--auth-dir",
            auth.to_str().unwrap(),
        ])
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("PATH", bin)
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("exited unsuccessfully"),
        "{}",
        stderr(&output)
    );
    assert!(!root.path().join("config/af/providers.toml").exists());
}

#[test]
fn concurrent_identical_setup_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("codex-auth");
    std::fs::create_dir(&auth).unwrap();
    let bin = fake_provider(
        root.path(),
        "codex",
        r#"#!/bin/sh
if [ "$1" = login ] && [ "$2" = status ]; then
  if [ -f "$CODEX_HOME/logged-in" ]; then
    printf '%s\n' 'Logged in using ChatGPT' >&2
    exit 0
  fi
  printf '%s\n' 'Not logged in' >&2
  exit 1
fi
if [ "$1" = login ]; then
  printf '%s\n' login >> "$CODEX_HOME/login-log"
  sleep 1
  : > "$CODEX_HOME/logged-in"
  exit 0
fi
exit 64
"#,
    );
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for index in 0..2 {
        let barrier = Arc::clone(&barrier);
        let home = root.path().to_path_buf();
        let auth = auth.clone();
        let bin = bin.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            let registry = home.join(format!("registry-{index}.toml"));
            setup_in_terminal(
                &home,
                &bin,
                &[
                    "codex-main",
                    "--kind",
                    "codex",
                    "--auth-dir",
                    auth.to_str().unwrap(),
                ],
                &[("AF_PROVIDERS_FILE", registry.as_os_str())],
            )
        }));
    }
    barrier.wait();
    for thread in threads {
        let (code, output) = thread.join().unwrap();
        assert_eq!(code, 0, "{output}");
    }
    for index in 0..2 {
        let registry =
            std::fs::read_to_string(root.path().join(format!("registry-{index}.toml"))).unwrap();
        assert_eq!(
            registry.matches("id = \"codex-main\"").count(),
            1,
            "{registry}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(auth.join("login-log")).unwrap(),
        "login\n",
        "concurrent first-time setup must run one interactive login"
    );
}

#[test]
fn add_creates_a_machine_local_registry_and_refuses_ambiguous_duplicates() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("codex-auth");
    std::fs::create_dir(&auth).unwrap();

    let output = af(
        root.path(),
        &[
            "provider",
            "add",
            "codex-main",
            "--kind",
            "codex",
            "--auth-dir",
            auth.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("codex-main registered"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let path = root.path().join("config/af/providers.toml");
    let registry = std::fs::read_to_string(&path).unwrap();
    let value: toml::Value = toml::from_str(&registry).unwrap();
    assert_eq!(value["version"].as_integer(), Some(1));
    assert_eq!(value["providers"][0]["id"].as_str(), Some("codex-main"));
    assert_eq!(value["providers"][0]["kind"].as_str(), Some("codex"));
    assert_eq!(
        value["providers"][0]["auth_dir"].as_str(),
        auth.canonicalize().unwrap().to_str()
    );

    let duplicate = af(
        root.path(),
        &[
            "provider",
            "add",
            "another-codex",
            "--kind",
            "codex",
            "--auth-dir",
            auth.to_str().unwrap(),
        ],
    );
    assert!(!duplicate.status.success());
    assert!(
        stderr(&duplicate).contains("duplicates provider `codex-main`"),
        "{}",
        stderr(&duplicate)
    );
}

#[test]
fn add_creates_private_registry_state_even_with_a_wide_umask() {
    for umask in ["0002", "0177", "0777"] {
        let root = tempfile::tempdir().unwrap();
        let auth = root.path().join("codex-auth");
        let second_auth = root.path().join("second-codex-auth");
        std::fs::create_dir(&auth).unwrap();
        std::fs::create_dir(&second_auth).unwrap();
        let script = format!(
            "umask {umask}; exec \"$1\" provider add codex-main --kind codex --auth-dir \"$2\""
        );
        let output = Command::new("/bin/sh")
            .args(["-c", &script, "sh"])
            .arg(env!("CARGO_BIN_EXE_af"))
            .arg(&auth)
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("AF_SELF_OFFLINE", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "umask {umask}: {}",
            stderr(&output)
        );
        let second_script = format!(
            "umask {umask}; exec \"$1\" provider add codex-second --kind codex --auth-dir \"$2\""
        );
        let second = Command::new("/bin/sh")
            .args(["-c", &second_script, "sh"])
            .arg(env!("CARGO_BIN_EXE_af"))
            .arg(&second_auth)
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("AF_SELF_OFFLINE", "1")
            .output()
            .unwrap();
        assert!(
            second.status.success(),
            "second update under umask {umask}: {}",
            stderr(&second)
        );

        let directory = root.path().join("config/af");
        let registry = directory.join("providers.toml");
        let lock = directory.join("providers.toml.lock");
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700,
            "umask {umask}"
        );
        assert_eq!(
            std::fs::metadata(&registry).unwrap().permissions().mode() & 0o777,
            0o600,
            "umask {umask}"
        );
        assert_eq!(
            std::fs::metadata(&lock).unwrap().permissions().mode() & 0o777,
            0o600,
            "umask {umask}"
        );
        let registry_text = std::fs::read_to_string(&registry).unwrap();
        assert!(registry_text.contains("codex-main"));
        assert!(registry_text.contains("codex-second"));
        let recovery = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".af-provider-recovery-")
            })
            .expect("second update creates a recovery directory")
            .path();
        assert_eq!(
            std::fs::metadata(&recovery).unwrap().permissions().mode() & 0o777,
            0o700,
            "recovery directory under umask {umask}"
        );
        for name in ["candidate", "displaced"] {
            assert_eq!(
                std::fs::metadata(recovery.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "recovery {name} under umask {umask}"
            );
        }
    }
}

#[test]
fn add_rejects_registry_state_writable_by_other_users() {
    for unsafe_target in ["directory", "registry"] {
        let root = tempfile::tempdir().unwrap();
        let auth = root.path().join("codex-auth");
        let directory = root.path().join("config/af");
        let registry = directory.join("providers.toml");
        std::fs::create_dir(&auth).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&registry, "version = 1\nproviders = []\n").unwrap();
        let target = if unsafe_target == "directory" {
            &directory
        } else {
            &registry
        };
        let mut permissions = std::fs::metadata(target).unwrap().permissions();
        permissions.set_mode(if unsafe_target == "directory" {
            0o777
        } else {
            0o666
        });
        std::fs::set_permissions(target, permissions).unwrap();

        let output = af(
            root.path(),
            &[
                "provider",
                "add",
                "codex-main",
                "--kind",
                "codex",
                "--auth-dir",
                auth.to_str().unwrap(),
            ],
        );
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("writable by another user"),
            "{}: {}",
            unsafe_target,
            stderr(&output)
        );
    }
}

#[test]
fn add_rejects_a_nonsticky_writable_rename_ancestor() {
    let root = tempfile::tempdir().unwrap();
    let shared = root.path().join("shared");
    let auth = shared.join("owned-auth");
    std::fs::create_dir(&shared).unwrap();
    std::fs::create_dir(&auth).unwrap();
    let mut shared_permissions = std::fs::metadata(&shared).unwrap().permissions();
    shared_permissions.set_mode(0o777);
    std::fs::set_permissions(&shared, shared_permissions).unwrap();
    let mut auth_permissions = std::fs::metadata(&auth).unwrap().permissions();
    auth_permissions.set_mode(0o700);
    std::fs::set_permissions(&auth, auth_permissions).unwrap();

    let output = af(
        root.path(),
        &[
            "provider",
            "add",
            "codex-main",
            "--kind",
            "codex",
            "--auth-dir",
            auth.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("writable by another user without the sticky bit"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn add_uses_the_active_cli_directory_by_default() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("codex-home");
    std::fs::create_dir(&auth).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .args(["provider", "add", "codex-main", "--kind", "codex"])
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("CODEX_HOME", &auth)
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let registry = std::fs::read_to_string(root.path().join("config/af/providers.toml")).unwrap();
    assert!(
        registry.contains(auth.canonicalize().unwrap().to_str().unwrap()),
        "{registry}"
    );
}

#[test]
fn add_refuses_ids_reserved_for_ambient_discovery() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("codex-auth");
    std::fs::create_dir(&auth).unwrap();
    let output = af(
        root.path(),
        &[
            "provider",
            "add",
            "codex-ambient",
            "--kind",
            "codex",
            "--auth-dir",
            auth.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("reserved for ambient discovery"),
        "{}",
        stderr(&output)
    );
    assert!(!root.path().join("config/af/providers.toml").exists());
}

#[test]
fn concurrent_adds_are_serialized_without_losing_entries() {
    let root = tempfile::tempdir().unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let mut handles = Vec::new();
    for index in 0..8 {
        let home = root.path().to_path_buf();
        let auth = home.join(format!("codex-auth-{index}"));
        std::fs::create_dir(&auth).unwrap();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let id = format!("codex-{index}");
            let output = af(
                &home,
                &[
                    "provider",
                    "add",
                    &id,
                    "--kind",
                    "codex",
                    "--auth-dir",
                    auth.to_str().unwrap(),
                ],
            );
            assert!(output.status.success(), "{}", stderr(&output));
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let registry = std::fs::read_to_string(root.path().join("config/af/providers.toml")).unwrap();
    let value: toml::Value = toml::from_str(&registry).unwrap();
    let providers = value["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 8, "{registry}");
    for index in 0..8 {
        assert!(
            providers
                .iter()
                .any(|provider| provider["id"].as_str() == Some(&format!("codex-{index}"))),
            "{registry}"
        );
    }
}

#[test]
fn add_normalizes_valid_inline_and_empty_provider_arrays() {
    for existing in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config/af");
        let first = root.path().join("codex-first");
        let second = root.path().join("codex-second");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let registry = if existing {
            format!(
                "version = 1\nproviders = [{{ id = \"codex-first\", kind = \"codex\", auth_dir = {:?} }}]\n",
                first.canonicalize().unwrap().to_str().unwrap()
            )
        } else {
            "version = 1\nproviders = []\n".to_string()
        };
        std::fs::write(config.join("providers.toml"), registry).unwrap();
        let output = af(
            root.path(),
            &[
                "provider",
                "add",
                "codex-second",
                "--kind",
                "codex",
                "--auth-dir",
                second.to_str().unwrap(),
            ],
        );
        assert!(output.status.success(), "{}", stderr(&output));
        let registry = std::fs::read_to_string(config.join("providers.toml")).unwrap();
        let value: toml::Value = toml::from_str(&registry).unwrap();
        assert_eq!(
            value["providers"].as_array().unwrap().len(),
            usize::from(existing) + 1,
            "{registry}"
        );
    }
}

#[test]
fn add_refuses_to_exceed_the_registry_entry_limit() {
    const LIMIT: usize = 32;
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config/af");
    std::fs::create_dir_all(&config).unwrap();
    let mut registry = "version = 1\n".to_string();
    for index in 0..LIMIT {
        let auth = root.path().join(format!("auth-{index}"));
        std::fs::create_dir(&auth).unwrap();
        registry.push_str(&format!(
            "[[providers]]\nid = \"codex-{index}\"\nkind = \"codex\"\nauth_dir = {:?}\n",
            auth.canonicalize().unwrap().to_str().unwrap()
        ));
    }
    let path = config.join("providers.toml");
    std::fs::write(&path, &registry).unwrap();
    let extra = root.path().join("auth-extra");
    std::fs::create_dir(&extra).unwrap();
    let output = af(
        root.path(),
        &[
            "provider",
            "add",
            "codex-extra",
            "--kind",
            "codex",
            "--auth-dir",
            extra.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("limit of 32 entries"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), registry);
}

/// A registry of Codex contexts under `root`, each with its own auth directory, written by hand
/// with a comment above the file and above every entry.
fn commented_registry(root: &Path, ids: &[&str]) -> (std::path::PathBuf, String) {
    let config = root.join("config/af");
    std::fs::create_dir_all(&config).unwrap();
    let mut registry = "# machine-local\nversion = 1\n".to_string();
    for id in ids {
        let auth = root.join(format!("{id}-auth"));
        std::fs::create_dir(&auth).unwrap();
        registry.push_str(&format!(
            "\n# the {id} login\n[[providers]]\nid = \"{id}\"\nkind = \"codex\"\nauth_dir = {:?}\n",
            auth.canonicalize().unwrap().to_str().unwrap()
        ));
    }
    let path = config.join("providers.toml");
    std::fs::write(&path, &registry).unwrap();
    (path, registry)
}

fn registered_ids(path: &Path) -> Vec<String> {
    let registry = std::fs::read_to_string(path).unwrap();
    let value: toml::Value = toml::from_str(&registry).unwrap();
    assert_eq!(value["version"].as_integer(), Some(1), "{registry}");
    let providers = value.get("providers").and_then(toml::Value::as_array);
    let ids = providers.into_iter().flatten();
    ids.map(|provider| provider["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn remove_drops_the_named_entry_and_touches_nothing_else() {
    let root = tempfile::tempdir().unwrap();
    let (path, registry) = commented_registry(root.path(), &["codex-first", "codex-second"]);

    let output = af(root.path(), &["provider", "remove", "codex-first"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let first = root.path().join("codex-first-auth").canonicalize().unwrap();
    let removed = format!(
        "provider codex-first removed from {} (codex, {})",
        path.display(),
        first.display()
    );
    assert!(stdout.contains(&removed), "{stdout}");
    assert!(
        stdout.contains("auth directories and their logins were not touched"),
        "{stdout}"
    );
    // The entry and its own comment are gone; the rest of the file is as it was written.
    let entry = |id: &str| registry.find(&format!("\n# the {id} login")).unwrap();
    let (first_entry, second_entry) = (entry("codex-first"), entry("codex-second"));
    let expected = format!("{}{}", &registry[..first_entry], &registry[second_entry..]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    assert_eq!(registered_ids(&path), ["codex-second"]);
    assert!(first.is_dir(), "the auth directory is not af's to delete");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // The registry it replaced is preserved, byte for byte.
    let preserved = stdout
        .lines()
        .find_map(|line| line.strip_prefix("previous provider registry preserved at "))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert_eq!(std::fs::read_to_string(preserved).unwrap(), registry);

    // Removing the last entry leaves a valid, empty registry that `add` extends again.
    let output = af(root.path(), &["provider", "remove", "codex-second"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(registered_ids(&path).is_empty());
    let output = af(
        root.path(),
        &[
            "provider",
            "add",
            "codex-first",
            "--kind",
            "codex",
            "--auth-dir",
            first.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(registered_ids(&path), ["codex-first"]);
}

#[test]
fn remove_refuses_what_is_not_a_registry_entry() {
    // No registry: nothing is registered, and asking creates neither the registry nor its lock.
    let empty = tempfile::tempdir().unwrap();
    let output = af(empty.path(), &["provider", "remove", "codex-main"]);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(
        error.contains("provider `codex-main` is not registered")
            && error.contains("does not exist"),
        "{error}"
    );
    let config = empty.path().join("config/af");
    assert!(!config.join("providers.toml").exists());
    assert!(!config.join("providers.toml.lock").exists());

    let root = tempfile::tempdir().unwrap();
    let (path, registry) = commented_registry(root.path(), &["codex-main"]);
    for (ids, refusal) in [
        (
            &["codex-other"][..],
            "provider `codex-other` is not registered in",
        ),
        // One unknown ID refuses the whole command: the registered one stays.
        (
            &["codex-main", "codex-other"],
            "provider `codex-other` is not registered in",
        ),
        (
            &["codex-other", "codex-third"],
            "providers `codex-other`, `codex-third` are not registered in",
        ),
        // Status lists an ambient label, but no registry entry stands behind it.
        (
            &["codex-ambient"],
            "`codex-ambient` is an ambient discovery label, not a registry entry",
        ),
        (
            &["claude-ambient", "codex-main"],
            "CLAUDE_CONFIG_DIR, or ~/.claude",
        ),
        (&["../codex-main"], "provider id `../codex-main` is unsafe"),
    ] {
        let mut args = vec!["provider", "remove"];
        args.extend(ids);
        let output = af(root.path(), &args);
        assert_eq!(output.status.code(), Some(1), "{ids:?}");
        assert!(stderr(&output).contains(refusal), "{}", stderr(&output));
        assert!(output.stdout.is_empty(), "{ids:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), registry, "{ids:?}");
    }
    // An ID is required: clap's usage error, not a silent success.
    let output = af(root.path(), &["provider", "remove"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), registry);
}

#[test]
fn remove_repairs_a_registry_whose_auth_directories_are_gone() {
    let root = tempfile::tempdir().unwrap();
    let ids = ["codex-kept", "codex-gone", "codex-lost"];
    let (path, registry) = commented_registry(root.path(), &ids);
    std::fs::remove_dir(root.path().join("codex-gone-auth")).unwrap();
    std::fs::remove_dir(root.path().join("codex-lost-auth")).unwrap();
    // One missing auth directory makes the whole registry invalid: nothing can be added.
    let extra = root.path().join("codex-extra-auth");
    std::fs::create_dir(&extra).unwrap();
    let add = |root: &Path| {
        let extra = extra.to_str().unwrap();
        let args = [
            "provider",
            "add",
            "codex-extra",
            "--kind",
            "codex",
            "--auth-dir",
            extra,
        ];
        af(root, &args)
    };
    let output = add(root.path());
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("cannot be inspected"),
        "{}",
        stderr(&output)
    );

    // Removing one stale entry would publish a registry that is still invalid, so it is refused
    // and names the other one.
    let output = af(root.path(), &["provider", "remove", "codex-gone"]);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(
        error.contains("removing `codex-gone` would leave provider registry")
            && error.contains("provider `codex-lost` auth_dir")
            && error.contains("nothing was removed"),
        "{error}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), registry);

    // Both in one command is the repair.
    let output = af(
        root.path(),
        &["provider", "remove", "codex-gone", "codex-lost"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(registered_ids(&path), ["codex-kept"]);
    let output = add(root.path());
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(registered_ids(&path), ["codex-kept", "codex-extra"]);
}
