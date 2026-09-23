//! The Claude adapter's package surface, without a model: the adapter-owned security flags that
//! follow package model flags, the package arguments it refuses, and the exact input
//! `af review render` shows. No model, no network, no spend.

use std::path::Path;

use review_config::lock::{Lockfile, Registry};
use review_core::{Arg, Command};
use review_runner::ReviewerAdapter;
use review_runner_claude::task::ClaudeTaskAdapter;

/// A locked package whose manifest names `program` with `args` (TOML array items).
fn package(dir: &Path, program: &str, args: &str) -> review_config::lock::ResolvedReviewer {
    let registry_root = dir.join("registry");
    let package = registry_root.join("tester");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("reviewer.toml"),
        format!(
            "name = \"tester\"\nversion = \"1.0.0\"\nsubjects = [\"whole-tree\"]\n\n[runner]\nprogram = \"{program}\"\nargs = [{args}]\n"
        ),
    )
    .unwrap();
    std::fs::write(package.join("reviewer.md"), "You are a test reviewer.\n").unwrap();
    let registry = Registry::new(registry_root);
    let mut lockfile = Lockfile::empty();
    lockfile.workers.insert(
        "tester".to_string(),
        Lockfile::pin("tester", &registry).unwrap(),
    );
    lockfile
        .resolve_for_subject("tester", &registry, review_core::SubjectKind::WholeTree)
        .unwrap()
}

#[test]
fn adapter_security_flags_follow_allowed_package_model_flags() {
    let dir = tempfile::tempdir().unwrap();
    let package = package(
        dir.path(),
        "claude",
        "{ value = \"--model\" }, { value = \"opus\" }, { value = \"--effort\" }, { value = \"high\" }",
    );
    let args = review_runner_claude::ClaudeAdapter::from_package(&package)
        .unwrap()
        .attempt_command(&Default::default())
        .resolve()
        .unwrap();
    assert!(args.starts_with(&[
        "-p".into(),
        "--output-format".into(),
        "json".into(),
        "--model".into(),
        "opus".into(),
        "--effort".into(),
        "high".into(),
    ]));
    assert!(args.ends_with(&[
        "--safe-mode".into(),
        "--restricted".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--strict-mcp-config".into(),
        "--tools".into(),
        "Read,Glob,Grep".into(),
        "--allowedTools".into(),
        "Read,Glob,Grep".into(),
    ]));
}

#[test]
fn package_cannot_add_filesystem_settings_or_mcp_authority() {
    for arguments in [
        vec![Arg::literal("--add-dir"), Arg::literal("/")],
        vec![
            Arg::literal("--settings"),
            Arg::literal("project-settings.json"),
        ],
        vec![
            Arg::literal("--mcp-config"),
            Arg::literal("project-mcp.json"),
        ],
    ] {
        let Err(error) = ClaudeTaskAdapter::new(&Command::new("claude", arguments)) else {
            panic!("a package argument that widens authority was accepted");
        };
        assert!(error.contains("packages may set only"), "{error}");
    }
}

/// The adapter refuses a package that names anything but claude.
#[test]
fn a_package_naming_another_runner_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let package = package(dir.path(), "/opt/bin/codex", "");

    let error = review_runner_claude::ClaudeAdapter::from_package(&package)
        .map(|_| ())
        .unwrap_err();
    assert!(error.contains("drives claude"), "{error}");
}

/// `render_input` is the prompt composition the Task host sends: the package instructions with
/// the focus narrowing, then the labelled inputs. Rendering spawns nothing.
#[test]
fn rendered_input_is_the_composed_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("prompt-dump");
    let stub_path = dir.path().join("claude");
    std::fs::write(
        &stub_path,
        format!("#!/bin/sh\ncat > \"{}\"\nexit 1\n", dump.display()),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let package = package(
        dir.path(),
        stub_path.to_str().unwrap(),
        "{ value = \"--model\" }, { value = \"opus\" }",
    );
    let adapter = review_runner_claude::ClaudeAdapter::from_package(&package)
        .unwrap()
        .with_focus("the parser");
    let inputs = review_runner::ReviewerInputs {
        result_contract: review_core::ReviewerResultContract::V2,
        refused_attempts: vec!["previous answer was not JSON".into()],
        ..Default::default()
    };

    let rendered = adapter.render_input(&inputs).unwrap();
    assert_eq!(rendered.transport, review_runner::InputTransport::Prompt);
    assert_eq!(
        rendered.manifest.rendered_bytes,
        rendered.bytes.len() as u64
    );
    let (prompt, manifest) = review_runner::compose_model_prompt(
        "You are a test reviewer.\n\n\n## Focus for this run\n\nthe parser",
        &inputs,
    )
    .unwrap();
    assert_eq!(rendered.bytes, prompt.into_bytes());
    assert_eq!(rendered.manifest, manifest);
    assert!(!dump.exists(), "rendering must not spawn the model");
}
