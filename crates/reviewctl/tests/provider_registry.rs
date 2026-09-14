//! Provider registry bootstrap is owned by the CLI: a fresh installation should not require
//! hand-authoring the machine-local TOML before an explicit Worker binding can be admitted.

use std::path::Path;
use std::process::{Command, Output};

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
