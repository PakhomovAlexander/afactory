//! The adapter against a scripted fake claude, emitting envelope shapes captured from the
//! real CLI (2.1.234, 2026-08-18) — including two failure envelopes that arrived for real
//! during the capture session: a hijacked-auth "Credit balance is too low" and a scrubbed-env
//! "Not logged in". No model, no network, no spend.

use std::path::{Path, PathBuf};
use std::time::Duration;

use review_config::lock::{Lockfile, Registry};
use review_core::{Arg, Command};
use review_runner::{ReviewerAdapter, RunnerError};
use review_store::Cas;

const ANSWER: &str = r#"{"verdict":"request-changes","summary":null,"findings":[
    {"severity":"major","file":"src/main.rs","line":1,"title":"Unbounded loop",
     "body":"spins forever","fix":"bound it","confidence":0.9}
],"benchmark_demands":[],"disputes":[]}"#;

#[test]
fn adapter_security_flags_follow_allowed_package_model_flags() {
    let runner = Command::new(
        "claude",
        vec![
            Arg::literal("--model"),
            Arg::literal("opus"),
            Arg::literal("--effort"),
            Arg::literal("high"),
        ],
    );
    let args = review_runner_claude::smoke_command(&runner)
        .unwrap()
        .resolve()
        .unwrap();
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
        let error =
            review_runner_claude::smoke_command(&Command::new("claude", arguments)).unwrap_err();
        assert!(error.contains("packages may set only"), "{error}");
    }
}

fn success_envelope(result: &str) -> String {
    serde_json::json!({
        "type": "result", "subtype": "success", "is_error": false,
        "result": result, "total_cost_usd": 0.42, "num_turns": 3,
        "usage": {
            "input_tokens": 1804, "output_tokens": 5233,
            "cache_read_input_tokens": 951_000, "cache_creation_input_tokens": 42_000
        }
    })
    .to_string()
}

fn stub(dir: &Path, envelope: &str, code: i32) -> PathBuf {
    let path = dir.join("claude");
    std::fs::write(
        &path,
        format!("#!/bin/sh\ncat <<'ENVELOPE'\n{envelope}\nENVELOPE\nexit {code}\n"),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn package(dir: &Path, stub_path: &Path) -> review_config::lock::ResolvedReviewer {
    let registry_root = dir.join("registry");
    let package = registry_root.join("tester");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("reviewer.toml"),
        format!(
            "name = \"tester\"\nversion = \"1.0.0\"\nsubjects = [\"whole-tree\"]\n\n[runner]\nprogram = \"{}\"\n\
             args = [{{ value = \"--model\" }}, {{ value = \"opus\" }}]\n",
            stub_path.display()
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

fn adapter_for(
    dir: &Path,
    envelope: &str,
    code: i32,
) -> (review_runner_claude::ClaudeAdapter, Cas, PathBuf) {
    let stub_path = stub(dir, envelope, code);
    let package = package(dir, &stub_path);
    let adapter =
        review_runner_claude::ClaudeAdapter::from_package(&package, Duration::from_secs(10))
            .unwrap();
    let cas = Cas::open(dir.join("cas")).unwrap();
    let sandbox = dir.join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    (adapter, cas, sandbox)
}

/// Success: the reviewer JSON from `result`, the cost from input+output tokens — and *not*
/// from the million cached-read tokens an agentic session accrues. The mapping is the test.
#[test]
fn a_success_envelope_yields_the_answer_and_uncached_cost() {
    let dir = tempfile::tempdir().unwrap();
    let (adapter, cas, sandbox) = adapter_for(dir.path(), &success_envelope(ANSWER), 0);
    assert_eq!(
        adapter.credential_mode(),
        review_runner::BrokerCredentialModeV1::TrustedUnsafe
    );

    let receipt = adapter
        .invoke_receipted(&cas, &sandbox, &Default::default())
        .unwrap();
    let returned = receipt.returned;
    assert_eq!(
        returned.cost_tokens,
        1804 + 42_000 + 5233,
        "cache reads are excluded but cache creation is chargeable"
    );
    assert_eq!(receipt.usage.input_tokens, Some(1804));
    assert_eq!(receipt.usage.cache_read_tokens, Some(951_000));
    assert_eq!(receipt.usage.cache_write_tokens, Some(42_000));
    assert_eq!(receipt.usage.output_tokens, Some(5233));
    assert_eq!(receipt.usage.chargeable_tokens, returned.cost_tokens);
    assert_eq!(returned.output.findings.len(), 1);
    assert_eq!(returned.output.findings[0].title, "Unbounded loop");
    assert!(cas.contains(&returned.raw_artifact));
}

/// Captured for real: "Not logged in" with zero usage. Nothing was spent, so the kernel must
/// release, so this is Unavailable.
#[test]
fn a_zero_usage_error_is_unavailable() {
    let envelope = serde_json::json!({
        "type": "result", "subtype": "success", "is_error": true,
        "result": "Not logged in · Please run /login", "total_cost_usd": 0,
        "usage": {"input_tokens": 0, "output_tokens": 0}
    })
    .to_string();
    let dir = tempfile::tempdir().unwrap();
    let (adapter, cas, sandbox) = adapter_for(dir.path(), &envelope, 1);

    let error = adapter
        .invoke(&cas, &sandbox, &Default::default())
        .unwrap_err();
    let RunnerError::Unavailable(message) = &error else {
        panic!("expected Unavailable, got {error:?}");
    };
    assert!(message.contains("Not logged in"), "{message}");
}

/// An error after usage was reported spent real tokens: Failed, and the kernel charges.
#[test]
fn an_error_with_usage_is_failed() {
    let envelope = serde_json::json!({
        "type": "result", "is_error": true,
        "result": "API error after three turns", "num_turns": 3,
        "usage": {"input_tokens": 2000, "output_tokens": 900}
    })
    .to_string();
    let dir = tempfile::tempdir().unwrap();
    let (adapter, cas, sandbox) = adapter_for(dir.path(), &envelope, 1);

    assert!(matches!(
        adapter
            .invoke(&cas, &sandbox, &Default::default())
            .unwrap_err(),
        RunnerError::Failed { .. }
    ));
}

/// A clean exit whose result is prose is malformed — typed, raw kept — never an empty review.
#[test]
fn a_prose_result_is_malformed() {
    let dir = tempfile::tempdir().unwrap();
    let (adapter, cas, sandbox) =
        adapter_for(dir.path(), &success_envelope("Looks good to me!"), 0);
    assert!(matches!(
        adapter
            .invoke(&cas, &sandbox, &Default::default())
            .unwrap_err(),
        RunnerError::MalformedOutput { .. }
    ));
}

/// The first live run's exact failure shape: one narrative sentence, then a fenced result.
/// The last fenced block wins; the prose around it is tolerated, ambiguity is not.
#[test]
fn prose_followed_by_a_fenced_result_parses() {
    let mixed = format!(
        "I've read the full workspace and executed two scratch programs to verify.\n\n         ```json\n{ANSWER}\n```"
    );
    let dir = tempfile::tempdir().unwrap();
    let (adapter, cas, sandbox) = adapter_for(dir.path(), &success_envelope(&mixed), 0);

    let returned = adapter.invoke(&cas, &sandbox, &Default::default()).unwrap();
    assert_eq!(returned.output.findings.len(), 1);
}

/// ...and a malformed answer's error names the stored raw envelope, so "what did it actually
/// say" is one CAS lookup, not an archaeology dig.
#[test]
fn a_malformed_error_names_the_raw_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let (adapter, cas, sandbox) =
        adapter_for(dir.path(), &success_envelope("Looks good to me!"), 0);
    let error = adapter
        .invoke(&cas, &sandbox, &Default::default())
        .unwrap_err();
    let RunnerError::MalformedOutput { raw_artifact, why } = &error else {
        panic!("expected MalformedOutput, got {error:?}");
    };
    assert!(raw_artifact.starts_with("sha256:"), "{raw_artifact}");
    assert!(!why.is_empty());
}

/// A fenced answer is unwrapped, same rule as codex.
#[test]
fn a_fenced_result_is_unwrapped() {
    let fenced = format!("```json\n{ANSWER}\n```");
    let dir = tempfile::tempdir().unwrap();
    let (adapter, cas, sandbox) = adapter_for(dir.path(), &success_envelope(&fenced), 0);
    assert_eq!(
        adapter
            .invoke(&cas, &sandbox, &Default::default())
            .unwrap()
            .output
            .findings
            .len(),
        1
    );
}

/// Garbage on stdout with a failed exit is Unavailable — no envelope means no evidence
/// anything was spent.
#[test]
fn an_unparseable_stream_on_failure_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let (adapter, cas, sandbox) = adapter_for(dir.path(), "segfault haiku", 1);
    assert!(matches!(
        adapter
            .invoke(&cas, &sandbox, &Default::default())
            .unwrap_err(),
        RunnerError::Unavailable(_)
    ));
}

/// The adapter refuses a package that names anything but claude.
#[test]
fn a_package_naming_another_runner_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let stub_path = stub(dir.path(), "{}", 0);
    let renamed = dir.path().join("codex");
    std::fs::rename(&stub_path, &renamed).unwrap();
    let package = package(dir.path(), &renamed);

    let error = review_runner_claude::ClaudeAdapter::from_package(&package, Duration::from_secs(1))
        .map(|_| ())
        .unwrap_err();
    assert!(error.contains("drives claude"), "{error}");
}

/// `render_input` is the exact stdin the adapter writes: same bytes, same manifest, no spawn.
#[test]
fn rendered_input_is_exactly_what_the_stub_receives() {
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
    let package = package(dir.path(), &stub_path);
    let adapter =
        review_runner_claude::ClaudeAdapter::from_package(&package, Duration::from_secs(10))
            .unwrap()
            .with_focus("the parser");
    let inputs = review_runner::ReviewerInputs {
        result_contract: review_core::ReviewerResultContract::V2,
        finding_identity_policy: Some(review_core::CANONICAL_FINDING_IDENTITY_POLICY.into()),
        refused_attempts: vec!["previous answer was not JSON".into()],
        ..Default::default()
    };

    let rendered = adapter
        .render_input(&inputs)
        .unwrap()
        .expect("claude renders");
    assert_eq!(rendered.transport, review_runner::InputTransport::Prompt);
    assert_eq!(
        rendered.manifest.rendered_bytes,
        rendered.bytes.len() as u64
    );
    assert!(!dump.exists(), "rendering must not spawn the model");

    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let sandbox = dir.path().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let _ = adapter.invoke(&cas, &sandbox, &inputs);
    assert_eq!(std::fs::read(&dump).unwrap(), rendered.bytes);
}
