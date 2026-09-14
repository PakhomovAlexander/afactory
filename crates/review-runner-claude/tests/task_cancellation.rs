#![cfg(unix)]
#[path = "../../review-runner/tests/support/native_cancellation.rs"]
mod fixture;

#[test]
fn native_cancellation_retains_full_usage_and_reaps_owned_process() {
    let output = serde_json::to_vec(&serde_json::json!({"is_error":false,"result":"OK",
        "usage":{"input_tokens":u64::MAX,"output_tokens":20,"cache_creation_input_tokens":7,"cache_read_input_tokens":3}})).unwrap();
    fixture::check(
        |program| {
            Box::new(
                review_runner_claude::task::ClaudeTaskAdapter::new(&review_core::Command::new(
                    program,
                    ["--model", "fixture-model-1", "--effort", "high"]
                        .into_iter()
                        .map(review_core::Arg::literal)
                        .collect(),
                ))
                .unwrap(),
            )
        },
        &output,
        review_core::task::usage::TaskTokenUsageV3 {
            input_tokens: Some(u128::from(u64::MAX).into()),
            output_tokens: Some(20u128.into()),
            cache_read_tokens: Some(3u128.into()),
            cache_write_tokens: Some(7u128.into()),
            reasoning_tokens: None,
            chargeable_tokens: (u128::from(u64::MAX) + 27).into(),
        },
    );
}
