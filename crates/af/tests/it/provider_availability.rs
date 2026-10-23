//! Machine-local suspension never asks a disabled harness anything.
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn command(home: &Path) -> Command {
    let mut command = crate::common::af();
    command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("AF_PROVIDERS_FILE", home.join("config/af/providers.toml"))
        .env("CLAUDE_CONFIG_DIR", home.join("auth"))
        .env("PATH", home.join("bin"));
    command
}
fn run(home: &Path, args: &[&str]) -> Output {
    command(home).args(args).output().unwrap()
}
fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
fn fixture(home: &Path) -> PathBuf {
    let bin = home.join("bin");
    let auth = home.join("auth");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&auth).unwrap();
    let marker = home.join("spawned");
    // Mark *all* invocations, including help/version/account/model probes.
    let script = format!(
        "#!/bin/sh\nprintf called >> '{}'\nprintf '%s\\n' '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"subscriptionType\":\"max\"}}'\n",
        marker.display()
    );
    let script = crate::provider_cli::with_help("claude", &script);
    let script = script.replacen(
        '\n',
        &format!("\nprintf called >> '{}'\n", marker.display()),
        1,
    );
    std::fs::write(bin.join("claude"), script).unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o700)).unwrap();
    success(run(
        home,
        &[
            "provider",
            "add",
            "fixture",
            "--kind",
            "claude",
            "--auth-dir",
            auth.to_str().unwrap(),
        ],
    ));
    marker
}
fn state(home: &Path) -> PathBuf {
    home.join("config/af/provider-disables-v1.json")
}

#[test]
fn disable_status_catalog_setup_and_enable_are_token_free_and_survive_processes() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    let marker = fixture(home);
    let registry = std::fs::read(home.join("config/af/providers.toml")).unwrap();
    let disabled = success(run(
        home,
        &["provider", "disable", "fixture", "--for", "2d"],
    ));
    let text = String::from_utf8_lossy(&disabled.stdout);
    assert!(text.contains("disabled until Unix"));
    assert!(text.contains("af provider enable fixture"));
    let bytes = std::fs::read(state(home)).unwrap();
    let stored: Value = serde_json::from_slice(&bytes).unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("provider-disables-v1.json"),
        &stored,
    );
    let status = success(run(home, &["provider", "status", "--usage", "--json"]));
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("provider-status-v3.json"),
        &status,
    );
    let row = status["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "fixture")
        .unwrap();
    assert_eq!(row["auth"], "not_probed");
    assert_eq!(row["usability"], "unusable");
    assert_eq!(row["disabled_until"], stored["providers"][0]["expires_at"]);
    assert_eq!(row["enable_command"], "af provider enable fixture");
    let catalog = run(home, &["provider", "models", "fixture"]);
    assert!(!catalog.status.success());
    assert!(String::from_utf8_lossy(&catalog.stderr).contains("af provider enable fixture"));
    let setup = run(
        home,
        &[
            "provider",
            "setup",
            "fixture",
            "--kind",
            "claude",
            "--auth-dir",
            home.join("auth").to_str().unwrap(),
        ],
    );
    assert!(!setup.status.success());
    assert!(String::from_utf8_lossy(&setup.stderr).contains("disabled until"));
    assert!(!marker.exists(), "disabled harness spawned");
    assert_eq!(
        std::fs::read(home.join("config/af/providers.toml")).unwrap(),
        registry
    );
    success(run(home, &["provider", "enable", "fixture"]));
    success(run(home, &["provider", "enable", "fixture"]));
    assert!(!marker.exists(), "enable probed a provider");
    success(run(home, &["provider", "status", "--json"]));
    assert!(
        marker.exists(),
        "enabled provider did not become available immediately"
    );
}

#[test]
fn invalid_duration_and_provider_do_not_publish_or_spawn_and_expired_state_is_inert() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    let marker = fixture(home);
    for args in [
        vec!["provider", "disable", "fixture", "--for", "0h"],
        vec![
            "provider",
            "disable",
            "fixture",
            "--for",
            "18446744073709551615d",
        ],
        vec!["provider", "disable", "unknown", "--for", "2d"],
        vec!["provider", "enable", "unknown"],
        vec!["provider", "disable", "claude-ambient", "--for", "2d"],
    ] {
        assert!(!run(home, &args).status.success());
        assert!(!state(home).exists());
        assert!(!marker.exists());
    }
    success(run(
        home,
        &["provider", "disable", "fixture", "--for", "2d"],
    ));
    let mut stored: Value = serde_json::from_slice(&std::fs::read(state(home)).unwrap()).unwrap();
    stored["providers"][0]["expires_at"] = json!({"unix_seconds": 1, "nanoseconds": 0});
    std::fs::write(state(home), serde_json::to_vec(&stored).unwrap()).unwrap();
    let output = success(run(home, &["provider", "status", "--json"]));
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["providers"][0]["disabled_until"], Value::Null);
    assert!(
        marker.exists(),
        "expiry did not take effect on a fresh process"
    );
}

#[test]
fn malformed_state_refuses_catalog_and_never_calls_harness() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    let marker = fixture(home);
    std::fs::write(state(home), "{}").unwrap();
    std::fs::set_permissions(state(home), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        !run(home, &["provider", "enable", "fixture"])
            .status
            .success()
    );
    assert!(
        !run(home, &["provider", "models", "fixture"])
            .status
            .success()
    );
    let output = success(run(home, &["provider", "status", "--json"]));
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        status["providers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["usability"] == "unusable")
    );
    assert!(!marker.exists());
}
