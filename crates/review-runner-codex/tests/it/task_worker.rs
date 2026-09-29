use review_core::{Arg, Command};
use review_runner::task::{WorkerAccess, WorkerContract, WorkerModelAdapter};
use review_runner_codex::task::{CodexTaskAdapter, task_sandbox_mode};
use review_store::Cas;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

/// Shell lines that write `reply` to the `-o` file, the only place the pinned codex CLI's final
/// message is read from.
fn write_reply(reply: &str) -> String {
    format!(
        "out=; prev=; for arg in \"$@\"; do [ \"$prev\" = -o ] && out=$arg; prev=$arg; done\nprintf '%s' '{}' >\"$out\"\n",
        reply.replace('\'', "'\\''")
    )
}

#[test]
fn native_task_adapter_declares_trusted_unsafe_credentials() {
    let adapter = CodexTaskAdapter::new(&Command::new("codex", vec![])).unwrap();
    assert_eq!(
        adapter.credential_mode(),
        review_core::CredentialModeV1::TrustedUnsafe
    );
}

#[test]
fn review_role_keeps_the_legacy_workspace_write_sandbox() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let program = temp.path().join("fake-codex");
    std::fs::write(&program, format!("#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' \"$@\" >&2\n{}printf '%s\\n' '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"OK\"}}}}' '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}'\n", write_reply("OK"))).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = CodexTaskAdapter::new(&Command::new(program.to_str().unwrap(), vec![])).unwrap();
    let returned = adapter.invoke(
        &cas,
        temp.path(),
        b"review".to_vec(),
        Duration::from_secs(5),
        WorkerAccess::WriteSource,
        None,
        &[],
    );
    assert_eq!(returned.message.unwrap(), b"OK");
    let flags = String::from_utf8(cas.get(&returned.raw_artifact_ids[1]).unwrap()).unwrap();
    let flags: Vec<_> = flags.lines().collect();
    let index = flags.iter().position(|value| *value == "-s").unwrap();
    assert_eq!(flags[index + 1], "workspace-write");
    assert!(!flags.contains(&"read-only"));
    assert!(!flags.iter().any(|flag| flag.contains("dangerously-bypass")));
}

#[test]
fn execute_checks_runs_workspace_write_rooted_at_the_sandbox() {
    // Package runner args are restricted to one model and one reasoning effort; sandbox,
    // approval and configuration flags are refused before a command exists.
    for (option, value) in [
        ("-s", "danger-full-access"),
        ("--sandbox", "workspace-write"),
        ("-C", "/"),
        ("-c", "sandbox_mode=\"danger-full-access\""),
        ("--dangerously-bypass-approvals-and-sandbox", "true"),
    ] {
        assert!(
            CodexTaskAdapter::new(&Command::new(
                "codex",
                vec![Arg::literal(option), Arg::literal(value)],
            ))
            .is_err(),
            "{option} is package-supplied authority"
        );
    }
    for (access, mode) in [
        (WorkerAccess::ReadOnly, "read-only"),
        (WorkerAccess::ExecuteChecks, "workspace-write"),
        (WorkerAccess::WriteSource, "workspace-write"),
        (WorkerAccess::WriteSourceWithShell, "workspace-write"),
    ] {
        assert_eq!(task_sandbox_mode(access), mode);
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        let sandbox = temp.path().join("sandbox");
        std::fs::create_dir(&sandbox).unwrap();
        let program = temp.path().join("fake-codex");
        std::fs::write(&program, format!("#!/bin/sh\ncat >/dev/null\npwd >&2\nprintf '%s\\n' \"$@\" >&2\n{}printf '%s\\n' '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}'\n", write_reply("OK"))).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = CodexTaskAdapter::new(&Command::new(
            program.to_str().unwrap(),
            ["--model", "gpt-x", "-c", "model_reasoning_effort=high"]
                .into_iter()
                .map(Arg::literal)
                .collect(),
        ))
        .unwrap();
        let returned = adapter.invoke(
            &cas,
            &sandbox,
            b"review".to_vec(),
            Duration::from_secs(5),
            access,
            None,
            &[],
        );
        assert_eq!(returned.message.unwrap(), b"OK");
        let lines = String::from_utf8(cas.get(&returned.raw_artifact_ids[1]).unwrap()).unwrap();
        let lines: Vec<_> = lines.lines().collect();
        // The process starts in the sandbox root, and `-C` names that same root.
        let root = std::fs::canonicalize(&sandbox).unwrap();
        assert_eq!(std::fs::canonicalize(lines[0]).unwrap(), root);
        let flags = &lines[1..];
        let at = |flag: &str| flags.iter().position(|value| *value == flag).unwrap();
        assert_eq!(
            std::fs::canonicalize(flags[at("-C") + 1]).unwrap(),
            root,
            "{flags:?}"
        );
        assert_eq!(flags[at("-s") + 1], mode);
        assert_eq!(flags.iter().filter(|value| **value == "-s").count(), 1);
        // Nothing but the package's own model flags follows the adapter-owned prefix.
        let output = at("-o");
        assert_eq!(
            &flags[output + 2..],
            ["--model", "gpt-x", "-c", "model_reasoning_effort=high", "-",]
        );
        assert!(!flags.iter().any(|flag| flag.contains("dangerously")));
    }
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
            "#!/bin/sh\nif [ \"$1\" = --fixture-ready ]; then exit 0; fi\ncat >/dev/null\n{}printf '%s' '{quoted}'\nprintf '%s' 'diagnostic' >&2\n{}\n",
            write_reply("OK"),
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
            WorkerAccess::ReadOnly,
            None,
            &[],
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
        assert_eq!(
            returned.usage.unwrap().chargeable_tokens.get(),
            u128::from(u64::MAX) + 20
        );
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
                "#!/bin/sh\ncat >/dev/null\n{}printf '%s' '{quoted}'\nexit {}\n",
                write_reply(message),
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
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        assert_eq!(returned.usage.as_ref().unwrap().chargeable_tokens.get(), 35);
        assert_eq!(
            cas.get(&returned.raw_artifact_ids[0]).unwrap(),
            output.as_bytes()
        );
        assert!(
            returned.usage_observation.is_none(),
            "valid usage retains the frozen path, including failed calls"
        );
        let admitted = returned
            .message
            .and_then(|bytes| contract.validate_reply(&bytes));
        assert_eq!(admitted.is_ok(), valid);
    }
}

#[test]
fn multiple_native_turns_retain_exact_components_and_uncached_charge() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let script = temp.path().join("provider");
    let mut lines = Vec::new();
    for (input, cached, output, reasoning, write) in [
        (u64::MAX, 7, u64::MAX, u64::MAX, u64::MAX),
        (20, 3, 30, 40, 50),
    ] {
        lines.push(serde_json::json!({"type":"turn.completed","usage":{"input_tokens":input,"cached_input_tokens":cached,
            "output_tokens":output,"reasoning_output_tokens":reasoning,"cache_write_input_tokens":write}}).to_string());
    }
    lines.push(
        serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":"OK"}})
            .to_string(),
    );
    let output = lines.join("\n");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncat >/dev/null\n{}printf '%s' '{}'\n",
            write_reply("OK"),
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
        Duration::from_secs(5),
        WorkerAccess::ReadOnly,
        None,
        &[],
    );
    assert_eq!(returned.message.unwrap(), b"OK");
    let usage = returned.usage.unwrap();
    let max = u128::from(u64::MAX);
    assert_eq!(usage.input_tokens.unwrap().get(), max + 20);
    assert_eq!(usage.output_tokens.unwrap().get(), max + 30);
    assert_eq!(usage.cache_read_tokens.unwrap().get(), 10);
    assert_eq!(usage.cache_write_tokens.unwrap().get(), max + 50);
    assert_eq!(usage.reasoning_tokens.unwrap().get(), max + 40);
    assert_eq!(usage.chargeable_tokens.get(), 2 * max + 40);
    assert_eq!(
        cas.get(&returned.raw_artifact_ids[0]).unwrap(),
        output.as_bytes()
    );
}

#[test]
fn malformed_native_usage_refuses_message_and_survives_raw_capture_outage() {
    for (billing_invalid, outage) in [(true, false), (false, false), (true, true)] {
        let temp = tempfile::tempdir().unwrap();
        let cas_path = temp.path().join("cas");
        let cas = Cas::open(&cas_path).unwrap();
        let mut usage = serde_json::json!({"input_tokens":11,"output_tokens":7});
        usage[if billing_invalid {
            "input_tokens"
        } else {
            "reasoning_output_tokens"
        }] = serde_json::Value::Null;
        let output=serde_json::json!({"type":"turn.completed","usage":usage}).to_string() + "\n" + &serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":"OK"}}).to_string();
        let script = temp.path().join("provider");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\n{}printf '%s' '{}'\nprintf '%s' 'usage fixture' >&2\n",
                write_reply("OK"),
                output.replace('\'', "'\\''")
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        if outage {
            std::fs::remove_dir_all(&cas_path).unwrap();
            std::fs::write(&cas_path, b"outage").unwrap();
        }
        let adapter =
            CodexTaskAdapter::new(&Command::new(script.to_str().unwrap(), vec![])).unwrap();
        let returned = adapter.invoke(
            &cas,
            temp.path(),
            b"input".to_vec(),
            Duration::from_secs(5),
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        assert!(returned.message.is_err());
        let observation = returned.usage_observation.unwrap();
        assert_eq!(observation.charge_complete, !billing_invalid);
        assert_eq!(observation.reported_usage, returned.usage);
        assert_eq!(
            returned.usage.unwrap().chargeable_tokens.get(),
            if billing_invalid { 7 } else { 18 }
        );
        if outage {
            assert!(returned.raw_artifact_ids.is_empty());
        } else {
            assert_eq!(
                cas.get(&returned.raw_artifact_ids[0]).unwrap(),
                output.as_bytes()
            );
            assert_eq!(
                cas.get(&returned.raw_artifact_ids[1]).unwrap(),
                b"usage fixture"
            );
        }
    }
}
