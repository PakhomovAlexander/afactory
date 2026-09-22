use review_core::Command;
use review_runner::task::WorkerModelAdapter;
use review_runner_codex::task::CodexTaskAdapter;
use review_store::Cas;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

#[test]
fn held_output_retains_reported_overrun_without_admitting_the_message() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let output = format!(
        "{}\n{}\n",
        serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":"OK"}}),
        serde_json::json!({"type":"turn.completed","usage":{"input_tokens":u64::MAX,"output_tokens":20}})
    );
    let script = temp.path().join("provider");
    let quoted = output.replace('\'', "'\\''");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncat >/dev/null\nout=; prev=; for arg in \"$@\"; do [ \"$prev\" = -o ] && out=$arg; prev=$arg; done\nprintf OK >\"$out\"\nprintf '%s' '{quoted}'\nprintf diagnostic >&2\nsleep 30 &\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = CodexTaskAdapter::new(&Command::new(script.to_str().unwrap(), vec![])).unwrap();
    let started = Instant::now();
    let returned = adapter.invoke(
        &cas,
        temp.path(),
        b"input".to_vec(),
        Duration::from_secs(5),
        false,
        None,
        &[],
    );
    assert!(
        returned
            .message
            .as_ref()
            .unwrap_err()
            .contains("stdout pipe was still held"),
        "{:?}",
        returned.message
    );
    assert_eq!(
        returned.usage.unwrap().chargeable_tokens.get(),
        u128::from(u64::MAX) + 20
    );
    assert_eq!(returned.raw_artifact_ids.len(), 2);
    assert_eq!(
        cas.get(&returned.raw_artifact_ids[0]).unwrap(),
        output.as_bytes()
    );
    assert_eq!(
        cas.get(&returned.raw_artifact_ids[1]).unwrap(),
        b"diagnostic"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}
