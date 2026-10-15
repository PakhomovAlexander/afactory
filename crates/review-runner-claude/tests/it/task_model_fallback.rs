//! Issue #263: Claude Code can answer a Worker Attempt with a model af did not bind. A fake
//! `claude` answers like the recorded results; af refuses the reply by name, classifies a
//! safety-classifier refusal `model_refused`, charges exactly what the result reports, and runs
//! every Attempt with Claude Code's automatic model fallback turned off (ADR-0147).
use crate::load_safe_wall::LOAD_SAFE_WALL;
use review_core::{Arg, Command};
use review_runner::native_failure::NativeFailureKind;
use review_runner::task::{ModelWorkerReturn, WorkerAccess, WorkerModelAdapter};
use review_runner_claude::hint;
use review_runner_claude::task::ClaudeTaskAdapter;
use review_store::Cas;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

/// The text Claude Code shows when the bound model's safety classifier flagged a request and no
/// fallback model answered.
const SAFEGUARDS: &str = "Claude Fable 5.1's safeguards flagged this message (https://www.anthropic.com/legal/aup). This sometimes happens with safe, normal conversations.";

/// The result Claude Code returned to three Provider-admission Attempts (2026-10-13 23:47,
/// 2026-10-14 00:04, 2026-10-15 01:11) asked for `claude-fable-5-1` at effort `xhigh` with the
/// input `Reply with exactly: OK`.
fn recorded_fallback() -> Value {
    json!({"type":"result","subtype":"success","is_error":false,"result":"OK","num_turns":2,
        "safety_stops":2,"stop_reason":"end_turn","fallback_credit":null,
        "usage":{"input_tokens":4,"output_tokens":4,"cache_creation_input_tokens":9488,"cache_read_input_tokens":3001},
        "modelUsage":{
            "claude-fable-5-1":{"inputTokens":2,"outputTokens":0,"cacheCreationInputTokens":3210,"cacheReadInputTokens":3001,"canonicalModel":"claude-fable-5-1"},
            "claude-opus-5":{"inputTokens":2,"outputTokens":4,"cacheCreationInputTokens":6278,"cacheReadInputTokens":0,"canonicalModel":"claude-opus-5"}}})
}

/// The bound model's own share of that result, alone.
fn fable_only() -> Value {
    json!({"claude-fable-5-1":{"inputTokens":2,"outputTokens":0,"cacheCreationInputTokens":3210,"cacheReadInputTokens":3001,"canonicalModel":"claude-fable-5-1"}})
}

/// Run one Attempt bound to `model` at effort `xhigh` against a fake `claude` that prints
/// `result` and exits with `exit`.
fn attempt(model: &str, result: &Value, exit: u8) -> ModelWorkerReturn {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let program = temp.path().join("claude");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{}'\nexit {exit}\n",
            result.to_string().replace('\'', "'\\''")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = ClaudeTaskAdapter::new(&Command::new(
        program.to_str().unwrap(),
        ["--model", model, "--effort", "xhigh"]
            .into_iter()
            .map(Arg::literal)
            .collect(),
    ))
    .unwrap();
    adapter.invoke(
        &cas,
        temp.path(),
        b"Reply with exactly: OK\n".to_vec(),
        LOAD_SAFE_WALL,
        WorkerAccess::ReadOnly,
        None,
        &[],
    )
}

/// (a) The recorded fallback: two models, `safety_stops` 2, `num_turns` 2, `is_error` false.
/// Refused with the specific diagnostic, classified `model_refused`, charged 9496.
#[test]
fn the_recorded_fallback_is_refused_by_name_and_charged_9496() {
    let returned = attempt("claude-fable-5-1", &recorded_fallback(), 0);
    let error = returned.message.unwrap_err();
    assert!(error.starts_with(hint::DIAGNOSTIC_PREFIX), "{error}");
    for part in [
        "the safety classifier of claude-fable-5-1 flagged the request and Claude Code switched models, a safety-classifier fallback (model_refused)",
        "the Worker bound claude-fable-5-1 (effort xhigh), which wrote 0 output tokens",
        "but claude-opus-5 (4 output tokens) answered (safety_stops 2, num_turns 2)",
        "af refused the reply, because af never runs or accepts a model the Worker did not bind",
        "this is neither a credential nor a model-spelling problem",
        "next: af already sets CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1",
        "; transport: exited 0",
    ] {
        assert!(error.contains(part), "{part}: {error}");
    }
    assert!(!error.contains("unexpected model identity"), "{error}");
    assert!(!error.contains("Reply with exactly"), "{error}");
    let found = hint::find(&error).unwrap();
    assert!(
        found.ends_with("change the input or bind another model"),
        "{found}"
    );
    assert_eq!(
        returned.native_failure,
        Some(NativeFailureKind::ModelRefused)
    );
    assert!(NativeFailureKind::ModelRefused.refuses_input());
    let usage = returned.usage.unwrap();
    assert_eq!(usage.chargeable_tokens.get(), 9496);
    assert_eq!(usage.input_tokens.unwrap().get(), 4);
    assert_eq!(usage.output_tokens.unwrap().get(), 4);
    assert_eq!(usage.cache_write_tokens.unwrap().get(), 9488);
    assert_eq!(usage.cache_read_tokens.unwrap().get(), 3001);
    assert!(returned.usage_observation.unwrap().charge_complete);
}

/// (b) A refusal without fallback: `is_error` true, the safeguards text, `safety_stops` 1, usage
/// with zero output. Classified `model_refused` and charged what the result reports.
#[test]
fn a_refusal_without_fallback_is_model_refused_and_charged_what_it_reports() {
    let result = json!({"type":"result","subtype":"success","is_error":true,"result":SAFEGUARDS,
        "num_turns":1,"safety_stops":1,"stop_reason":"refusal",
        "stop_details":{"category":"cyber"},
        "usage":{"input_tokens":2,"output_tokens":0,"cache_creation_input_tokens":3210,"cache_read_input_tokens":3001},
        "modelUsage":fable_only()});
    let returned = attempt("claude-fable-5-1", &result, 1);
    let error = returned.message.unwrap_err();
    assert_eq!(
        hint::find(&error),
        Some(
            "the safety classifier of claude-fable-5-1 (effort xhigh) flagged the request and \
             claude-fable-5-1 refused it (model_refused; safety_stops 1, stop_reason refusal, \
             category cyber); af turned Claude Code's automatic switch to another model off, \
             because af never runs a model the Worker did not bind; next: af does not retry it, \
             because the same input is flagged again; change the input or bind another model, \
             then rerun"
        ),
        "{error}"
    );
    assert!(error.ends_with("; transport: exited 1"), "{error}");
    assert_eq!(
        returned.native_failure,
        Some(NativeFailureKind::ModelRefused)
    );
    assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 3212);
    assert!(returned.usage_observation.is_none());
    // The safeguards text alone, in an error result without any count, is the same refusal.
    let text_only = json!({"is_error":true,"result":SAFEGUARDS,
        "usage":{"input_tokens":2,"output_tokens":0,"cache_creation_input_tokens":3210}});
    let returned = attempt("claude-fable-5-1", &text_only, 1);
    let error = returned.message.unwrap_err();
    assert!(
        error.contains(
            "(model_refused; Claude Code reported that its safeguards flagged this message)"
        ),
        "{error}"
    );
    assert_eq!(
        returned.native_failure,
        Some(NativeFailureKind::ModelRefused)
    );
    assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 3212);
}

/// (c) `is_error` false with `safety_stops` 1 and only the bound model is never a success, and
/// neither is `stop_reason` `refusal`.
#[test]
fn a_safety_stop_on_the_bound_model_alone_is_never_a_success() {
    for (stops, stop_reason) in [(1, "end_turn"), (0, "refusal")] {
        let result = json!({"type":"result","subtype":"success","is_error":false,"result":"OK",
            "num_turns":1,"safety_stops":stops,"stop_reason":stop_reason,
            "usage":{"input_tokens":2,"output_tokens":0,"cache_creation_input_tokens":3210,"cache_read_input_tokens":3001},
            "modelUsage":fable_only()});
        let returned = attempt("claude-fable-5-1", &result, 0);
        let error = returned.message.unwrap_err();
        assert!(error.contains("(model_refused;"), "{error}");
        assert!(hint::find(&error).is_some(), "{error}");
        assert_eq!(
            returned.native_failure,
            Some(NativeFailureKind::ModelRefused)
        );
        assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 3212);
    }
    // Zero safety stops and an ordinary end are still a success.
    let mut result = recorded_fallback();
    result["modelUsage"] = fable_only();
    result["safety_stops"] = json!(0);
    result["num_turns"] = json!(1);
    result["usage"] = json!({"input_tokens":2,"output_tokens":0,"cache_creation_input_tokens":3210,"cache_read_input_tokens":3001});
    let returned = attempt("claude-fable-5-1", &result, 0);
    assert_eq!(returned.message.unwrap(), b"OK");
    assert_eq!(returned.native_failure, None);
}

/// (d) A misspelled model: Claude Code exits 1 with its own "issue with the selected model" text,
/// zero usage and no `modelUsage`. Classified `model_unavailable`, quoting the rejected spelling.
#[test]
fn a_misspelled_model_is_model_unavailable_and_names_both_provider_checks() {
    let result = json!({"type":"result","subtype":"success","is_error":true,
        "result":"There's an issue with the selected model (claude-fable-5.1). It may not exist or you may not have access to it. Run --model to pick a different model.",
        "usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}});
    let returned = attempt("claude-fable-5.1", &result, 1);
    let error = returned.message.unwrap_err();
    let found = hint::find(&error).unwrap();
    assert!(
        found.starts_with("Claude Code rejected the model `claude-fable-5.1` (model_unavailable)"),
        "{found}"
    );
    assert!(
        found.contains("does not know this model ID or the account cannot use it"),
        "{found}"
    );
    assert!(
        found.contains("`af provider status` (token-free)"),
        "{found}"
    );
    assert!(found.contains("`af provider doctor` (charged)"), "{found}");
    assert_eq!(
        returned.native_failure,
        Some(NativeFailureKind::ModelUnavailable)
    );
    assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 0);
}

/// (e) Every Attempt's environment carries the new guard beside the two existing ones, for every
/// access mode, and never `CLAUDE_CODE_NO_MODEL_FALLBACK`; the command and its tool grant are
/// unchanged.
#[test]
fn every_attempt_turns_the_refusal_fallback_off_and_keeps_compaction_substitution() {
    for access in [
        WorkerAccess::ReadOnly,
        WorkerAccess::ExecuteChecks,
        WorkerAccess::WriteSource,
        WorkerAccess::WriteSourceWithShell,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path().join("cas")).unwrap();
        let program = temp.path().join("claude");
        std::fs::write(
            &program,
            r#"#!/bin/sh
cat >/dev/null
printf '%s\n' "$CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC" "$CLAUDE_CODE_DISABLE_TERMINAL_TITLE" "${CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK-unset}" "${CLAUDE_CODE_NO_MODEL_FALLBACK-unset}" "$@" >&2
printf '%s' '{"is_error":false,"result":"OK","usage":{"input_tokens":0,"output_tokens":0}}'
"#,
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = ClaudeTaskAdapter::new(&Command::new(
            program.to_str().unwrap(),
            ["--model", "claude-fable-5-1", "--effort", "xhigh"]
                .into_iter()
                .map(Arg::literal)
                .collect(),
        ))
        .unwrap();
        let returned = adapter.invoke(
            &cas,
            temp.path(),
            b"input".to_vec(),
            LOAD_SAFE_WALL,
            access,
            None,
            &[],
        );
        assert_eq!(returned.message.unwrap(), b"OK");
        let said = String::from_utf8(cas.get(&returned.raw_artifact_ids[1]).unwrap()).unwrap();
        let said: Vec<&str> = said.lines().collect();
        let tools = review_runner_claude::task::task_tools(access);
        assert_eq!(
            said,
            [
                "1",
                "1",
                "1",
                "unset",
                "-p",
                "--output-format",
                "json",
                "--model",
                "claude-fable-5-1",
                "--effort",
                "xhigh",
                "--safe-mode",
                "--restricted",
                "--permission-mode",
                "dontAsk",
                "--strict-mcp-config",
                "--tools",
                tools,
                "--allowedTools",
                tools,
            ],
            "{access:?}"
        );
    }
}
