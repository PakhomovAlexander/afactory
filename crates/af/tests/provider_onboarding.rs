//! Agent-safe Provider onboarding: what `af provider setup` and `af provider status` promise a
//! caller that is not a human at a terminal.
//!
//! The boundary under test is narrow and load-bearing. An automated caller must be able to reach
//! a determinate answer — registered, or exactly what a human has to do — without af ever
//! starting an OAuth exchange whose URL and code would land in that caller's transcript. These
//! tests therefore assert absence as much as presence: no login child, no Provider-authored text
//! in the documents, no account identity anywhere.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// A shell stand-in for an official Provider CLI, on a `PATH` that holds nothing else.
fn fake_provider(root: &Path, name: &str, script: &str) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join(name);
    std::fs::write(&path, script).unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    bin
}

/// `af` with piped standard streams: the non-interactive shape every agent and CI job has.
fn af(home: &Path, bin: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("PATH", bin)
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap()
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

fn document(output: &Output) -> Value {
    let stdout = out(output);
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("stdout is not one document: {error}\n{stdout}"))
}

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// Validate a document against the published schema, so the CLI and `schemas/` cannot drift.
fn valid(schema: &str, value: &Value) {
    let directory = workspace().join("schemas");
    let mut registry = jsonschema::Registry::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|suffix| suffix.to_str()) != Some("json") {
            continue;
        }
        let contents: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let Some(id) = contents["$id"].as_str().map(str::to_owned) else {
            continue;
        };
        registry = registry
            .add(id, jsonschema::Resource::from_contents(contents))
            .unwrap();
    }
    let bytes = std::fs::read(directory.join(schema)).unwrap();
    let root: Value = serde_json::from_slice(&bytes).unwrap();
    let prepared = registry.prepare().unwrap();
    let validator = jsonschema::options()
        .with_registry(&prepared)
        .build(&root)
        .unwrap();
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|error| format!("{error} at {}", error.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{schema}: {}", errors.join("; "));
}

/// A Claude CLI that is logged out until a login runs, and whose logged-in answer carries the
/// account identity the documents must never repeat.
const LOGGED_OUT_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = auth ] && [ "$2" = status ] && [ "$3" = --json ]; then
  if [ -f "$CLAUDE_CONFIG_DIR/logged-in" ]; then
    printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"person@example.test","organizationId":"org_SECRET_0001","subscriptionType":"max"}'
    exit 0
  fi
  printf '%s\n' '{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}'
  exit 1
fi
if [ "$1" = auth ] && [ "$2" = login ]; then
  printf '%s\n' "$*" >> "$CLAUDE_CONFIG_DIR/login-log"
  : > "$CLAUDE_CONFIG_DIR/logged-in"
  exit 0
fi
exit 64
"#;

fn logged_out_claude(root: &Path) -> (PathBuf, PathBuf) {
    let auth = root.join("claude-auth");
    let bin = fake_provider(root, "claude", LOGGED_OUT_CLAUDE);
    (auth, bin)
}

fn claude_setup(auth: &Path, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "provider".to_string(),
        "setup".to_string(),
        "claude-main".to_string(),
        "--kind".to_string(),
        "claude".to_string(),
        "--auth-dir".to_string(),
        auth.to_str().unwrap().to_string(),
    ];
    args.extend(extra.iter().copied().map(String::from));
    args
}

fn borrowed(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

/// The core refusal: a login is never started when the process is not at a terminal, even when
/// the operator asked for one.
#[test]
fn non_interactive_setup_never_starts_a_login_child() {
    let root = tempfile::tempdir().unwrap();
    let (auth, bin) = logged_out_claude(root.path());
    let args = claude_setup(&auth, &["--login"]);

    let output = af(root.path(), &bin, &borrowed(&args));

    assert_eq!(code(&output), 3, "{}", err(&output));
    assert!(
        !auth.join("login-log").exists() && !auth.join("logged-in").exists(),
        "setup started an official login without an interactive terminal"
    );
    let stdout = out(&output);
    let stderr = err(&output);
    assert!(stdout.contains("human action required:"), "{stdout}");
    assert!(
        stdout.contains("not attached to an interactive terminal"),
        "{stdout}"
    );
    assert!(stdout.contains("private interactive terminal"), "{stdout}");
    let resolved = auth.canonicalize().unwrap();
    let expected = format!(
        "af provider setup claude-main --kind claude --auth-dir {} --login",
        resolved.display()
    );
    assert!(stdout.contains(&expected), "{stdout}");
    // The result is on stdout; stderr carries the diagnostic for the same outcome, and neither
    // stream borrows the other's job.
    assert!(stderr.contains("af provider setup: "), "{stderr}");
    assert!(stderr.contains("(human_action_required)"), "{stderr}");
    assert!(!stdout.contains("af provider setup: "), "{stdout}");
    assert!(!stderr.contains("human action required:"), "{stderr}");
    assert!(!root.path().join("config/af/providers.toml").exists());
}

/// Without the opt-in there is no login either, and the next action names `--login` explicitly.
#[test]
fn setup_requires_an_explicit_opt_in_before_any_login() {
    let root = tempfile::tempdir().unwrap();
    let (auth, bin) = logged_out_claude(root.path());
    let args = claude_setup(&auth, &["--json"]);

    let output = af(root.path(), &bin, &borrowed(&args));

    assert_eq!(code(&output), 3, "{}", err(&output));
    assert!(!auth.join("login-log").exists());
    let value = document(&output);
    valid("provider-setup-v1.json", &value);
    assert_eq!(value["result"], "human_action_required");
    assert_eq!(value["login_launched"], false);
    assert_eq!(value["next_action"]["kind"], "interactive_login");
    let command = value["next_action"]["command"].as_str().unwrap();
    assert!(command.ends_with("--login"), "{command}");
    assert_eq!(value["provider"]["auth"], "not_authenticated");
    assert_eq!(value["provider"]["usability"], "unusable");
    assert_eq!(value["provider"]["registered"], false);
}

/// JSON is the automation surface and login is the private-terminal surface. Combining them
/// would let Provider output corrupt the one-document contract and make `login_launched: false`
/// untrue, so clap refuses the combination before any Provider process is started.
#[test]
fn setup_refuses_json_with_interactive_login() {
    let root = tempfile::tempdir().unwrap();
    let (auth, bin) = logged_out_claude(root.path());
    let args = claude_setup(&auth, &["--login", "--json"]);

    let output = af(root.path(), &bin, &borrowed(&args));

    assert_eq!(code(&output), 2, "{}", err(&output));
    assert!(out(&output).is_empty(), "{}", out(&output));
    let stderr = err(&output);
    assert!(stderr.contains("cannot be used with"), "{stderr}");
    assert!(!auth.join("login-log").exists());
    assert!(!auth.join("logged-in").exists());
}

/// An already-authenticated context is registrable without any login process at all.
#[test]
fn an_authenticated_context_registers_without_a_login() {
    let root = tempfile::tempdir().unwrap();
    let (auth, bin) = logged_out_claude(root.path());
    std::fs::create_dir(&auth).unwrap();
    std::fs::write(auth.join("logged-in"), "").unwrap();
    let args = claude_setup(&auth, &["--json"]);

    let output = af(root.path(), &bin, &borrowed(&args));

    assert_eq!(code(&output), 0, "{}", err(&output));
    assert!(
        !auth.join("login-log").exists(),
        "setup started a login for a context that was already authenticated"
    );
    assert!(err(&output).is_empty(), "{}", err(&output));
    let value = document(&output);
    valid("provider-setup-v1.json", &value);
    assert_eq!(value["result"], "registered");
    assert_eq!(value["provider"]["registered"], true);
    assert_eq!(value["provider"]["auth"], "authenticated");
    assert_eq!(value["provider"]["usability"], "usable_or_untested");
    let resolved = auth.canonicalize().unwrap();
    let context = Value::from(resolved.to_str().unwrap());
    assert_eq!(value["provider"]["auth_context"], context);
    let registry_path = root.path().join("config/af/providers.toml");
    let registry = std::fs::read_to_string(registry_path).unwrap();
    assert!(registry.contains("claude-main"), "{registry}");

    // Re-running is safe and still starts nothing.
    let again = af(root.path(), &bin, &borrowed(&args));
    assert_eq!(code(&again), 0, "{}", err(&again));
    assert_eq!(document(&again)["result"], "already_registered");
    assert!(!auth.join("login-log").exists());
}

/// The documents describe states, never the account behind them or what the Provider printed.
#[test]
fn documents_redact_identity_and_provider_output() {
    let root = tempfile::tempdir().unwrap();
    let (auth, bin) = logged_out_claude(root.path());
    std::fs::create_dir(&auth).unwrap();
    std::fs::write(auth.join("logged-in"), "").unwrap();
    let args = claude_setup(&auth, &["--json"]);

    let setup = af(root.path(), &bin, &borrowed(&args));
    assert_eq!(code(&setup), 0, "{}", err(&setup));
    let status = af(
        root.path(),
        &bin,
        &["provider", "status", "--json", "--usage"],
    );
    let reported = document(&status);
    valid("provider-status-v1.json", &reported);

    for rendered in [out(&setup), out(&status)] {
        for leaked in [
            "person@example.test",
            "org_SECRET_0001",
            "loggedIn",
            "apiProvider",
            "authMethod",
            "subscriptionType",
        ] {
            assert!(
                !rendered.contains(leaked),
                "document leaked `{leaked}`: {rendered}"
            );
        }
    }

    let provider = reported["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == "claude-main")
        .unwrap()
        .clone();
    assert_eq!(provider["auth"], "authenticated");
    assert_eq!(provider["credential"], "subscription");
    assert_eq!(provider["usability"], "usable_or_untested");
    assert_eq!(provider["registered"], true);
}

/// Every documented exit condition, from one place, with its stdout/stderr placement.
#[test]
fn documented_exit_conditions_are_stable() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    let authenticated = home.join("codex-auth");
    std::fs::create_dir(&authenticated).unwrap();
    let bin = fake_provider(
        home,
        "codex",
        r#"#!/bin/sh
if [ "$1" = login ] && [ "$2" = status ]; then
  printf '%s\n' 'Logged in using ChatGPT' >&2
  if [ -f "$CODEX_HOME/broken" ]; then
    exit 1
  fi
  exit 0
fi
exit 64
"#,
    );

    // 0 — an authenticated context is registered, with nothing on stderr.
    let success = af(
        home,
        &bin,
        &[
            "provider",
            "setup",
            "codex-main",
            "--kind",
            "codex",
            "--auth-dir",
            authenticated.to_str().unwrap(),
        ],
    );
    assert_eq!(code(&success), 0, "{}", err(&success));
    let stdout = out(&success);
    assert!(stdout.contains("authenticated and registered"), "{stdout}");
    assert!(err(&success).is_empty(), "{}", err(&success));

    // 5 — the same auth context under a second ID conflicts, refused before any Provider work.
    let conflict = af(
        home,
        &bin,
        &[
            "provider",
            "setup",
            "codex-other",
            "--kind",
            "codex",
            "--auth-dir",
            authenticated.to_str().unwrap(),
        ],
    );
    assert_eq!(code(&conflict), 5, "{}", err(&conflict));
    let stdout = out(&conflict);
    let stderr = err(&conflict);
    assert!(stdout.contains("registry_conflict"), "{stdout}");
    assert!(stderr.contains("registered as provider"), "{stderr}");

    // 6 — the Provider CLI answered, and its answer was not a usable login.
    let broken = home.join("codex-broken");
    std::fs::create_dir(&broken).unwrap();
    std::fs::write(broken.join("broken"), "").unwrap();
    let failed = af(
        home,
        &bin,
        &[
            "provider",
            "setup",
            "codex-broken",
            "--kind",
            "codex",
            "--auth-dir",
            broken.to_str().unwrap(),
        ],
    );
    assert_eq!(code(&failed), 6, "{}", err(&failed));
    let stdout = out(&failed);
    let stderr = err(&failed);
    assert!(stdout.contains("authentication_failed"), "{stdout}");
    assert!(stderr.contains("exited unsuccessfully"), "{stderr}");

    // 4 — no Provider CLI at all, so there is nothing to verify and nothing to log in to.
    let empty = home.join("empty-bin");
    std::fs::create_dir(&empty).unwrap();
    let absent = home.join("codex-missing");
    let missing = af(
        home,
        &empty,
        &[
            "provider",
            "setup",
            "codex-missing",
            "--kind",
            "codex",
            "--auth-dir",
            absent.to_str().unwrap(),
            "--login",
        ],
    );
    assert_eq!(code(&missing), 4, "{}", err(&missing));
    let stdout = out(&missing);
    let stderr = err(&missing);
    assert!(stdout.contains("provider_cli_missing"), "{stdout}");
    assert!(stderr.contains("is not on PATH"), "{stderr}");

    // 2 — clap still owns usage errors.
    let usage = af(home, &bin, &["provider", "setup", "codex-main"]);
    assert_eq!(code(&usage), 2, "{}", err(&usage));

    // 3 — a context that needs a login, with no opt-in.
    let second = tempfile::tempdir().unwrap();
    let (claude_auth, claude_bin) = logged_out_claude(second.path());
    let args = claude_setup(&claude_auth, &[]);
    let human = af(second.path(), &claude_bin, &borrowed(&args));
    assert_eq!(code(&human), 3, "{}", err(&human));

    // Every refusal left the registry exactly as the one success wrote it.
    let registry_path = home.join("config/af/providers.toml");
    let registry = std::fs::read_to_string(registry_path).unwrap();
    assert_eq!(registry.matches("id = ").count(), 1, "{registry}");
    assert!(registry.contains("codex-main"), "{registry}");
}

/// Status is a registry and authentication check; usage is a separate, opt-in axis that can
/// fail without saying anything about the login.
#[test]
fn status_keeps_authentication_when_optional_usage_is_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("codex-auth");
    std::fs::create_dir(&auth).unwrap();
    let bin = fake_provider(
        root.path(),
        "codex",
        r#"#!/bin/sh
if [ "$1" = login ] && [ "$2" = status ]; then
  printf '%s\n' 'Logged in using ChatGPT' >&2
  exit 0
fi
if [ "$1" = app-server ]; then
  printf '%s\n' app-server >> "$CODEX_HOME/usage-probe-log"
  exit 1
fi
exit 64
"#,
    );
    // CODEX_HOME names the registered context too, so the ambient candidate folds into it and
    // the document has exactly one Provider to reason about.
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_af"))
            .args(args)
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("CODEX_HOME", &auth)
            .env("PATH", &bin)
            .env("AF_SELF_OFFLINE", "1")
            .output()
            .unwrap()
    };

    let registered = run(&[
        "provider",
        "add",
        "codex-main",
        "--kind",
        "codex",
        "--auth-dir",
        auth.to_str().unwrap(),
    ]);
    assert_eq!(code(&registered), 0, "{}", err(&registered));

    // Default: fast. No usage probe is started at all.
    let fast = run(&["provider", "status", "--json"]);
    assert_eq!(code(&fast), 0, "{}", err(&fast));
    let reported = document(&fast);
    valid("provider-status-v1.json", &reported);
    assert_eq!(reported["usage_requested"], false);
    let provider = reported["providers"][0].clone();
    assert_eq!(provider["id"], "codex-main");
    assert_eq!(provider["auth"], "authenticated");
    assert_eq!(provider["usage"]["state"], "not_requested");
    assert!(
        !auth.join("usage-probe-log").exists(),
        "the default status probed usage"
    );

    // Opt-in: the probe runs, fails, and changes nothing about authentication.
    let probed = run(&["provider", "status", "--json", "--usage"]);
    assert_eq!(code(&probed), 7, "{}", err(&probed));
    let reported = document(&probed);
    valid("provider-status-v1.json", &reported);
    assert_eq!(reported["usage_requested"], true);
    assert_eq!(reported["exit_code"], 7);
    let provider = reported["providers"][0].clone();
    assert_eq!(provider["auth"], "authenticated");
    assert_eq!(provider["usability"], "usable_or_untested");
    assert_eq!(provider["usage"]["state"], "unavailable");
    assert_eq!(provider["usage"]["windows"], Value::Array(Vec::new()));
    assert!(auth.join("usage-probe-log").is_file());
    let stderr = err(&probed);
    assert!(stderr.contains("authentication is unaffected"), "{stderr}");
}
