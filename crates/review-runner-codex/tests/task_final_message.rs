//! Exercise actual native output objects in a supervised test subprocess. A regression that
//! blocks opening a FIFO must fail within the harness deadline and leave no blocked test thread.

use review_core::Command;
use review_runner::task::{MAX_WORKER_BYTES, WorkerModelAdapter};
use review_runner_codex::task::CodexTaskAdapter;
use review_store::Cas;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

const CASE_ENV: &str = "AF_CODEX_LAST_MESSAGE_TEST_CASE";
const CASES: &[&str] = &[
    "fifo",
    "symlink",
    "dangling_symlink",
    "directory",
    "oversize",
    "regular",
    "exact_bound",
    "empty",
    "missing",
];

#[test]
fn native_final_message_objects_are_bounded_and_keep_exact_usage() {
    if let Ok(case) = std::env::var(CASE_ENV) {
        assert!(CASES.contains(&case.as_str()));
        run_case(&case);
        return;
    }
    for case in CASES {
        let temporary = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "native_final_message_objects_are_bounded_and_keep_exact_usage",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CASE_ENV, case)
            // Keep even a killed child's private output directory beneath parent-owned cleanup.
            .env("TMPDIR", temporary.path())
            .current_dir(temporary.path());
        let output = review_runner::run_supervised(&mut child, None, Duration::from_secs(15))
            .unwrap_or_else(|error| panic!("{case}: child did not finish: {error}"));
        assert!(
            output.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn run_case(case: &str) {
    let temporary = tempfile::tempdir().unwrap();
    let cas = Cas::open(temporary.path().join("cas")).unwrap();
    let script = temporary.path().join("native-codex");
    let source = format!(
        r#"#!/usr/bin/python3
import json, os, sys
sys.stdin.read()
print('native-final-message fixture diagnostic', file=sys.stderr)
output = sys.argv[sys.argv.index('-o') + 1]
case = '{case}'
if case == 'fifo':
    os.mkfifo(output)
elif case in ('symlink', 'dangling_symlink'):
    target = os.path.join(os.getcwd(), 'outside-message')
    if case == 'symlink':
        with open(target, 'wb') as stream: stream.write(b'never admit a followed link')
    os.symlink(target, output)
elif case == 'directory':
    os.mkdir(output)
elif case == 'oversize':
    with open(output, 'wb') as stream: stream.write(b'x' * ({MAX_WORKER_BYTES} + 1))
elif case == 'exact_bound':
    with open(output, 'wb') as stream: stream.write(b'x' * {MAX_WORKER_BYTES})
elif case == 'regular':
    with open(output, 'wb') as stream: stream.write(b'held regular file')
elif case == 'empty':
    with open(output, 'wb'): pass
elif case != 'missing':
    raise AssertionError(case)
print(json.dumps({{'type':'item.completed','item':{{'type':'agent_message','text':'stdout is not the reply'}}}}))
print(json.dumps({{'type':'turn.completed','usage':{{'input_tokens':18446744073709551615,'output_tokens':20}}}}))
"#
    );
    std::fs::write(&script, source).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = CodexTaskAdapter::new(&Command::new(script.to_str().unwrap(), vec![])).unwrap();
    let started = Instant::now();
    let returned = adapter.invoke(
        &cas,
        temporary.path(),
        b"native request".to_vec(),
        Duration::from_secs(5),
        review_runner::task::WorkerAccess::ReadOnly,
        None,
        &[],
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    let usage = returned.usage.unwrap();
    assert_eq!(usage.input_tokens.unwrap().get(), u128::from(u64::MAX));
    assert_eq!(usage.output_tokens.unwrap().get(), 20);
    assert_eq!(usage.chargeable_tokens.get(), u128::from(u64::MAX) + 20);
    assert_eq!(returned.raw_artifact_ids.len(), 2);
    // The fixture emits its own diagnostic: interpreter startup warnings differ by platform.
    let stderr = cas.get(&returned.raw_artifact_ids[1]).unwrap();
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .contains("native-final-message fixture diagnostic\n")
    );
    let stdout = cas.get(&returned.raw_artifact_ids[0]).unwrap();
    assert!(
        String::from_utf8(stdout)
            .unwrap()
            .contains("turn.completed")
    );
    match case {
        "regular" => assert_eq!(returned.message.unwrap(), b"held regular file"),
        "exact_bound" => assert_eq!(returned.message.unwrap(), vec![b'x'; MAX_WORKER_BYTES]),
        // The stdout agent message never stands in for an absent or empty `-o` file.
        "empty" | "missing" => assert_eq!(
            returned.message.unwrap_err(),
            "Codex Worker returned no final message"
        ),
        "fifo" | "directory" => {
            assert!(
                returned
                    .message
                    .unwrap_err()
                    .contains("must be a regular file")
            );
        }
        "symlink" | "dangling_symlink" => {
            assert!(
                returned
                    .message
                    .unwrap_err()
                    .contains("Cannot open Codex Worker final message")
            );
        }
        "oversize" => assert!(
            returned
                .message
                .unwrap_err()
                .contains("exceeds its byte bound")
        ),
        _ => unreachable!(),
    }
}
