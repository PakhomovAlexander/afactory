use crate::load_safe_wall::LOAD_SAFE_WALL;
use review_core::Command;
use review_runner::task::{WorkerAccess, WorkerModelAdapter};
use review_runner_codex::task::CodexTaskAdapter;
use review_store::Cas;
use std::os::unix::fs::PermissionsExt;

#[test]
fn authentication_failure_survives_missing_usage_without_retaining_challenges() {
    for malformed_usage in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        let mut output = serde_json::json!({
            "type": "turn.failed",
            "error": {"message": "OAuth token has been revoked; access_token=FIXTURE_SECRET https://auth.openai.com/oauth/authorize?code=FIXTURE_CODE"}
        }).to_string();
        if malformed_usage {
            output.push_str("\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":null,\"output_tokens\":7}}");
        }
        let script = temp.path().join("provider");
        std::fs::write(&script, format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{}'\nprintf '%s' 'FIXTURE_STDERR_SECRET' >&2\nexit 1\n",
            output.replace('\'', "'\\''")
        )).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter =
            CodexTaskAdapter::new(&Command::new(script.to_str().unwrap(), vec![])).unwrap();
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
        assert!(error.contains("revoked"), "{error}");
        assert_eq!(returned.usage.is_some(), malformed_usage);
        if malformed_usage {
            assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 7);
            assert!(!returned.usage_observation.unwrap().charge_complete);
            assert!(error.contains("usage"), "{error}");
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
        let output =
            serde_json::json!({"type":"turn.failed","error":{"message":native}}).to_string();
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
        let adapter =
            CodexTaskAdapter::new(&Command::new(script.to_str().unwrap(), vec![])).unwrap();
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
fn plaintext_auth_failure_is_private_even_without_a_json_event() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let script = temp.path().join("provider");
    std::fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' 'Access token expired: FIXTURE_SECRET'\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = CodexTaskAdapter::new(&Command::new(script.to_str().unwrap(), vec![])).unwrap();
    let returned = adapter.invoke(
        &cas,
        temp.path(),
        b"input".to_vec(),
        LOAD_SAFE_WALL,
        WorkerAccess::ReadOnly,
        None,
        &[],
    );
    assert!(returned.message.unwrap_err().contains("(auth_expired)"));
    for id in returned.raw_artifact_ids {
        assert!(
            !String::from_utf8(cas.get(&id).unwrap())
                .unwrap()
                .contains("FIXTURE_SECRET")
        );
    }
}

#[test]
fn network_failure_does_not_publish_device_challenges_from_either_stream() {
    for challenge in [
        "HTTPS://AUTH.OPENAI.COM/CODEX/DEVICE FIXTURE_CODE",
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
            let output =
                serde_json::json!({"type":"turn.failed","error":{"message":message}}).to_string();
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
            let adapter =
                CodexTaskAdapter::new(&Command::new(script.to_str().unwrap(), vec![])).unwrap();
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
fn successful_output_about_device_authentication_is_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let script = temp.path().join("provider");
    let message = "Documentation: https://auth.openai.com/codex/device userCode=FIXTURE_EXAMPLE";
    let output = format!(
        "{}\n{}",
        serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":message}}),
        serde_json::json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}})
    );
    std::fs::write(&script, format!("#!/bin/sh\ncat >/dev/null\nout=; prev=; for arg in \"$@\"; do [ \"$prev\" = -o ] && out=$arg; prev=$arg; done\nprintf '%s' '{}' >\"$out\"\nprintf '%s' '{}'\n", message.replace('\'', "'\\''"), output.replace('\'', "'\\''"))).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = CodexTaskAdapter::new(&Command::new(script.to_str().unwrap(), vec![])).unwrap();
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
fn model_text_about_revoked_credentials_cannot_replace_a_network_failure() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let output = [
        serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":"Reviewed: the OAuth token has been revoked (auth_revoked) branch."}}),
        serde_json::json!({"type":"turn.failed","error":{"message":"stream disconnected: connection refused"}}),
    ]
    .map(|event| event.to_string())
    .join("\n");
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
    let adapter = CodexTaskAdapter::new(&Command::new(script.to_str().unwrap(), vec![])).unwrap();
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

/// Issue #165 (ADR-0143): tool use and then `turn.failed` with `Selected model is at capacity`
/// reports no usage. The Attempt names the classified cause, never the bare exit status, and
/// the raw Provider message stays out of the diagnostic; an unclassified failure event is
/// named as such. Neither invents usage.
#[test]
fn a_failure_event_without_usage_names_its_classified_cause() {
    use review_runner::native_failure::NativeFailureKind;
    for (message, diagnostic, kind) in [
        (
            "Selected model is at capacity. Please try a different model.",
            "Codex Worker failed: Provider model at capacity (capacity); transport: ",
            NativeFailureKind::Capacity,
        ),
        (
            "something unexpected happened",
            "Codex Worker failed: Provider returned an unclassified failure (unknown); transport: ",
            NativeFailureKind::Unknown,
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        let output = [
            serde_json::json!({"type":"thread.started","thread_id":"fixture"}),
            serde_json::json!({"type":"item.completed","item":{"type":"command_execution",
                "command":"ls","aggregated_output":"","exit_code":0,"status":"completed"}}),
            serde_json::json!({"type":"turn.failed","error":{"message":message}}),
        ]
        .map(|event| event.to_string())
        .join("\n");
        let script = temp.path().join("provider");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{}'\nexit 1\n",
                output.replace('\'', "'\\''")
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter =
            CodexTaskAdapter::new(&Command::new(script.to_str().unwrap(), vec![])).unwrap();
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
        assert!(error.starts_with(diagnostic), "{error}");
        assert!(!error.contains(message), "{error}");
        assert!(returned.usage.is_none(), "no usage was reported");
        assert!(returned.usage_observation.is_none());
        assert_eq!(returned.native_failure, Some(kind));
        assert_eq!(
            kind.unknown_usage_cause().as_str(),
            if kind == NativeFailureKind::Capacity {
                "capacity"
            } else {
                "unreported"
            }
        );
    }
}
