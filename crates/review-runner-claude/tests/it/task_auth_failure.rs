use crate::load_safe_wall::LOAD_SAFE_WALL;
use review_core::{Arg, Command};
use review_runner::task::{WorkerAccess, WorkerModelAdapter};
use review_runner_claude::task::ClaudeTaskAdapter;
use review_store::Cas;
use std::os::unix::fs::PermissionsExt;

#[test]
fn refresh_contention_survives_missing_usage_without_retaining_challenges() {
    for malformed_usage in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        let mut output = serde_json::json!({
            "type": "result", "is_error": true,
            "errors": ["Failed to refresh OAuth token: another Claude Code process is refreshing it or exited mid-refresh. refresh_token=FIXTURE_SECRET https://claude.ai/oauth/authorize?code=FIXTURE_CODE"],
            "result": "FIXTURE_RESULT_SECRET"
        });
        if malformed_usage {
            output["usage"] = serde_json::json!({"input_tokens": null, "output_tokens": 7});
            output["modelUsage"] = serde_json::json!({});
        }
        let script = temp.path().join("provider");
        std::fs::write(&script, format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{}'\nprintf '%s' 'FIXTURE_STDERR_SECRET' >&2\nexit 1\n",
            output.to_string().replace('\'', "'\\''")
        )).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = ClaudeTaskAdapter::new(&Command::new(
            script.to_str().unwrap(),
            vec![Arg::literal("--model"), Arg::literal("claude-fixture-1")],
        ))
        .unwrap();
        let returned = adapter.invoke(
            &cas,
            temp.path(),
            b"input".to_vec(),
            LOAD_SAFE_WALL,
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        let error = returned.message.unwrap_err();
        assert!(error.contains("refresh contention"), "{error}");
        assert!(!returned.usage_observation.unwrap().charge_complete);
        if malformed_usage {
            assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 7);
            assert!(error.contains("usage"), "{error}");
        } else {
            assert!(returned.usage.is_none());
        }
        let mut evidence = error;
        assert_eq!(returned.raw_artifact_ids.len(), 1);
        for id in returned.raw_artifact_ids {
            evidence.push_str(&String::from_utf8(cas.get(&id).unwrap()).unwrap());
        }
        assert!(!evidence.contains("FIXTURE_"), "{evidence}");
        assert!(!evidence.contains("https://"), "{evidence}");
    }
}

#[test]
fn non_auth_native_failures_keep_their_evidence_and_classification() {
    for (native, expected) in [
        (
            "Failed to refresh OAuth token: connection refused",
            "(network)",
        ),
        ("HTTP 429: rate limit exceeded", "(quota)"),
        ("model_not_found", "(model_unavailable)"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        let output = serde_json::json!({"is_error":true,"result":native}).to_string();
        let script = temp.path().join("provider");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{}'\nexit 1\n",
                output.replace('\'', "'\\''")
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = ClaudeTaskAdapter::new(&Command::new(
            script.to_str().unwrap(),
            vec![Arg::literal("--model"), Arg::literal("claude-fixture-1")],
        ))
        .unwrap();
        let returned = adapter.invoke(
            &cas,
            temp.path(),
            b"input".to_vec(),
            LOAD_SAFE_WALL,
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        let error = returned.message.unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("(auth_"), "{error}");
        assert_eq!(
            cas.get(&returned.raw_artifact_ids[0]).unwrap(),
            output.as_bytes()
        );
    }
}

#[test]
fn successful_output_about_authentication_is_not_classified_as_failure() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let script = temp.path().join("provider");
    let message = "Documentation: token revoked means authentication failed; https://auth.openai.com/codex/device userCode=FIXTURE_EXAMPLE";
    let output = serde_json::json!({"is_error":false,"result":message, "usage":{"input_tokens":1,"output_tokens":1}}).to_string();
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{}'\n",
            output.replace('\'', "'\\''")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = ClaudeTaskAdapter::new(&Command::new(
        script.to_str().unwrap(),
        vec![Arg::literal("--model"), Arg::literal("claude-fixture-1")],
    ))
    .unwrap();
    let returned = adapter.invoke(
        &cas,
        temp.path(),
        b"input".to_vec(),
        LOAD_SAFE_WALL,
        WorkerAccess::ReadOnly,
        None,
        &[],
    );
    assert_eq!(returned.message.unwrap(), message.as_bytes());
    assert_eq!(
        cas.get(&returned.raw_artifact_ids[0]).unwrap(),
        output.as_bytes()
    );
}

#[test]
fn network_failure_does_not_publish_device_challenges_from_either_stream() {
    for challenge in [
        "https://auth.openai.com/codex/device FIXTURE_CODE",
        r#"{"userCode":"FIXTURE_CODE"}"#,
        r#"{"user_code":"FIXTURE_CODE"}"#,
    ] {
        for challenge_on_stdout in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let cas = Cas::open(temp.path().join("cas")).unwrap();
            let message = if challenge_on_stdout {
                format!("network failed: {challenge}")
            } else {
                "network failed".into()
            };
            let output = serde_json::json!({"is_error":true,"result":message}).to_string();
            let stderr = if challenge_on_stdout {
                "FIXTURE_STDERR"
            } else {
                challenge
            };
            let script = temp.path().join("provider");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{}'\nprintf '%s' '{}' >&2\nexit 1\n",
                    output.replace('\'', "'\\''"),
                    stderr.replace('\'', "'\\''")
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let adapter = ClaudeTaskAdapter::new(&Command::new(
                script.to_str().unwrap(),
                vec![Arg::literal("--model"), Arg::literal("claude-fixture-1")],
            ))
            .unwrap();
            let returned = adapter.invoke(
                &cas,
                temp.path(),
                b"input".to_vec(),
                LOAD_SAFE_WALL,
                WorkerAccess::ReadOnly,
                None,
                &[],
            );
            let error = returned.message.unwrap_err();
            assert!(error.contains("(network)"), "{error}");
            assert!(!error.contains("FIXTURE_"), "{error}");
            assert_eq!(returned.raw_artifact_ids.len(), 1);
            assert_eq!(
                cas.get(&returned.raw_artifact_ids[0]).unwrap(),
                b"Provider network request failed (network)"
            );
        }
    }
}

#[test]
fn model_text_about_revoked_credentials_cannot_replace_a_network_failure() {
    let model_text = "Reviewed: the OAuth token has been revoked (auth_revoked) branch.";
    for (output, stderr) in [
        // A failed process whose envelope still carries the model's reply.
        (
            serde_json::json!({"is_error":false,"result":model_text,"usage":{"input_tokens":1,"output_tokens":1}}),
            "connection refused",
        ),
        // Structured native errors outrank the reply text of an error envelope.
        (
            serde_json::json!({"is_error":true,"errors":["connection refused"],"result":model_text}),
            "",
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        let output = output.to_string();
        let script = temp.path().join("provider");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{}'\nprintf '%s' '{}' >&2\nexit 1\n",
                output.replace('\'', "'\\''"),
                stderr
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = ClaudeTaskAdapter::new(&Command::new(
            script.to_str().unwrap(),
            vec![Arg::literal("--model"), Arg::literal("claude-fixture-1")],
        ))
        .unwrap();
        let returned = adapter.invoke(
            &cas,
            temp.path(),
            b"input".to_vec(),
            LOAD_SAFE_WALL,
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        let error = returned.message.unwrap_err();
        assert!(error.contains("(network)"), "{error}");
        assert!(!error.contains("(auth_"), "{error}");
        assert_eq!(
            cas.get(&returned.raw_artifact_ids[0]).unwrap(),
            output.as_bytes()
        );
    }
}

/// Issue #259: Claude Code 2.1.101 refuses `--safe-mode` on its argument parser, exits 1, and
/// prints no envelope. The Attempt diagnostic names the exit, the CLI's own line and the
/// `unsupported_invocation` classification with the upgrade, never a debug-printed exit status;
/// a generic envelope-less failure keeps its exit and a bounded, sanitized stderr excerpt.
#[test]
fn a_cli_that_refuses_afs_arguments_is_an_unsupported_invocation_with_its_own_line() {
    use review_runner::native_failure::NativeFailureKind;
    for (stderr, unsupported) in [
        ("error: unknown option '--safe-mode'\n", true),
        (
            "\x1b[31mfatal\x1b[0m: cannot open the bundled runtime\nsecond line\n",
            false,
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        let said = temp.path().join("stderr");
        std::fs::write(&said, stderr).unwrap();
        let script = temp.path().join("provider");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\ncat '{}' >&2\nexit 1\n",
                said.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = ClaudeTaskAdapter::new(&Command::new(
            script.to_str().unwrap(),
            vec![Arg::literal("--model"), Arg::literal("claude-fixture-1")],
        ))
        .unwrap();
        let returned = adapter.invoke(
            &cas,
            temp.path(),
            b"input".to_vec(),
            LOAD_SAFE_WALL,
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        let error = returned.message.unwrap_err();
        assert!(!error.contains("ExitStatus"), "{error}");
        assert!(returned.usage.is_none(), "no usage was reported");
        if unsupported {
            assert_eq!(
                returned.native_failure,
                Some(NativeFailureKind::UnsupportedInvocation)
            );
            assert_eq!(
                error,
                "Claude Worker failed: the installed claude CLI does not accept af's invocation (unsupported_invocation); it exited 1: error: unknown option '--safe-mode'; fix: upgrade it with `npm install -g @anthropic-ai/claude-code@latest`, then rerun `af provider status`"
            );
        } else {
            assert_eq!(returned.native_failure, None);
            assert_eq!(
                error,
                "Claude Worker exited 1 without a parsable result; stderr: fatal: cannot open the bundled runtime | second line"
            );
        }
    }
}

/// Issue #259, round 5: a CLI argument error outranks an authentication hint beside it. A Worker
/// that prints no envelope and whose stderr holds both `error: unknown option '--safe-mode'` and
/// `Authentication required` is `unsupported_invocation`, with its exit and the bounded excerpt,
/// never blamed on credentials; its raw streams stay as withheld as the auth text made them.
#[test]
fn an_argument_error_outranks_an_authentication_hint() {
    use review_runner::native_failure::NativeFailureKind;
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let script = temp.path().join("provider");
    std::fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' \"error: unknown option '--safe-mode'\" 'Authentication required' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = ClaudeTaskAdapter::new(&Command::new(
        script.to_str().unwrap(),
        vec![Arg::literal("--model"), Arg::literal("claude-fixture-1")],
    ))
    .unwrap();
    let returned = adapter.invoke(
        &cas,
        temp.path(),
        b"input".to_vec(),
        LOAD_SAFE_WALL,
        WorkerAccess::ReadOnly,
        None,
        &[],
    );
    assert_eq!(
        returned.native_failure,
        Some(NativeFailureKind::UnsupportedInvocation)
    );
    let error = returned.message.unwrap_err();
    assert_eq!(
        error,
        "Claude Worker failed: the installed claude CLI does not accept af's invocation (unsupported_invocation); it exited 1: error: unknown option '--safe-mode' | Authentication required; fix: upgrade it with `npm install -g @anthropic-ai/claude-code@latest`, then rerun `af provider status`"
    );
    assert!(!error.contains("auth_missing"), "{error}");
    assert!(returned.usage.is_none(), "no usage was reported");
    for id in &returned.raw_artifact_ids {
        let kept = String::from_utf8(cas.get(id).unwrap()).unwrap();
        assert!(!kept.contains("Authentication required"), "{kept}");
    }
}

/// Issue #259, round 2: a Worker that exits without a parsable result keeps its exit and its own
/// bounded stderr excerpt whatever its failure is classified as, and a line that looks like
/// credential material is withheld whole while the exit and the classification stay.
#[test]
fn a_classified_failure_without_events_keeps_its_exit_and_stderr_excerpt() {
    use review_runner::cli::WITHHELD_LINE;
    use review_runner::native_failure::NativeFailureKind;
    for (stderr, kind, quoted) in [
        (
            "error: connection refused while loading the native runtime",
            NativeFailureKind::Network,
            "error: connection refused while loading the native runtime",
        ),
        (
            "network failed: proxy refused connection",
            NativeFailureKind::Network,
            "network failed: proxy refused connection",
        ),
        (
            "error: unknown option '--safe-mode'; API_KEY=sk-secret",
            NativeFailureKind::UnsupportedInvocation,
            WITHHELD_LINE,
        ),
        (
            "network failed; OPENAI_API_KEY=\"sk-proj-FIXTURE\"",
            NativeFailureKind::Network,
            WITHHELD_LINE,
        ),
        (
            "network failed: 3f9a1c0b7e6d5a4f3e2d1c0b9a8f7e6d5c4b3a2f",
            NativeFailureKind::Network,
            WITHHELD_LINE,
        ),
        (
            "Authorization: Bearer FIXTUREabcdef",
            NativeFailureKind::Unknown,
            WITHHELD_LINE,
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        let said = temp.path().join("stderr");
        std::fs::write(&said, format!("{stderr}\n")).unwrap();
        let script = temp.path().join("provider");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\ncat '{}' >&2\nexit 1\n",
                said.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = ClaudeTaskAdapter::new(&Command::new(
            script.to_str().unwrap(),
            vec![Arg::literal("--model"), Arg::literal("claude-fixture-1")],
        ))
        .unwrap();
        let returned = adapter.invoke(
            &cas,
            temp.path(),
            b"input".to_vec(),
            LOAD_SAFE_WALL,
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        let error = returned.message.unwrap_err();
        assert_eq!(
            returned.native_failure,
            (kind != NativeFailureKind::Unknown).then_some(kind),
            "{error}"
        );
        assert!(error.contains("exited 1"), "{error}");
        assert!(error.contains(quoted), "{error}");
        if kind != NativeFailureKind::Unknown {
            let closed = kind.diagnostic();
            assert!(
                error.contains(&closed[closed.rfind('(').unwrap()..]),
                "{error}"
            );
        }
        for secret in ["sk-", "FIXTURE", "3f9a1c0b"] {
            assert!(!error.contains(secret), "{error}");
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let script = temp.path().join("provider");
    std::fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\necho 'network failed: proxy refused connection' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = ClaudeTaskAdapter::new(&Command::new(
        script.to_str().unwrap(),
        vec![Arg::literal("--model"), Arg::literal("claude-fixture-1")],
    ))
    .unwrap();
    let returned = adapter.invoke(
        &cas,
        temp.path(),
        b"input".to_vec(),
        LOAD_SAFE_WALL,
        WorkerAccess::ReadOnly,
        None,
        &[],
    );
    assert_eq!(
        returned.message.unwrap_err(),
        "Claude Worker failed: Provider network request failed (network); it exited 1 without a parsable result; stderr: network failed: proxy refused connection"
    );
}

/// Issue #259, round 3: an envelope is only a JSON object carrying a boolean `is_error` and the
/// `result`. Any other JSON on stdout, `{}` among it, is no parsable result, so the Worker keeps
/// its exit, its stderr excerpt and its classification.
#[test]
fn json_that_is_not_an_envelope_is_no_parsable_result() {
    use review_runner::native_failure::NativeFailureKind;
    for stdout in [
        "{}",
        "[]",
        "\"done\"",
        r#"{"is_error":"false","result":"OK"}"#,
        r#"{"is_error":false}"#,
        r#"{"result":"OK"}"#,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        let said = temp.path().join("stdout");
        std::fs::write(&said, stdout).unwrap();
        let script = temp.path().join("provider");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\ncat '{}'\necho 'network failed: proxy refused connection' >&2\nexit 1\n",
                said.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = ClaudeTaskAdapter::new(&Command::new(
            script.to_str().unwrap(),
            vec![Arg::literal("--model"), Arg::literal("claude-fixture-1")],
        ))
        .unwrap();
        let returned = adapter.invoke(
            &cas,
            temp.path(),
            b"input".to_vec(),
            LOAD_SAFE_WALL,
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        assert_eq!(
            returned.native_failure,
            Some(NativeFailureKind::Network),
            "{stdout}"
        );
        assert_eq!(
            returned.message.unwrap_err(),
            "Claude Worker failed: Provider network request failed (network); it exited 1 without a parsable result; stderr: network failed: proxy refused connection",
            "{stdout}"
        );
    }
}
