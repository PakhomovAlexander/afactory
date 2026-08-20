//! The adapter against a scripted fake claude, emitting envelope shapes captured from the
//! real CLI (2.1.234, 2026-08-18) — including two failure envelopes that arrived for real
//! during the capture session: a hijacked-auth "Credit balance is too low" and a scrubbed-env
//! "Not logged in". No model, no network, no spend.

use std::path::{Path, PathBuf};
use std::time::Duration;

use review_config::lock::{Lockfile, Registry};
use review_runner::{ReviewerAdapter, RunnerError};
use review_store::Cas;

const ANSWER: &str = r#"{"verdict":"request-changes","summary":null,"findings":[
    {"severity":"major","file":"src/main.rs","line":1,"title":"Unbounded loop",
     "body":"spins forever","fix":"bound it","confidence":0.9}
],"benchmark_demands":[],"disputes":[]}"#;

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
    #[cfg(unix)]
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
            "name = \"tester\"\nversion = \"1.0.0\"\n\n[runner]\nprogram = \"{}\"\n\
             args = [{{ value = \"--model\" }}, {{ value = \"opus\" }}]\n",
            stub_path.display()
        ),
    )
    .unwrap();
    std::fs::write(package.join("reviewer.md"), "You are a test reviewer.\n").unwrap();
    let registry = Registry::new([registry_root]);
    let mut lockfile = Lockfile::empty();
    lockfile.reviewers.insert(
        "tester".to_string(),
        Lockfile::pin("tester", &registry).unwrap(),
    );
    lockfile.resolve("tester", &registry).unwrap()
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

    let returned = adapter.invoke(&cas, &sandbox, &Default::default()).unwrap();
    assert_eq!(
        returned.cost_tokens,
        1804 + 42_000 + 5233,
        "cache reads are excluded but cache creation is chargeable"
    );
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
        RunnerError::MalformedOutput(_)
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
    let RunnerError::MalformedOutput(message) = &error else {
        panic!("expected MalformedOutput, got {error:?}");
    };
    assert!(message.contains("stored as sha256:"), "{message}");
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
