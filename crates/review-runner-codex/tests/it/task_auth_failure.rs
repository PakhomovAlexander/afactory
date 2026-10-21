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
    // The raw-output rule: the Store keeps the turn line and the `-o` final message, never the
    // agent message's stream line.
    let kept = output.split_once('\n').unwrap().1;
    assert_eq!(
        cas.get(&returned.raw_artifact_ids[0]).unwrap(),
        kept.as_bytes()
    );
    assert_eq!(
        cas.get(returned.raw_artifact_ids.last().unwrap()).unwrap(),
        message.as_bytes()
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
    let kept = output.split_once('\n').unwrap().1;
    assert_eq!(
        cas.get(&returned.raw_artifact_ids[0]).unwrap(),
        kept.as_bytes()
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
            "Codex Worker failed: Provider model at capacity (capacity); it exited 1 without a parsable result and wrote no stderr",
            NativeFailureKind::Capacity,
        ),
        (
            "something unexpected happened",
            "Codex Worker failed: Provider returned an unclassified failure (unknown); it exited 1 without a parsable result and wrote no stderr",
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

/// Issue #259: a codex-cli whose `exec` refuses one of af's options exits 2 with clap's usage on
/// stderr and frames no event. The Attempt diagnostic carries the exit, the CLI's own line and
/// the `unsupported_invocation` classification with the upgrade.
#[test]
fn a_cli_that_refuses_afs_arguments_is_an_unsupported_invocation_with_its_own_line() {
    use review_runner::native_failure::NativeFailureKind;
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let script = temp.path().join("provider");
    std::fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' \"error: unexpected argument '--ephemeral' found\" '' \"  tip: to pass '--ephemeral' as a value, use '-- --ephemeral'\" '' 'Usage: codex exec [OPTIONS] [PROMPT]' '' \"For more information, try '--help'.\" >&2\nexit 2\n",
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
    assert_eq!(
        returned.native_failure,
        Some(NativeFailureKind::UnsupportedInvocation)
    );
    assert!(
        error.starts_with(
            "Codex Worker failed: the installed codex CLI does not accept af's invocation (unsupported_invocation); it exited 2: error: unexpected argument '--ephemeral' found"
        ),
        "{error}"
    );
    assert!(
        error.ends_with("; fix: upgrade it with `npm install -g @openai/codex@latest`, then rerun `af provider status`"),
        "{error}"
    );
    assert!(!error.contains("ExitStatus"), "{error}");
    assert!(returned.usage.is_none(), "no usage was reported");
}

/// Issue #259, round 5: a CLI argument error on stderr outranks every native classification when
/// there is no parsable result: beside an authentication hint, and after an unrelated framed
/// event. Each Attempt is `unsupported_invocation` with its exit and the bounded excerpt, never a
/// credential failure or a generic one; the auth text keeps the raw streams withheld.
#[test]
fn an_argument_error_outranks_an_auth_hint_and_framed_events() {
    use review_runner::native_failure::NativeFailureKind;
    for (stdout, stderr, excerpt, auth) in [
        (
            "",
            "error: unexpected argument '--ephemeral' found\nAuthentication required\n",
            "error: unexpected argument '--ephemeral' found | Authentication required",
            true,
        ),
        (
            "{\"type\":\"thread.started\",\"thread_id\":\"fixture\"}\n",
            "error: unexpected argument '--ephemeral' found\n",
            "error: unexpected argument '--ephemeral' found",
            false,
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        std::fs::write(temp.path().join("stdout"), stdout).unwrap();
        std::fs::write(temp.path().join("stderr"), stderr).unwrap();
        let script = temp.path().join("provider");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\ncat '{0}/stdout'\ncat '{0}/stderr' >&2\nexit 2\n",
                temp.path().display()
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
        assert_eq!(
            returned.native_failure,
            Some(NativeFailureKind::UnsupportedInvocation),
            "{stdout}{stderr}"
        );
        let error = returned.message.unwrap_err();
        assert_eq!(
            error,
            format!(
                "Codex Worker failed: the installed codex CLI does not accept af's invocation (unsupported_invocation); it exited 2: {excerpt}; fix: upgrade it with `npm install -g @openai/codex@latest`, then rerun `af provider status`"
            )
        );
        assert!(returned.usage.is_none(), "no usage was reported");
        if auth {
            for id in &returned.raw_artifact_ids {
                let kept = String::from_utf8(cas.get(id).unwrap()).unwrap();
                assert!(!kept.contains("Authentication required"), "{kept}");
            }
        }
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
    assert_eq!(
        returned.message.unwrap_err(),
        "Codex Worker failed: Provider network request failed (network); it exited 1 without a parsable result; stderr: network failed: proxy refused connection"
    );
}

/// Issue #259, round 3: the only parsable result is the `-o` final message of a process that
/// exited 0 without a failure event. A missing final message at exit 0, stdout holding only
/// unrelated framed events, and a non-zero exit after `turn.failed` each keep the exit, the
/// bounded stderr excerpt and the classification.
#[test]
fn every_exit_without_a_final_message_keeps_its_exit_and_stderr_excerpt() {
    use review_runner::native_failure::NativeFailureKind;
    let started = r#"{"type":"thread.started","thread_id":"fixture"}"#;
    let completed = r#"{"type":"turn.completed","usage":{"input_tokens":3,"output_tokens":2}}"#;
    let failed = r#"{"type":"turn.failed","error":{"message":"Selected model is at capacity."}}"#;
    for (stdout, stderr, exit, kind, expected) in [
        (
            format!("{started}\n{completed}"),
            "error: cannot write last message",
            0,
            None,
            "Codex Worker returned no final message; it exited 0 without a parsable result; stderr: error: cannot write last message",
        ),
        (
            started.to_string(),
            "error: cannot write last message",
            1,
            None,
            "Codex Worker exited 1 without a parsable result; stderr: error: cannot write last message",
        ),
        (
            started.to_string(),
            "network failed: proxy refused connection",
            1,
            Some(NativeFailureKind::Network),
            "Codex Worker failed: Provider network request failed (network); it exited 1 without a parsable result; stderr: network failed: proxy refused connection",
        ),
        (
            format!("{started}\n{failed}"),
            "error: cannot write last message",
            1,
            Some(NativeFailureKind::Capacity),
            "Codex Worker failed: Provider model at capacity (capacity); it exited 1 without a parsable result; stderr: error: cannot write last message",
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        std::fs::write(temp.path().join("stdout"), format!("{stdout}\n")).unwrap();
        std::fs::write(temp.path().join("stderr"), format!("{stderr}\n")).unwrap();
        let script = temp.path().join("provider");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\ncat '{0}/stdout'\ncat '{0}/stderr' >&2\nexit {exit}\n",
                temp.path().display()
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
        assert_eq!(returned.message.unwrap_err(), expected);
        assert_eq!(returned.native_failure, kind, "{expected}");
    }
}

/// Issue #259, round 6: without a parsable result a CLI argument error on stderr decides the
/// classification whatever the exit status: exit 0 with no final message, exit 2 after a framed
/// event, and exit 1 beside an authentication hint are each `unsupported_invocation` with the exit
/// and the bounded excerpt. A final message is a success whatever stderr warns about, even a
/// warning that names an unknown option.
#[test]
fn an_argument_error_without_a_result_is_an_unsupported_invocation_whatever_the_exit() {
    use review_runner::native_failure::NativeFailureKind;
    let started = "{\"type\":\"thread.started\",\"thread_id\":\"fixture\"}\n";
    let completed =
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}\n";
    let refusal = "error: unexpected argument '--ephemeral' found\n";
    for (stdout, stderr, exit, final_message, refused) in [
        (
            "",
            refusal.to_string(),
            0,
            false,
            Some("error: unexpected argument '--ephemeral' found"),
        ),
        (
            started,
            refusal.to_string(),
            2,
            false,
            Some("error: unexpected argument '--ephemeral' found"),
        ),
        (
            "",
            format!("Authentication required\n{refusal}"),
            1,
            false,
            Some("Authentication required | error: unexpected argument '--ephemeral' found"),
        ),
        (
            completed,
            "warning: the update check was skipped\n".to_string(),
            0,
            true,
            None,
        ),
        (
            completed,
            "warning: unknown option 'theme' in config.toml was ignored\n".to_string(),
            0,
            true,
            None,
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        std::fs::write(temp.path().join("stdout"), stdout).unwrap();
        std::fs::write(temp.path().join("stderr"), &stderr).unwrap();
        let reply = if final_message {
            "out=; prev=; for arg in \"$@\"; do [ \"$prev\" = -o ] && out=$arg; prev=$arg; done\nprintf done >\"$out\"\n"
        } else {
            ""
        };
        let script = temp.path().join("provider");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\n{reply}cat '{0}/stdout'\ncat '{0}/stderr' >&2\nexit {exit}\n",
                temp.path().display()
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
        let Some(excerpt) = refused else {
            assert_eq!(returned.native_failure, None, "{stderr}");
            assert_eq!(returned.message.unwrap(), b"done", "{stderr}");
            continue;
        };
        assert_eq!(
            returned.native_failure,
            Some(NativeFailureKind::UnsupportedInvocation),
            "{stdout}{stderr}"
        );
        assert_eq!(
            returned.message.unwrap_err(),
            format!(
                "Codex Worker failed: the installed codex CLI does not accept af's invocation (unsupported_invocation); it exited {exit}: {excerpt}; fix: upgrade it with `npm install -g @openai/codex@latest`, then rerun `af provider status`"
            )
        );
        assert!(returned.usage.is_none(), "no usage was reported");
        for id in &returned.raw_artifact_ids {
            let kept = String::from_utf8(cas.get(id).unwrap()).unwrap();
            assert!(!kept.contains("Authentication required"), "{kept}");
        }
    }
}
