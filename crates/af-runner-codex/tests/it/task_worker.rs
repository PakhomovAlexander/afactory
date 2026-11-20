use crate::load_safe_wall::LOAD_SAFE_WALL;
use af_contracts::{Arg, Command};
use af_runner::task::{WorkerAccess, WorkerContract, WorkerModelAdapter};
use af_runner_codex::task::{CodexTaskAdapter, task_sandbox_mode};
use af_store::Cas;
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
        af_contracts::CredentialModeV1::TrustedUnsafe
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
        LOAD_SAFE_WALL,
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
            ["--model", "gpt-x-1", "-c", "model_reasoning_effort=high"]
                .into_iter()
                .map(Arg::literal)
                .collect(),
        ))
        .unwrap();
        let returned = adapter.invoke(
            &cas,
            &sandbox,
            b"review".to_vec(),
            LOAD_SAFE_WALL,
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
            [
                "--model",
                "gpt-x-1",
                "-c",
                "model_reasoning_effort=high",
                "-",
            ]
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
            af_runner::run_supervised(&mut ready, None, Duration::from_secs(5))
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
            let message = returned.message.as_ref().unwrap_err();
            assert!(
                message.contains("did not answer within 500ms"),
                "the 500 ms wall ends the provider: {message}"
            );
            assert_eq!(returned.raw_artifact_ids.len(), 2);
            // The raw-output rule: the turn line is kept, the agent message's line is not.
            let kept = output.split_once('\n').unwrap().1;
            assert_eq!(
                cas.get(&returned.raw_artifact_ids[0]).unwrap(),
                kept.as_bytes()
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
            LOAD_SAFE_WALL,
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        assert_eq!(returned.usage.as_ref().unwrap().chargeable_tokens.get(), 35);
        // The raw-output rule: the turn line is kept, the agent message's line is not.
        let kept = output.split_once('\n').unwrap().1;
        assert_eq!(
            cas.get(&returned.raw_artifact_ids[0]).unwrap(),
            kept.as_bytes()
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
        LOAD_SAFE_WALL,
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
    // The raw-output rule: the Store keeps every turn line the accounting read, the `-o` final
    // message beside them, and never the agent message's stream line.
    let kept = output.rsplit_once('\n').unwrap().0.to_string() + "\n";
    assert_eq!(
        cas.get(&returned.raw_artifact_ids[0]).unwrap(),
        kept.as_bytes()
    );
    assert_eq!(
        cas.get(returned.raw_artifact_ids.last().unwrap()).unwrap(),
        b"OK"
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
            LOAD_SAFE_WALL,
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
            let kept = output.split_once('\n').unwrap().0.to_string() + "\n";
            assert_eq!(
                cas.get(&returned.raw_artifact_ids[0]).unwrap(),
                kept.as_bytes()
            );
            assert_eq!(
                cas.get(&returned.raw_artifact_ids[1]).unwrap(),
                b"usage fixture"
            );
        }
    }
}

/// A Codex Attempt's HOME is a private directory af owns for that Attempt, not the candidate
/// tree it works in, so a committed dotfile never configures its tools' shell (L9). CODEX_HOME
/// is still the granted auth directory, redacted from what is stored.
#[test]
fn a_codex_attempt_home_is_private_and_not_the_candidate_tree() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let workdir = temp.path().join("candidate");
    std::fs::create_dir(&workdir).unwrap();
    std::fs::write(workdir.join(".bash_profile"), "echo planted\n").unwrap();
    let auth = temp.path().join("codex-auth");
    std::fs::create_dir(&auth).unwrap();
    let program = temp.path().join("fake-codex");
    std::fs::write(&program, format!("#!/bin/sh\ncat >/dev/null\nprintf 'home=%s\\n' \"$HOME\" >&2\n[ -d \"$HOME\" ] && [ -w \"$HOME\" ] && [ -z \"$(ls -A \"$HOME\")\" ] && printf 'private-empty\\n' >&2\nprintf 'codex_home=%s\\n' \"$CODEX_HOME\" >&2\n{}printf '%s\\n' '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}'\n", write_reply("OK"))).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = CodexTaskAdapter::new(&Command::new(program.to_str().unwrap(), vec![]))
        .unwrap()
        .with_codex_home(auth.display().to_string());
    let returned = adapter.invoke(
        &cas,
        &workdir,
        b"implement".to_vec(),
        LOAD_SAFE_WALL,
        WorkerAccess::WriteSourceWithShell,
        None,
        &[],
    );
    assert_eq!(returned.message.unwrap(), b"OK");
    let stderr = String::from_utf8(cas.get(&returned.raw_artifact_ids[1]).unwrap()).unwrap();
    let home = stderr
        .lines()
        .find_map(|line| line.strip_prefix("home="))
        .unwrap();
    assert!(stderr.contains("private-empty\n"), "{stderr}");
    let candidate = workdir.canonicalize().unwrap();
    assert!(!home.is_empty() && std::path::Path::new(home).is_absolute());
    assert_ne!(std::path::Path::new(home), workdir);
    assert!(!std::path::Path::new(home).starts_with(&workdir));
    assert!(!std::path::Path::new(home).starts_with(&candidate));
    assert!(stderr.contains("codex_home=[redacted]\n"), "{stderr}");
}

/// ADR-0208, issue #33: a typed request's reply contract reaches Codex as `--output-schema`: a
/// file in the Attempt's staging directory whose bytes are exactly the strict schema
/// `reply_schema::strict` derives from the request's own output schemas, so Codex itself refuses
/// a structurally wrong final message. What the strict schema made the model write beyond the
/// contract — `null` for an optional field, an empty unused port — is removed before the kernel
/// judges the reply; a legacy prompt runs without a schema.
#[test]
fn a_typed_request_passes_its_reply_contract_as_the_output_schema_file() {
    use af_runner::task::{WorkerRequest, reply_schema};
    use std::collections::BTreeMap;
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let copy = temp.path().join("schema-copy.json");
    let reply = r#"{"schema":"af.worker-reply/1","outputs":{"notes":[],"result":[{"outcome":"passed","note":null}]}}"#;
    let program = temp.path().join("fake-codex");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' \"$@\" >&2\nschema=; prev=; for arg in \"$@\"; do [ \"$prev\" = --output-schema ] && schema=$arg; prev=$arg; done\n[ -n \"$schema\" ] && cp \"$schema\" '{}'\n{}printf '%s\\n' '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}'\n",
            copy.display(),
            write_reply(reply)
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = CodexTaskAdapter::new(&Command::new(program.to_str().unwrap(), vec![])).unwrap();
    let request = WorkerRequest::new(
        "Judge it.",
        BTreeMap::new(),
        Vec::new(),
        BTreeMap::from([
            (
                "result".to_string(),
                serde_json::json!({"type":"object","additionalProperties":false,"required":["outcome"],
                    "properties":{"outcome":{"enum":["passed","failed"]},"note":{"type":"string"}}}),
            ),
            (
                "notes".to_string(),
                serde_json::json!({"type":"object","additionalProperties":false,"properties":{}}),
            ),
        ]),
    )
    .rendered()
    .unwrap();
    let returned = adapter.invoke(
        &cas,
        temp.path(),
        request.clone(),
        LOAD_SAFE_WALL,
        WorkerAccess::ReadOnly,
        None,
        &[],
    );
    let message = returned.message.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&message).unwrap(),
        serde_json::json!({"schema":"af.worker-reply/1","outputs":{"result":[{"outcome":"passed"}]}})
    );
    let flags = String::from_utf8(cas.get(&returned.raw_artifact_ids[1]).unwrap()).unwrap();
    let flags: Vec<_> = flags.lines().collect();
    let at = flags
        .iter()
        .position(|flag| *flag == "--output-schema")
        .expect("the schema argument is passed");
    assert!(flags[at + 1].ends_with("/reply-schema.json"), "{flags:?}");
    let expected = reply_schema::strict(&request).unwrap().unwrap().to_string();
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), expected);
    // The `-o` file is kept as it was written, nulls and all.
    assert!(
        returned
            .raw_artifact_ids
            .iter()
            .any(|id| cas.get(id).unwrap() == reply.as_bytes())
    );

    // A legacy prompt passes no schema.
    std::fs::remove_file(&copy).unwrap();
    let returned = adapter.invoke(
        &cas,
        temp.path(),
        b"review".to_vec(),
        LOAD_SAFE_WALL,
        WorkerAccess::ReadOnly,
        None,
        &[],
    );
    let flags = String::from_utf8(cas.get(&returned.raw_artifact_ids[1]).unwrap()).unwrap();
    assert!(
        !flags.lines().any(|flag| flag == "--output-schema"),
        "{flags}"
    );
    assert!(!copy.exists());
}

/// Current Task strict reply normalization preserves raw bytes; a missing final message
/// retains usage and never admits a business output.
#[test]
fn a_typed_request_keeps_raw_strict_reply_and_missing_message_usage() {
    use af_runner::task::{WorkerContract, WorkerRequest, reply_schema::ReplyContract};
    use serde_json::json;
    use std::collections::BTreeMap;
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let copy = temp.path().join("schema-copy.json");
    let payload = json!({"type":"object","additionalProperties":false,"required":["outcome"],
        "properties":{"outcome":{"enum":["passed","failed"]},"note":{"type":"string"}}});
    let outputs = BTreeMap::from([("result".into(), payload)]);
    let contract =
        WorkerContract::capture(&cas, json!({"type":"object"}), outputs.clone()).unwrap();
    let request = WorkerRequest::new("Judge it.", BTreeMap::new(), Vec::new(), outputs)
        .rendered()
        .unwrap();
    let reply =
        r#"{"schema":"af.worker-reply/1","outputs":{"result":[{"outcome":"passed","note":null}]}}"#;
    let script = |write: &str| {
        format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' \"$@\" >&2\nschema=; prev=; for arg in \"$@\"; do [ \"$prev\" = --output-schema ] && schema=$arg; prev=$arg; done\n[ -n \"$schema\" ] && cp \"$schema\" '{}'\n{write}printf '%s\\n' '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":7,\"output_tokens\":2}}}}'\n",
            copy.display(),
        )
    };
    let program = temp.path().join("fake-codex");
    std::fs::write(&program, script(&write_reply(reply))).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = CodexTaskAdapter::new(&Command::new(program.to_str().unwrap(), vec![])).unwrap();
    let invoke = || {
        adapter
            .invoke_constrained(
                &cas,
                temp.path(),
                request.clone(),
                LOAD_SAFE_WALL,
                WorkerAccess::WriteSource,
                None,
                &[],
                ReplyContract::Request,
            )
            .unwrap_or_else(|unstarted| *unstarted.0)
    };
    let returned = invoke();
    let message = returned.message.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&message).unwrap(),
        json!({"schema":"af.worker-reply/1","outputs":{"result":[{"outcome":"passed"}]}})
    );
    contract.validate_reply(&message).unwrap();
    let flags = String::from_utf8(cas.get(&returned.raw_artifact_ids[1]).unwrap()).unwrap();
    let flags: Vec<_> = flags.lines().collect();
    let at = flags
        .iter()
        .position(|flag| *flag == "--output-schema")
        .expect("the schema argument is passed");
    assert!(flags[at + 1].ends_with("/reply-schema.json"), "{flags:?}");
    assert_eq!(
        std::fs::read_to_string(&copy).unwrap(),
        ReplyContract::Request
            .strict(&request)
            .unwrap()
            .unwrap()
            .to_string()
    );
    assert!(
        returned
            .raw_artifact_ids
            .iter()
            .any(|id| cas.get(id).unwrap() == reply.as_bytes()),
        "the -o file is kept as written"
    );

    // No final message: nothing for the kernel to parse, and the call's usage stays.
    std::fs::write(&program, script("")).unwrap();
    let returned = invoke();
    assert!(
        returned
            .message
            .unwrap_err()
            .starts_with("Codex Worker returned no final message")
    );
    assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 9);
}

/// ADR-0208, issue #295: an exit 1 that wrote nothing is a transient ending, and so is an
/// overloaded model; an authentication failure is not.
#[test]
fn a_silent_exit_and_an_overloaded_model_are_transient_and_an_auth_failure_is_not() {
    use af_runner::native_failure::TransientClass;
    for (body, expected) in [
        (
            "cat >/dev/null\nexit 1",
            Some(TransientClass::ExitedWithoutOutput),
        ),
        (
            "cat >/dev/null\necho 'stream error: 529 overloaded_error' >&2\nexit 1",
            Some(TransientClass::Capacity),
        ),
        (
            "cat >/dev/null\necho 'Not logged in. Please run /login' >&2\nexit 1",
            None,
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        let program = temp.path().join("fake-codex");
        std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter =
            CodexTaskAdapter::new(&Command::new(program.to_str().unwrap(), vec![])).unwrap();
        let returned = adapter.invoke(
            &cas,
            temp.path(),
            b"review".to_vec(),
            LOAD_SAFE_WALL,
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        assert!(returned.message.is_err(), "{body}");
        assert_eq!(
            returned.notes.transient.map(|transient| transient.class),
            expected,
            "{body}"
        );
    }
}

/// #1106: real shipped triage/report/evaluator contracts reach the real adapter's schema
/// file; a synthetic native request rejection is deterministic and never re-asked.
#[test]
fn shipped_contract_schema_files_and_deterministic_rejections_are_hermetic() {
    use af_contracts::task::execution::TaskInvocationV1;
    use af_runner::native_failure::NativeFailureKind;
    use af_runner::task::{WorkerRequest, invoke_model, reply_schema};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let copy = temp.path().join("schema.json");
    let calls = temp.path().join("calls");
    let program = temp.path().join("fake-codex");
    let setup = format!(
        "#!/bin/sh\ncat >/dev/null\necho call >> '{}'\nprev=; for arg in \"$@\"; do [ \"$prev\" = --output-schema ] && cp \"$arg\" '{}'; prev=$arg; done\n",
        calls.display(),
        copy.display()
    );
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    for path in [
        "fixtures/custom-tasks/triage/worker/outputs/triage.schema.json",
        ".af/task-packages/backlog/triage-author/outputs/draft.schema.json",
        ".af/task-packages/issue-internal/report-verifier/outputs/result.schema.json",
        ".af/task-packages/issue-internal/evaluator/outputs/result.schema.json",
    ] {
        let schema: Value =
            serde_json::from_slice(&std::fs::read(root.join(path)).unwrap()).unwrap();
        let outputs = BTreeMap::from([("result".into(), schema)]);
        let contract =
            WorkerContract::capture(&cas, json!({"type":"object"}), outputs.clone()).unwrap();
        let request = WorkerRequest::new("Judge.", BTreeMap::new(), vec![], outputs)
            .rendered()
            .unwrap();
        std::fs::write(&program, format!("{setup}echo '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"invalid_json_schema\"}}}}'\n{}echo '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":2,\"output_tokens\":1}}}}'\n", write_reply(r#"{"schema":"af.worker-reply/1","outputs":{"result":[]}}"#))).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter =
            CodexTaskAdapter::new(&Command::new(program.to_str().unwrap(), vec![])).unwrap();
        let returned = adapter.invoke(
            &cas,
            temp.path(),
            request,
            LOAD_SAFE_WALL,
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        assert_eq!(returned.native_failure, None);
        assert!(returned.notes.transient.is_none());
        contract.validate_reply(&returned.message.unwrap()).unwrap();
        let actual: Value = serde_json::from_slice(&std::fs::read(&copy).unwrap()).unwrap();
        reply_schema::validate_strict(&actual).unwrap();
        let payload = &actual["properties"]["outputs"]["properties"]["result"]["items"];
        if path.contains("custom-tasks") {
            assert_eq!(payload["properties"]["schema"]["type"], "string");
            let kinds = payload["properties"]["class"]["type"].as_array().unwrap();
            assert!(kinds.contains(&json!("null")) && kinds.contains(&json!("string")));
        }
    }
    let adapter = CodexTaskAdapter::new(&Command::new(program.to_str().unwrap(), vec![])).unwrap();
    let before = std::fs::read(&calls).unwrap();
    let invalid = WorkerRequest::new(
        "Judge.",
        BTreeMap::new(),
        vec![],
        BTreeMap::from([("triage".into(), json!({"type":"bogus"}))]),
    )
    .rendered()
    .unwrap();
    let refused = adapter.invoke(
        &cas,
        temp.path(),
        invalid,
        LOAD_SAFE_WALL,
        WorkerAccess::ReadOnly,
        None,
        &[],
    );
    assert!(refused.message.unwrap_err().contains("triage"));
    assert_eq!(refused.usage.unwrap().chargeable_tokens.get(), 0);
    assert_eq!(
        std::fs::read(&calls).unwrap(),
        before,
        "preflight must not spawn provider"
    );

    // Real invoke_model/re-ask path: failed native events are never model prose. Include an
    // earlier transient error to prove the deterministic final schema rejection takes priority.
    for rejection_channel in [
        "framed",
        "stderr",
        "unframed",
        "mixed-stderr",
        "mixed-unframed",
        "mixed-error",
    ] {
        for auth_position in [None, Some(false), Some(true)] {
            for usage in [
                "",
                "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}\n",
            ] {
                let events = format!(
                    "{{\"type\":\"error\",\"message\":\"connection reset\"}}\n{usage}{{\"type\":\"turn.failed\",\"error\":{{\"code\":\"invalid_json_schema\",\"message\":\"Invalid schema for response_format codex_output_schema: At schema path /properties/outputs/properties/triage/items/properties/schema, schema must specify a type.\"}}}}\n"
                );
                let (events, diagnostic) = if rejection_channel == "framed"
                    || rejection_channel == "mixed-error"
                {
                    (
                        if rejection_channel == "mixed-error" {
                            events.replace("Invalid schema for", "HTTP 401 authentication_error for alice@example.com Invalid schema for")
                        } else {
                            events
                        },
                        String::new(),
                    )
                } else {
                    (
                        format!("{{\"type\":\"error\",\"message\":\"connection reset\"}}\n{usage}"),
                        format!(
                            "printf '%s\\n' '{}invalid_json_schema: schema must specify a type'{}\n",
                            if rejection_channel.starts_with("mixed-") {
                                "HTTP 401 authentication_error for alice@example.com "
                            } else {
                                ""
                            },
                            if rejection_channel.ends_with("stderr") {
                                " >&2"
                            } else {
                                ""
                            }
                        ),
                    )
                };
                let auth = "{\"type\":\"turn.failed\",\"error\":{\"message\":\"HTTP 401 authentication_error for alice@example.com\"}}\n";
                let events = match auth_position {
                    Some(false) => format!("{auth}{events}"),
                    Some(true) => format!("{events}{auth}"),
                    None => events,
                };
                std::fs::write(
                    &program,
                    format!("{setup}printf '%s' '{events}'\n{diagnostic}exit 1\n"),
                )
                .unwrap();
                let contract = WorkerContract::capture(
                    &cas,
                    json!({"type":"object"}),
                    BTreeMap::from([("result".into(), json!({"type":"string"}))]),
                )
                .unwrap();
                let context = contract
                    .prepare(
                        &cas,
                        &TaskInvocationV1 {
                            plan_id: cas.put(b"plan").unwrap(),
                            node: "root.nodes.test".into(),
                            inputs: BTreeMap::new(),
                        },
                        &[],
                        "Judge.",
                    )
                    .unwrap();
                let before = std::fs::read_to_string(&calls).unwrap().lines().count();
                let returned = invoke_model(
                    &cas,
                    temp.path(),
                    &adapter,
                    &contract,
                    &context,
                    LOAD_SAFE_WALL,
                    WorkerAccess::ReadOnly,
                    None,
                    1000,
                );
                let error = returned.reply.as_ref().unwrap_err();
                assert!(error.contains("invalid_json_schema"));
                assert!(!error.contains("alice@example.com"), "{error}");
                assert_eq!(
                    returned.native_failure,
                    Some(NativeFailureKind::UnsupportedInvocation)
                );
                assert!(returned.native_failure.unwrap().refuses_input());
                assert!(returned.notes.transient.is_none());
                if usage.is_empty() {
                    assert!(
                        returned.usage.is_none(),
                        "unreported is unknown, never zero"
                    );
                } else {
                    assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 5);
                }
                if auth_position.is_some() || rejection_channel.starts_with("mixed-") {
                    // Authentication remains sensitive before and after a deterministic
                    // schema rejection, while final classification and retry suppression
                    // still describe the schema refusal. Parsed usage stays independent.
                    assert!(!returned.raw_artifact_ids.is_empty());
                    for id in &returned.raw_artifact_ids {
                        assert_eq!(
                            cas.get(id).unwrap(),
                            NativeFailureKind::UnsupportedInvocation
                                .diagnostic()
                                .as_bytes()
                        );
                    }
                } else {
                    assert!(returned.raw_artifact_ids.iter().any(|id| {
                        String::from_utf8_lossy(&cas.get(id).unwrap())
                            .contains("schema must specify a type")
                    }));
                }
                assert_eq!(
                    std::fs::read_to_string(&calls).unwrap().lines().count(),
                    before + 1
                );
            }
        }
    }
}
