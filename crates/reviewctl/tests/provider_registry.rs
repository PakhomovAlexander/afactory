//! Provider registry bootstrap is owned by the CLI: a fresh installation should not require
//! hand-authoring the machine-local TOML before an explicit Worker binding can be admitted.

use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};

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
