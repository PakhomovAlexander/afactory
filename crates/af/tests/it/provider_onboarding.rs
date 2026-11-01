//! Agent-safe Provider onboarding: what `af provider setup` and `af provider status` promise a
//! caller that is not a human at a terminal.
//!
//! The boundary under test is narrow and load-bearing. An automated caller must be able to reach
//! a determinate answer — registered, or exactly what a human has to do — without af ever
//! starting an OAuth exchange whose URL and code would land in that caller's transcript. These
//! tests therefore assert absence as much as presence: no login child, no Provider-authored text
//! in the documents, no account identity anywhere.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

/// A shell stand-in for an official Provider CLI, on a `PATH` that holds nothing else. It answers
/// af's capability check with a page declaring every option af passes (ADR-0146).
fn fake_provider(root: &Path, name: &str, script: &str) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join(name);
    std::fs::write(&path, crate::provider_cli::with_help(name, script)).unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    bin
}

/// `af` with piped standard streams: the non-interactive shape every agent and CI job has.
fn af(home: &Path, bin: &Path, args: &[&str]) -> Output {
    crate::common::af()
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

/// The fake CLI and its auth directory, which exists and holds a login marker: setup without
/// `--login` never creates a directory, and status never probes one without a marker.
fn logged_out_claude(root: &Path) -> (PathBuf, PathBuf) {
    let auth = root.join("claude-auth");
    std::fs::create_dir(&auth).unwrap();
    std::fs::write(auth.join(".claude.json"), "{}").unwrap();
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
    valid("provider-setup-v2.json", &value);
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
    // Under --json the usage error is the one `af/error@2` document, and nothing else.
    let document: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("not one document: {}", out(&output)));
    assert_eq!(document["schema"], "af/error@2", "{document}");
    assert_eq!(document["exit_code"], 2, "{document}");
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
    valid("provider-setup-v2.json", &value);
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
    valid("provider-status-v5.json", &reported);

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
    // The next step of a first run follows a registration (ADR-0210).
    assert!(
        stdout
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("next: af onboard (in a repository)")),
        "{stdout}"
    );
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
/// fail without saying anything about the login. The app-server answers the compatibility
/// check's `initialize` (ADR-0146) and fails only the rate-limit request, which default status
/// never sends.
#[test]
fn status_keeps_authentication_when_optional_usage_is_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("codex-auth");
    std::fs::create_dir(&auth).unwrap();
    std::fs::write(auth.join("config.toml"), "").unwrap();
    let bin = fake_provider(
        root.path(),
        "codex",
        r#"#!/bin/sh
if [ "$1" = login ] && [ "$2" = status ]; then
  printf '%s\n' 'Logged in using ChatGPT' >&2
  exit 0
fi
if [ "$1" = app-server ]; then
  while IFS= read -r request; do
    case "$request" in
      *'"id":1'*) printf '%s\n' '{"id":1,"result":{}}' ;;
      *'"id":2'*) printf '%s\n' app-server >> "$CODEX_HOME/usage-probe-log"; exit 1 ;;
    esac
  done
  exit 0
fi
exit 64
"#,
    );
    // CODEX_HOME names the registered context too, so the ambient candidate folds into it and
    // the document has exactly one Provider to reason about.
    let run = |args: &[&str]| {
        crate::common::af()
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
    valid("provider-status-v5.json", &reported);
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
    valid("provider-status-v5.json", &reported);
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

#[test]
fn pi_setup_status_and_exact_model_catalog_are_token_free_and_secret_free() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let auth = root.path().join("auth");
    let (sdk, path, state) = crate::fake_models::install_pi(root.path(), &auth, "review");
    let invoke = |args: &[&str]| {
        crate::common::af()
            .args(args)
            .current_dir(&work)
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("PATH", &path)
            .env("AF_PROVIDERS_FILE", root.path().join("providers.toml"))
            .env("NODE_OPTIONS", "--import=/DO_NOT_LOAD")
            .env("OPENAI_API_KEY", "DO_NOT_USE_AMBIENT")
            .output()
            .unwrap()
    };
    let setup = invoke(&[
        "provider",
        "setup",
        "pi-personal",
        "--kind",
        "pi",
        "--backend",
        "openai-codex",
        "--auth-dir",
        auth.to_str().unwrap(),
        "--sdk-entrypoint",
        sdk.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        setup.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr)
    );
    let value: Value = serde_json::from_slice(&setup.stdout).unwrap();
    valid("pi-provider-setup-v2.json", &value);
    for (args, schema) in [
        (
            vec!["provider", "status", "--json"],
            "provider-status-v5.json",
        ),
        (
            vec!["provider", "models", "pi-personal", "--json"],
            "provider-models-v2.json",
        ),
        (
            vec![
                "provider",
                "models",
                "pi-personal",
                "--resolve",
                "o3/high",
                "--json",
            ],
            "provider-models-v2.json",
        ),
    ] {
        let out = invoke(&args);
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        valid(schema, &value);
        if schema == "provider-status-v5.json" {
            assert_eq!(value["schema"], "af/provider-status@5");
            assert_eq!(value["providers"][0]["kind"], "pi");
            assert_eq!(value["providers"][0]["auth"], "authenticated");
        }
        let text = String::from_utf8(out.stdout).unwrap();
        for secret in [
            "synthetic-account",
            "SYNTHETIC_ACCESS",
            "SYNTHETIC_REFRESH",
            "DO_NOT_USE_AMBIENT",
        ] {
            assert!(!text.contains(secret));
        }
    }
    assert!(
        !state.with_extension("json.calls").exists(),
        "token-free command started inference"
    );
    let unsupported = invoke(&[
        "provider",
        "setup",
        "pi-other",
        "--kind",
        "pi",
        "--backend",
        "anthropic",
        "--auth-dir",
        auth.to_str().unwrap(),
        "--sdk-entrypoint",
        sdk.to_str().unwrap(),
        "--json",
    ]);
    assert!(!unsupported.status.success());
    valid("pi-provider-setup-v2.json", &document(&unsupported));
    assert!(err(&unsupported).starts_with("af provider setup: "));

    // Losing one optional SDK installation leaves the registry readable and removable.
    std::fs::remove_file(&sdk).unwrap();
    let status = invoke(&["provider", "status", "--json"]);
    let value: Value = serde_json::from_slice(&status.stdout).unwrap();
    valid("provider-status-v5.json", &value);
    assert!(value["registry"]["warning"].is_null());
    assert_ne!(value["providers"][0]["auth"], "authenticated");
    assert!(!String::from_utf8_lossy(&status.stderr).contains("registry"));
    let text = invoke(&["provider", "status"]);
    assert!(String::from_utf8_lossy(&text.stdout).contains("Pi installation is missing"));
    let removed = invoke(&["provider", "remove", "pi-personal"]);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
}

#[test]
fn pi_node_resolution_and_setup_refuse_repository_owned_path_before_spawn() {
    for marker in ["git-directory", "git-worktree-file", "af-directory"] {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("repository/nested/cwd");
        std::fs::create_dir_all(&work).unwrap();
        let auth = root.path().join("auth");
        let (sdk, external_path, _) = crate::fake_models::install_pi(root.path(), &auth, "review");
        let invoke = |path: &std::ffi::OsStr, args: &[&str]| {
            crate::common::af()
                .args(args)
                .current_dir(&work)
                .env("HOME", root.path())
                .env("XDG_CONFIG_HOME", root.path().join("config"))
                .env("AF_PROVIDERS_FILE", root.path().join("providers.toml"))
                .env_remove("CODEX_HOME")
                .env_remove("CLAUDE_CONFIG_DIR")
                .env("PATH", path)
                .output()
                .unwrap()
        };
        let setup_args = [
            "provider",
            "setup",
            "pi-safe",
            "--kind",
            "pi",
            "--backend",
            "openai-codex",
            "--auth-dir",
            auth.to_str().unwrap(),
            "--sdk-entrypoint",
            sdk.to_str().unwrap(),
            "--json",
        ];
        // The same nested CWD permits a legitimate external Node installation.
        let setup = invoke(external_path.as_ref(), &setup_args);
        assert!(setup.status.success(), "{}", err(&setup));
        let repository = root.path().join("repository");
        match marker {
            "git-directory" => std::fs::create_dir(repository.join(".git")).unwrap(),
            "git-worktree-file" => {
                std::fs::write(repository.join(".git"), "gitdir: /synthetic/worktree\n").unwrap()
            }
            _ => std::fs::create_dir(repository.join(".af")).unwrap(),
        }
        let bin = repository.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let trap = bin.join("node");
        let executed = root.path().join("node-executed");
        std::fs::write(
            &trap,
            format!("#!/bin/sh\n: > '{}'\nexit 99\n", executed.display()),
        )
        .unwrap();
        std::fs::set_permissions(&trap, std::fs::Permissions::from_mode(0o700)).unwrap();
        let absolute_path = bin.canonicalize().unwrap();
        for args in [&["provider", "status", "--json"][..], &setup_args[..]] {
            let output = invoke(absolute_path.as_os_str(), args);
            assert!(!executed.exists(), "executed repository Node for {marker}");
            let value = document(&output);
            if args[1] == "setup" {
                assert_eq!(code(&output), 3, "{}", err(&output));
                valid("pi-provider-setup-v2.json", &value);
                assert!(
                    value["detail"]
                        .as_str()
                        .unwrap()
                        .contains("outside repository-owned paths")
                );
                assert!(err(&output).starts_with("af provider setup: "));
            } else {
                valid("provider-status-v5.json", &value);
                assert_ne!(value["providers"][0]["auth"], "authenticated");
                let human = invoke(absolute_path.as_os_str(), &["provider", "status"]);
                assert!(
                    out(&human).contains("outside repository-owned paths"),
                    "{}",
                    out(&human)
                );
            }
            assert!(!executed.exists(), "executed repository Node for {marker}");
        }
    }
}

#[test]
fn pi_setup_discovers_managed_launcher_without_executing_it_and_refuses_unsafe_versions() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let auth = root.path().join("auth");
    let (_, path, state) = crate::fake_models::install_pi(root.path(), &auth, "review");
    let agent = root.path().join("agent");
    let install = agent.join("install");
    let release = install.join("releases/1.4.0");
    std::fs::create_dir_all(release.parent().unwrap()).unwrap();
    std::fs::rename(root.path().join("installed"), &release).unwrap();
    let launcher = agent.join("bin/pi");
    std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    let marker = root.path().join("launcher-executed");
    std::fs::write(
        &launcher,
        format!("#!/bin/sh\n: > '{}'\nexit 99\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(&launcher, Path::new(&path).join("pi")).unwrap();
    let current = install.join("current-version");
    let setup = || {
        crate::common::af()
            .args([
                "provider",
                "setup",
                "pi-managed",
                "--kind",
                "pi",
                "--backend",
                "openai-codex",
                "--auth-dir",
                auth.to_str().unwrap(),
                "--json",
            ])
            .current_dir(&work)
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("PATH", &path)
            .env("AF_PROVIDERS_FILE", root.path().join("providers.toml"))
            .output()
            .unwrap()
    };
    let refused = |output: Output| {
        assert_eq!(code(&output), 3, "{} {}", out(&output), err(&output));
        let value = document(&output);
        valid("pi-provider-setup-v2.json", &value);
        assert_eq!(value["outcome"], "human_action_required");
        assert!(err(&output).starts_with("af provider setup: "));
        assert!(err(&output).contains(value["detail"].as_str().unwrap()));
        assert!(!root.path().join("providers.toml").exists());
        assert!(!marker.exists());
    };
    // The full malformed-version table runs in-process in providers::pi::tests.
    std::fs::write(&current, "../1.4.0").unwrap();
    refused(setup());
    std::fs::write(&current, "1.4.0\n").unwrap();
    let saved_version = root.path().join("saved-version");
    std::fs::rename(&current, &saved_version).unwrap();
    std::os::unix::fs::symlink(&saved_version, &current).unwrap();
    refused(setup());
    std::fs::remove_file(&current).unwrap();
    std::fs::rename(&saved_version, &current).unwrap();
    // The previous implementation derived agent/index.js, which does not exist.
    assert!(!agent.join("index.js").exists());
    let sdk = release.join("node_modules/@earendil-works/pi-coding-agent/dist/index.js");
    let saved = root.path().join("saved-sdk.js");
    std::fs::rename(&sdk, &saved).unwrap();
    std::os::unix::fs::symlink(&saved, &sdk).unwrap();
    refused(setup());
    std::fs::remove_file(&sdk).unwrap();
    std::fs::rename(&saved, &sdk).unwrap();
    let output = setup();
    assert_eq!(code(&output), 0, "{} {}", out(&output), err(&output));
    valid("pi-provider-setup-v2.json", &document(&output));
    assert_eq!(document(&output)["outcome"], "registered");
    assert!(err(&output).is_empty());
    let registry = std::fs::read_to_string(root.path().join("providers.toml")).unwrap();
    assert!(registry.contains(sdk.to_str().unwrap()));
    assert!(!marker.exists(), "discovery executed the managed launcher");
    assert!(
        !state.with_extension("json.calls").exists(),
        "setup started inference"
    );
}

#[test]
fn pi_failed_setup_emits_safe_json_and_human_stderr_for_probe_and_registration_conflict() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let auth = root.path().join("auth");
    let (sdk, path, _) = crate::fake_models::install_pi(root.path(), &auth, "review");
    let setup = |id: &str, json: bool| {
        let mut command = crate::common::af();
        command.args([
            "provider",
            "setup",
            id,
            "--kind",
            "pi",
            "--backend",
            "openai-codex",
            "--auth-dir",
            auth.to_str().unwrap(),
            "--sdk-entrypoint",
            sdk.to_str().unwrap(),
        ]);
        if json {
            command.arg("--json");
        }
        command
            .current_dir(&work)
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("PATH", &path)
            .env("AF_PROVIDERS_FILE", root.path().join("providers.toml"))
            .output()
            .unwrap()
    };
    let credential = std::fs::read(auth.join("auth.json")).unwrap();
    std::fs::write(
        auth.join("auth.json"),
        r#"{"openai-codex":{"type":"api_key","key":"RAW_AUTH_MUST_NOT_ESCAPE"}}"#,
    )
    .unwrap();
    let check = |output: &Output, expected_code, outcome: &str, json| {
        assert_eq!(
            code(output),
            expected_code,
            "{} {}",
            out(output),
            err(output)
        );
        if json {
            let value = document(output);
            valid("pi-provider-setup-v2.json", &value);
            assert_eq!(value["outcome"], outcome);
            assert!(err(output).contains(value["detail"].as_str().unwrap()));
        } else {
            assert!(out(output).starts_with("provider "));
        }
        assert!(
            err(output).starts_with("af provider setup: "),
            "{}",
            err(output)
        );
        assert!(err(output).contains(&format!("({outcome})")));
        for secret in [
            "RAW_AUTH_MUST_NOT_ESCAPE",
            "synthetic-account",
            "SYNTHETIC_REFRESH",
        ] {
            assert!(!out(output).contains(secret));
            assert!(!err(output).contains(secret));
        }
    };
    for json in [true, false] {
        check(&setup("pi-one", json), 3, "human_action_required", json);
    }
    std::fs::write(auth.join("auth.json"), credential).unwrap();
    assert_eq!(code(&setup("pi-one", true)), 0);
    let repeated = setup("pi-one", true);
    assert_eq!(code(&repeated), 0);
    valid("pi-provider-setup-v2.json", &document(&repeated));
    assert_eq!(document(&repeated)["outcome"], "already_registered");
    assert!(err(&repeated).is_empty());
    for json in [true, false] {
        check(&setup("pi-two", json), 5, "registry_conflict", json);
    }
    let registry = root.path().join("providers.toml");
    std::fs::write(&registry, "RAW_REGISTRY_MUST_NOT_ESCAPE invalid TOML").unwrap();
    let refused = setup("pi-one", true);
    check(&refused, 3, "human_action_required", true);
    assert!(!out(&refused).contains("RAW_REGISTRY_MUST_NOT_ESCAPE"));
    assert!(!err(&refused).contains("RAW_REGISTRY_MUST_NOT_ESCAPE"));
    assert_eq!(
        std::fs::read_to_string(registry).unwrap(),
        "RAW_REGISTRY_MUST_NOT_ESCAPE invalid TOML"
    );
}

#[test]
fn pi_early_setup_refusals_have_schema_valid_json_and_safe_stderr_without_invented_identity() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let auth = root.path().join("auth");
    let (sdk, path, state) = crate::fake_models::install_pi(root.path(), &auth, "review");
    let auth = auth.to_str().unwrap();
    let sdk = sdk.to_str().unwrap();
    let cases: Vec<(&str, Vec<&str>, bool, bool)> = vec![
        ("../RAW_REQUEST_MUST_NOT_ESCAPE", vec![], false, false),
        ("codex-ambient", vec![], false, false),
        ("pi-one", vec![], true, false),
        (
            "pi-one",
            vec!["--backend", "RAW_REQUEST_MUST_NOT_ESCAPE"],
            true,
            false,
        ),
        ("pi-one", vec!["--backend", "openai-codex"], true, true),
        (
            "pi-one",
            vec!["--backend", "openai-codex", "--auth-dir", auth],
            true,
            true,
        ),
        (
            "pi-one",
            vec!["--backend", "openai-codex", "--auth-dir", "relative"],
            true,
            true,
        ),
        (
            "pi-one",
            vec![
                "--backend",
                "openai-codex",
                "--auth-dir",
                "/RAW_REQUEST_MUST_NOT_ESCAPE",
            ],
            true,
            true,
        ),
        (
            "pi-one",
            vec![
                "--backend",
                "openai-codex",
                "--auth-dir",
                auth,
                "--sdk-entrypoint",
                "/RAW_REQUEST_MUST_NOT_ESCAPE",
            ],
            true,
            true,
        ),
    ];
    for (id, args, valid_id, valid_backend) in cases {
        let output = crate::common::af()
            .args(["provider", "setup", id, "--kind", "pi", "--json"])
            .args(args)
            .current_dir(&work)
            .env("PATH", &path)
            .env("AF_PROVIDERS_FILE", root.path().join("providers.toml"))
            .output()
            .unwrap();
        assert_eq!(code(&output), 3, "{} {}", out(&output), err(&output));
        let value = document(&output);
        valid("pi-provider-setup-v2.json", &value);
        assert_eq!(!value["id"].is_null(), valid_id);
        assert_eq!(!value["backend"].is_null(), valid_backend);
        assert_eq!(value["outcome"], "human_action_required");
        assert!(err(&output).starts_with("af provider setup: "));
        assert!(err(&output).contains(value["detail"].as_str().unwrap()));
        assert!(!out(&output).contains("RAW_REQUEST_MUST_NOT_ESCAPE"));
        assert!(!err(&output).contains("RAW_REQUEST_MUST_NOT_ESCAPE"));
        assert!(!root.path().join("providers.toml").exists());
    }
    // Explicit installed entrypoint remains covered by the successful setup test above.
    assert!(Path::new(sdk).is_file());
    assert!(!state.with_extension("json.calls").exists());
}
