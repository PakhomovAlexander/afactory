#![cfg(unix)]
#[path = "../../review-runner/tests/support/native_cancellation.rs"]
mod fixture;

#[test]
fn native_cancellation_retains_full_turn_usage_and_reaps_owned_process() {
    let output = format!(
        "{}\n{}\n{}\n",
        serde_json::json!({"type":"turn.completed","usage":{"input_tokens":u64::MAX,"output_tokens":20}}),
        serde_json::json!({"type":"turn.completed","usage":{"input_tokens":7,"output_tokens":3}}),
        serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":"OK"}})
    );
    fixture::check(
        |program| {
            Box::new(
                review_runner_codex::task::CodexTaskAdapter::new(&review_core::Command::new(
                    program,
                    [
                        "--model",
                        "fixture-model-1",
                        "-c",
                        "model_reasoning_effort=\"high\"",
                    ]
                    .into_iter()
                    .map(review_core::Arg::literal)
                    .collect(),
                ))
                .unwrap(),
            )
        },
        output.as_bytes(),
        review_core::task::usage::TaskTokenUsageV3 {
            input_tokens: Some((u128::from(u64::MAX) + 7).into()),
            output_tokens: Some(23u128.into()),
            cache_read_tokens: Some(0u128.into()),
            cache_write_tokens: Some(0u128.into()),
            reasoning_tokens: Some(0u128.into()),
            chargeable_tokens: (u128::from(u64::MAX) + 30).into(),
        },
    );
}
