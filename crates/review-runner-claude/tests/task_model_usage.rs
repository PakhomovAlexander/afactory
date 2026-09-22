#![cfg(unix)]

use review_core::{Arg, Command, Producer, task::usage::TaskTokenUsageV2};
use review_runner::task::{
    WorkerModelAdapter,
    usage::{persist_task_usage_exact, read_task_usage_exact},
};
use review_runner_claude::task::ClaudeTaskAdapter;
use review_store::Cas;
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, time::Duration};

/// The explicit model restriction every Task binding carries.
fn opus() -> Vec<Arg> {
    vec![Arg::literal("--model"), Arg::literal("claude-opus-5")]
}

#[test]
fn title_suppression_reaches_the_child_without_replacing_personal_auth_grants() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let program = temp.path().join("synthetic-env");
    std::fs::write(&program, r##"#!/bin/sh
cat >/dev/null
printf '%s\n' "$CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC" "$CLAUDE_CODE_DISABLE_TERMINAL_TITLE" "$USER" "$HOME" "$CLAUDE_CONFIG_DIR" "${ANTHROPIC_API_KEY-unset}" "${ANTHROPIC_AUTH_TOKEN-unset}" >&2
printf '%s' '{"is_error":false,"result":"OK","usage":{"input_tokens":0,"output_tokens":0}}'
"##).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = temp.path().join("synthetic-home").display().to_string();
    let config = temp
        .path()
        .join("synthetic-personal-config")
        .display()
        .to_string();
    let adapter = ClaudeTaskAdapter::new(&Command::new(program.to_str().unwrap(), opus()))
        .unwrap()
        .with_auth(Some(config.clone()), "synthetic-user".into(), home.clone());
    let result = adapter.invoke(
        &cas,
        temp.path(),
        b"public input".to_vec(),
        Duration::from_secs(5),
        false,
    );
    assert!(result.message.is_ok());
    assert_eq!(
        cas.get(&result.raw_artifact_ids[1]).unwrap(),
        format!("1\n1\nsynthetic-user\n{home}\n{config}\nunset\nunset\n").as_bytes()
    );
}

#[test]
fn synthetic_native_multi_model_usage_survives_refusal_timeout_and_cas_outage() {
    for scenario in [
        "allowed",
        "unexpected",
        "malformed",
        "failed",
        "timeout",
        "cas-outage",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cas");
        let cas = Cas::open(&path).unwrap();
        let mut native = json!({"is_error":false,"result":"OK",
            "usage":{"input_tokens":2,"output_tokens":9451,"cache_creation_input_tokens":132688},
            "modelUsage":{
                "claude-opus-5":{"inputTokens":2,"outputTokens":9451,"cacheCreationInputTokens":132688,"cacheReadInputTokens":3830,"canonicalModel":"claude-opus-5"},
                "claude-haiku-4-5-20251001":{"inputTokens":100687,"outputTokens":17,"cacheCreationInputTokens":0,"cacheReadInputTokens":0,"canonicalModel":"claude-haiku-4-5"}}});
        match scenario {
            // Entirely zero foreign usage is metadata, so only the selected model is billed.
            "allowed" => {
                native["modelUsage"]["claude-haiku-4-5-20251001"] = json!({"inputTokens":0,
                    "outputTokens":0,"cacheCreationInputTokens":0,"cacheReadInputTokens":0,
                    "canonicalModel":"claude-haiku-4-5"});
            }
            "malformed" => {
                native["modelUsage"]["claude-haiku-4-5-20251001"]["inputTokens"] =
                    serde_json::Value::Null;
            }
            _ => {}
        }
        let output = native.to_string();
        let program = temp.path().join("synthetic-claude");
        let ending = match scenario {
            "timeout" => "sleep 10",
            "failed" => "exit 7",
            _ => "exit 0",
        };
        std::fs::write(&program,format!("#!/bin/sh\nif [ \"$1\" = --fixture-ready ]; then exit 0; fi\ncat >/dev/null\nprintf '%s' '{}'\nprintf '%s' 'synthetic diagnostic' >&2\n{ending}\n",output.replace('\'',"'\\''"))).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut ready = std::process::Command::new(&program);
        ready.arg("--fixture-ready");
        assert!(
            review_runner::run_supervised(&mut ready, None, Duration::from_secs(5))
                .unwrap()
                .status
                .success()
        );
        if scenario == "cas-outage" {
            std::fs::remove_dir_all(&path).unwrap();
            std::fs::write(&path, b"controlled unavailable CAS").unwrap();
        }
        let adapter =
            ClaudeTaskAdapter::new(&Command::new(program.to_str().unwrap(), opus())).unwrap();
        let result = adapter.invoke(
            &cas,
            temp.path(),
            b"synthetic public input".to_vec(),
            if scenario == "timeout" {
                Duration::from_millis(500)
            } else {
                Duration::from_secs(5)
            },
            false,
        );
        assert_eq!(
            result.message.is_ok(),
            scenario == "allowed",
            "{scenario}: {:?}",
            result.message
        );
        // Positive auxiliary usage refuses the reply but is charged in full, whatever else
        // failed: exit status, deadline or raw capture.
        assert_eq!(
            result.usage.as_ref().unwrap().chargeable_tokens.get(),
            match scenario {
                "allowed" => 142_141,
                "malformed" => 142_158,
                _ => 242_845,
            },
            "{scenario}"
        );
        match scenario {
            "allowed" => assert!(result.usage_observation.is_none()),
            "malformed" => assert!(!result.usage_observation.unwrap().charge_complete),
            _ => assert!(
                result.usage_observation.unwrap().charge_complete,
                "{scenario}"
            ),
        }
        if scenario == "cas-outage" {
            assert!(result.raw_artifact_ids.is_empty());
        } else {
            assert_eq!(result.raw_artifact_ids.len(), 2);
            assert_eq!(
                cas.get(&result.raw_artifact_ids[0]).unwrap(),
                output.as_bytes()
            );
            assert_eq!(
                cas.get(&result.raw_artifact_ids[1]).unwrap(),
                b"synthetic diagnostic"
            );
        }
    }
}

#[test]
fn old_top_level_artifact_identity_and_reopened_new_charge_remain_exact() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cas");
    let cas = Cas::open(&path).unwrap();
    let context = cas.put(b"original captured context").unwrap();
    let producer = Producer::KernelOperation {
        run_id: "synthetic-usage-compatibility".into(),
        node_id: None,
        operation_id: "capture@1".into(),
    };
    let old = TaskTokenUsageV2 {
        input_tokens: Some(2.into()),
        output_tokens: Some(9451.into()),
        cache_write_tokens: Some(132688.into()),
        chargeable_tokens: 142141.into(),
        ..Default::default()
    };
    let old_id = cas
        .put_artifact(
            "af/TaskTokenUsage@2",
            producer.clone(),
            vec![context.clone()],
            None,
            serde_json::to_value(&old).unwrap(),
        )
        .unwrap()
        .0;
    let old_bytes = cas.get(&old_id).unwrap();
    let mut ids = vec![];
    for expanded in [false, true] {
        let mut native = json!({"is_error":false,"result":"OK","usage":{"input_tokens":2,"output_tokens":9451,"cache_creation_input_tokens":132688}});
        if expanded {
            native["modelUsage"] = json!({"opus":{"inputTokens":2,"outputTokens":9451,"cacheCreationInputTokens":132688},"aux":{"inputTokens":100687,"outputTokens":17}});
        }
        let program = temp
            .path()
            .join(if expanded { "expanded" } else { "original" });
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{}'\n",
                native.to_string().replace('\'', "'\\''")
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = ClaudeTaskAdapter::new(&Command::new(
            program.to_str().unwrap(),
            vec![Arg::literal("--model"), Arg::literal("opus")],
        ))
        .unwrap();
        let result = adapter.invoke(
            &cas,
            temp.path(),
            b"input".to_vec(),
            Duration::from_secs(5),
            false,
        );
        if expanded {
            // The auxiliary model's usage refuses the reply and is still charged exactly.
            assert!(result.message.is_err());
            assert!(result.usage_observation.as_ref().unwrap().charge_complete);
        } else {
            assert!(result.message.is_ok());
            assert!(result.usage_observation.is_none());
        }
        ids.push(
            persist_task_usage_exact(&cas, producer.clone(), &context, &result.usage.unwrap())
                .unwrap(),
        );
    }
    assert_eq!(ids[0], old_id);
    drop(cas);
    let reopened = Cas::open(&path).unwrap();
    assert_eq!(reopened.get(&old_id).unwrap(), old_bytes);
    assert_eq!(
        read_task_usage_exact(&reopened, &ids[0])
            .unwrap()
            .chargeable_tokens
            .get(),
        142141
    );
    assert_eq!(
        read_task_usage_exact(&reopened, &ids[1])
            .unwrap()
            .chargeable_tokens
            .get(),
        242845
    );
}
