use super::{assert_invalid, assert_valid};
use af_contracts::task::execution::*;
use serde_json::json;

fn id(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

#[test]
fn codex_records_and_private_artifact_metadata_are_closed() {
    let attempt_id = "01jd8m4qz9k7v3n2p6r8t0w200".into();
    let launch = TaskExecutionRecordV1::CodexLaunchArmed {
        attempt_id,
        launch: CodexLaunchV1 {
            context_id: id('a'),
            executable_digest: id('b'),
            protocol_digest: id('c'),
            runtime_digest: id('d'),
            sandbox_digest: id('e'),
            reply_schema_digest: id('f'),
            submission_digest: id('0'),
            nonce: id('1'),
            thread_id: "thread-1".into(),
            mode: CodexLaunchModeV1::WarmRepair {
                checkpoint_id: id('2'),
            },
        },
    };
    launch.validate().unwrap();
    let mut json = serde_json::to_value(launch).unwrap();
    let schema = "task-execution-record-v6.json";
    assert_valid(schema, &json);
    json["launch"]["mode"]["unknown"] = true.into();
    assert_invalid(schema, &json, "unknown mode field");
    assert!(serde_json::from_value::<TaskExecutionRecordV1>(json).is_err());

    let mut evidence = json!({
        "launch_id": id('1'), "nonce": id('2'), "thread_id": "thread-1",
        "outcome": { "kind": "completed", "turn_id": "turn-1" }, "proof_id": id('3')
    });
    let schema = "codex-dispatch-evidence-v1.json";
    assert_valid(schema, &evidence);
    evidence["outcome"] = json!({"kind": "timeout"});
    assert_invalid(schema, &evidence, "timeout cannot resolve a fence");
    assert!(serde_json::from_value::<CodexDispatchEvidenceV1>(evidence).is_err());

    let mut checkpoint = json!({
        "launch_id": id('0'), "output_id": id('1'), "candidate_id": id('2'),
        "sealed_output_id": id('3'), "snapshot_id": id('4'), "reply_id": id('5'),
        "prefix_id": id('6'), "prefix_bytes": CODEX_PREFIX_MAX_BYTES
    });
    let schema = "codex-checkpoint-v1.json";
    assert_valid(schema, &checkpoint);
    serde_json::from_value::<CodexCheckpointV1>(checkpoint.clone())
        .unwrap()
        .validate()
        .unwrap();
    checkpoint["prefix_bytes"] = (CODEX_PREFIX_MAX_BYTES + 1).into();
    assert_invalid(schema, &checkpoint, "history byte bound");
    assert!(
        serde_json::from_value::<CodexCheckpointV1>(checkpoint)
            .unwrap()
            .validate()
            .is_err()
    );
}
