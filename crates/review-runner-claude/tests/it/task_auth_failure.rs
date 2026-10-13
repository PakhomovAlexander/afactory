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
