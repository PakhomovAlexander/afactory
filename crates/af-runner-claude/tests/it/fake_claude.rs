//! Native Task adapter security flags and model admission; no model or network.
use af_contracts::{Arg, Command};
use af_runner_claude::task::ClaudeTaskAdapter;

/// The command a Task Attempt runs: package model flags first, then the adapter-owned security
/// flags, which therefore win; and no session flag, because no session is assigned, captured or
/// resumed (ADR-0145).
#[test]
fn adapter_security_flags_follow_allowed_package_model_flags() {
    use std::os::unix::fs::PermissionsExt;

    use af_runner::task::{WorkerAccess, WorkerModelAdapter};

    let dir = tempfile::tempdir().unwrap();
    let cas = af_store::Cas::open(dir.path().join("cas")).unwrap();
    let program = dir.path().join("fake-claude");
    std::fs::write(
        &program,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' \"$@\" >&2\nprintf '%s' '{\"is_error\":false,\"result\":\"OK\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = ClaudeTaskAdapter::new(&Command::new(
        program.to_str().unwrap(),
        ["--model", "claude-opus-5-5", "--effort", "high"]
            .into_iter()
            .map(Arg::literal)
            .collect(),
    ))
    .unwrap();
    let returned = adapter.invoke(
        &cas,
        dir.path(),
        b"review".to_vec(),
        std::time::Duration::from_secs(5),
        WorkerAccess::ReadOnly,
        None,
        &[],
    );
    assert_eq!(returned.message.unwrap(), b"OK");
    let args = String::from_utf8(cas.get(&returned.raw_artifact_ids[1]).unwrap()).unwrap();
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(
        args,
        [
            "-p",
            "--output-format",
            "json",
            "--model",
            "claude-opus-5-5",
            "--effort",
            "high",
            "--safe-mode",
            "--restricted",
            "--permission-mode",
            "dontAsk",
            "--strict-mcp-config",
            "--tools",
            "Read,Glob,Grep",
            "--allowedTools",
            "Read,Glob,Grep",
        ]
    );
    for flag in ["--session-id", "--resume", "--fork-session"] {
        assert!(!args.contains(&flag), "{args:?}");
    }
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

/// A reviewer package that names an alias is refused when it is parsed, with the exact ID the
/// alias resolves to and the command that lists the spellings (ADR-0148).
#[test]
fn a_package_naming_an_alias_is_refused_with_its_exact_id() {
    let error = ClaudeTaskAdapter::new(&Command::new(
        "claude",
        ["--model", "opus", "--effort", "high"]
            .into_iter()
            .map(Arg::literal)
            .collect(),
    ))
    .map(|_| ())
    .unwrap_err();
    for part in [
        "af refused the binding of model opus (effort high) for a claude Provider",
        "the exact ID claude-opus-5-5",
        "`af provider models` (token-free)",
    ] {
        assert!(error.contains(part), "{part}: {error}");
    }
}
