use review_core::Command;
use review_runner::task::{WorkerContract, WorkerModelAdapter};
use review_runner_codex::task::CodexTaskAdapter;
use review_store::Cas;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

#[test]
fn native_task_adapter_declares_trusted_unsafe_credentials() {
    let adapter = CodexTaskAdapter::new(&Command::new("codex", vec![])).unwrap();
    assert_eq!(
        adapter.credential_mode(),
        review_core::BrokerCredentialModeV1::TrustedUnsafe
    );
}

#[test]
fn timeout_and_cas_failure_preserve_reported_overrun_without_admitting_the_message() {
    for timed_out in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let cas_path = temp.path().join("cas");
        let cas = Cas::open(&cas_path).unwrap();
        let output = format!(
            "{}\n{}\n",
            serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":"OK"}}),
            serde_json::json!({"type":"turn.completed","usage":{"input_tokens":u64::MAX,"output_tokens":20}})
        );
        let script = temp.path().join("provider");
        let quoted = output.replace('\'', "'\\''");
        std::fs::write(&script, format!(
            "#!/bin/sh\nif [ \"$1\" = --fixture-ready ]; then exit 0; fi\ncat >/dev/null\nprintf '%s' '{quoted}'\nprintf '%s' 'diagnostic' >&2\n{}\n",
            if timed_out { "sleep 10" } else { "exit 0" }
        )).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        // macOS may delay the first execution of a newly written script before its first
        // instruction. Prepare that executable without output; the measured provider invocation
        // below still gets the original 500 ms and must retain all already reported usage.
        let mut ready = std::process::Command::new(&script);
        ready.arg("--fixture-ready").current_dir(temp.path());
        assert!(
            review_runner::run_supervised(&mut ready, None, Duration::from_secs(5))
                .unwrap()
                .status
                .success()
        );
        if !timed_out {
            // Deterministically refuse CAS writes without relying on effective-user permissions.
            std::fs::remove_dir_all(&cas_path).unwrap();
            std::fs::write(&cas_path, b"not a directory").unwrap();
        }
        let adapter =
            CodexTaskAdapter::new(&Command::new(script.to_str().unwrap(), vec![])).unwrap();
        let returned = adapter.invoke(
            &cas,
            temp.path(),
            b"input".to_vec(),
            if timed_out {
                Duration::from_millis(500)
            } else {
                Duration::from_secs(5)
            },
            false,
        );
        assert!(
            returned.message.is_err(),
            "a printed message cannot overcome transport failure"
        );
        assert!(
            returned.usage.is_some(),
            "timed_out={timed_out}; message={:?}; captured={:?}",
            returned.message,
            returned
                .raw_artifact_ids
                .iter()
                .map(|id| cas.get(id))
                .collect::<Vec<_>>(),
        );
        assert_eq!(returned.usage.unwrap().chargeable_tokens, u64::MAX);
        if timed_out {
            assert_eq!(returned.raw_artifact_ids.len(), 2);
            assert_eq!(
                cas.get(&returned.raw_artifact_ids[0]).unwrap(),
                output.as_bytes()
            );
            assert_eq!(
                cas.get(&returned.raw_artifact_ids[1]).unwrap(),
                b"diagnostic"
            );
        } else {
            assert!(returned.raw_artifact_ids.is_empty());
        }
    }
}

#[test]
fn typed_document_and_malformed_or_failed_results_retain_the_same_provider_usage() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let workdir = temp.path().join("work");
    std::fs::create_dir(&workdir).unwrap();
    let contract = WorkerContract::capture(&cas, serde_json::json!({"type":"object"}),
        std::collections::BTreeMap::from([("document".into(), serde_json::json!({"type":"object","additionalProperties":false,"required":["text"],"properties":{"text":{"type":"string"}}}))])).unwrap();
    for (index, (message, failed, valid)) in [
        (
            r#"{"schema":"af.worker-reply/1","outputs":{"document":[{"text":"A release note"}]}}"#,
            false,
            true,
        ),
        (
            r#"{"schema":"af.worker-reply/1","outputs":{"document":[{"text":42}]}}"#,
            false,
            false,
        ),
        ("The provider failed after spending tokens", true, false),
    ]
    .into_iter()
    .enumerate()
    {
        let output = format!(
            "{}\n{}\n",
            serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":message}}),
            serde_json::json!({"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":70,"output_tokens":5}})
        );
        let script = temp.path().join(format!("provider-{index}"));
        let quoted = output.replace('\'', "'\\''");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{quoted}'\nexit {}\n",
                if failed { 7 } else { 0 }
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter =
            CodexTaskAdapter::new(&Command::new(script.to_str().unwrap(), vec![])).unwrap();
        let returned = adapter.invoke(
            &cas,
            &workdir,
            b"{\"declared\":\"input\"}".to_vec(),
            Duration::from_secs(5),
            false,
        );
        assert_eq!(returned.usage.as_ref().unwrap().chargeable_tokens, 35);
        assert_eq!(
            cas.get(&returned.raw_artifact_ids[0]).unwrap(),
            output.as_bytes()
        );
        let admitted = returned
            .message
            .and_then(|bytes| contract.validate_reply(&bytes));
        assert_eq!(admitted.is_ok(), valid);
    }
}
