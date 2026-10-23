//! Repository-policy planning fixtures: real model Worker contracts, isolated fake Providers,
//! and token-free catalog/identity probes only. No account or business model call is used.

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Providers {
    root: PathBuf,
}

impl Providers {
    pub fn new(root: &Path) -> Self {
        let root = root.join("planning-providers");
        for kind in ["claude", "codex"] {
            crate::fake_models::install(kind, &root.join("bin"), &root.join(kind));
        }
        std::fs::create_dir_all(root.join("home")).unwrap();
        let registry = serde_json::json!({"version": 1, "providers": [
            {"id": "fixture-claude", "kind": "claude", "auth_dir": root.join("claude")},
            {"id": "fixture-codex", "kind": "codex", "auth_dir": root.join("codex")},
        ]});
        std::fs::write(
            root.join("providers.toml"),
            toml::to_string(&registry).unwrap(),
        )
        .unwrap();
        Self { root }
    }

    pub fn configure(&self, command: &mut Command) {
        command
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("USER", "fixture")
            .env("CODEX_HOME", self.root.join("codex"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("AF_PROVIDERS_FILE", self.root.join("providers.toml"));
    }

    pub fn command(&self, repo: &Path) -> Command {
        let mut command = crate::common::af();
        command
            .current_dir(repo)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("AF_CACHE_POLICY_FILE")
            .env_remove("CARGO_TARGET_DIR");
        self.configure(&mut command);
        command
    }

    pub fn assert_no_dispatch(&self) {
        for kind in ["claude", "codex"] {
            let auth = self.root.join(kind);
            assert_eq!(
                crate::fake_models::turns(&auth),
                "",
                "{kind} started a turn"
            );
            for call in crate::fake_models::started(&auth).lines() {
                assert!(
                    matches!(
                        call,
                        "--help"
                            | "--version"
                            | "auth status --json"
                            | "login status"
                            | "app-server"
                            | "-p --input-format stream-json --output-format stream-json --verbose"
                    ),
                    "{kind} dispatched a model call: {call}"
                );
            }
        }
    }
}

/// Rename a copy without changing its model identity, contract, schemas or Attempt bounds.
pub fn local_replacement(repo: &Path, directory: &Path, package: &str, local: &str) -> toml::Value {
    let target = directory.join(local);
    crate::task_cli::copy_tree(&repo.join(".af/task-packages").join(package), &target);
    let manifest = target.join("worker.toml");
    let mut worker: toml::Value =
        toml::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    assert_eq!(worker["runner"]["kind"].as_str(), Some("model"));
    worker["name"] = toml::Value::String(local.into());
    std::fs::write(&manifest, toml::to_string(&worker).unwrap()).unwrap();
    pin(directory, local)
}

pub fn pin(directory: &Path, local: &str) -> toml::Value {
    let target = directory.join(local);
    let worker: toml::Value =
        toml::from_str(&std::fs::read_to_string(target.join("worker.toml")).unwrap()).unwrap();
    toml::Value::try_from(serde_json::json!({
        "version": worker["version"].as_str().unwrap(),
        "digest": review_config::lock::package_digest(local, &target).unwrap(),
        "path": local,
    }))
    .unwrap()
}

/// Explicit bindings for only the selected model packages; catalog defaults stay untouched.
pub fn provider_bindings(
    directory: &Path,
    packages: &toml::map::Map<String, toml::Value>,
) -> toml::Value {
    toml::Value::Table(
        packages
            .iter()
            .filter_map(|(name, pin)| {
                let worker: toml::Value = toml::from_str(
                    &std::fs::read_to_string(
                        directory
                            .join(pin["path"].as_str().unwrap())
                            .join("worker.toml"),
                    )
                    .unwrap(),
                )
                .unwrap();
                if worker["runner"]["kind"].as_str() == Some("model") {
                    Some((
                        name.clone(),
                        toml::Value::String(format!(
                            "fixture-{}",
                            worker["runner"]["provider_kind"].as_str().unwrap()
                        )),
                    ))
                } else {
                    None
                }
            })
            .collect(),
    )
}

pub fn assert_repository_policy(repo: &Path) {
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    assert_eq!(
        catalog["independence"]["distinct_principals"].as_bool(),
        Some(false)
    );
    assert_eq!(
        catalog["independence"]["distinct_models"].as_bool(),
        Some(true)
    );
}

/// The plan must bind the selected copies with their original model and effort identities.
pub fn assert_model_bindings(
    plan: &serde_json::Value,
    directory: &Path,
    bindings: &toml::map::Map<String, toml::Value>,
) {
    for (slot, name) in bindings["slots"].as_table().unwrap() {
        let name = name.as_str().unwrap();
        let pin = &bindings["packages"][name];
        let worker: toml::Value = toml::from_str(
            &std::fs::read_to_string(
                directory
                    .join(pin["path"].as_str().unwrap())
                    .join("worker.toml"),
            )
            .unwrap(),
        )
        .unwrap();
        let bound = &plan["bindings"][slot];
        assert_eq!(
            bound["package_digest"].as_str(),
            pin["digest"].as_str(),
            "{slot}"
        );
        for field in ["kind", "provider_kind", "model", "effort"] {
            assert_eq!(
                bound["execution"][field].as_str(),
                worker["runner"][field].as_str(),
                "{slot}: {field}"
            );
        }
    }
}
