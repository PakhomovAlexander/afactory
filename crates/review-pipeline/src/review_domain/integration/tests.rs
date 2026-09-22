use super::*;
use review_core::exec::Arg;
use std::time::Instant;

fn binding() -> review_config::GateExecutionSpec {
    review_config::GateExecutionSpec {
        provider: review_config::SandboxProviderSpec::TrustedLocal,
        required_isolation: review_config::IsolationSpec::None,
        mode: review_config::GateModeSpec::EphemeralWrite,
        image: None,
        caches: vec![],
        build_caches: vec![],
        build_cache_max_bytes: None,
        build_cache_max_entries: None,
    }
}

fn command(name: &str, script: &str) -> CheckDefinition {
    CheckDefinition::new(
        name,
        Command::new("/bin/sh", vec![Arg::literal("-c"), Arg::literal(script)]),
    )
}

fn policy(names: &[&str]) -> review_config::IntegrationSpec {
    review_config::IntegrationSpec {
        protected_paths: vec![],
        post_apply_checks: names.iter().map(|name| (*name).into()).collect(),
        reviewer_priority: vec![],
    }
}

#[test]
fn legacy_check_order_and_one_writable_clone_survive_the_shared_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let snapshot = cas.put(b"exact derived Snapshot").unwrap();
    let checks = [
        command(
            "first",
            "printf '%s' prepared > .shared-check; printf '%s' first",
        ),
        command("unselected", "exit 71"),
        command(
            "second",
            "test \"$(cat .shared-check)\" = prepared || exit 72; printf '%s' second",
        ),
    ];
    let binding = binding();
    let runner = IntegrationCheckSequence {
        cas: &cas,
        checks: &checks,
        check_timeout: Duration::from_secs(10),
        binding: &binding,
        container_provider: None,
    };
    // The legacy implementation filters declaration order. A differently ordered policy
    // cannot silently change it; new Task capture must validate its chosen sequence explicitly.
    let policy = policy(&["second", "first"]);
    let legacy = runner
        .run(&policy, &Manifest::default(), &snapshot, None)
        .unwrap();
    let bounded = runner
        .run(
            &policy,
            &Manifest::default(),
            &snapshot,
            Some(Instant::now() + Duration::from_secs(10)),
        )
        .unwrap();
    assert_eq!(
        bounded, legacy,
        "the deadline option changes no successful receipt bytes"
    );
    let cancellation = std::sync::atomic::AtomicBool::new(false);
    let controlled = runner
        .run_recorded(
            &policy,
            &Manifest::default(),
            &snapshot,
            None,
            Some(&cancellation),
        )
        .unwrap_or_else(|failure| panic!("{}", failure.message));
    assert_eq!(
        controlled, legacy,
        "an inactive control changes no successful canonical Check bytes"
    );
    assert!(legacy.passed());
    assert_eq!(
        legacy
            .checks
            .iter()
            .map(|check| check.name.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    for (check, stdout) in legacy.checks.iter().zip(["first", "second"]) {
        let result: review_check::CheckResult =
            serde_json::from_value(cas.get_json(&check.result_artifact_id).unwrap()).unwrap();
        assert_eq!(cas.get(&result.stdout.unwrap()).unwrap(), stdout.as_bytes());
    }
}

#[test]
fn an_absolute_attempt_deadline_refuses_setup_and_bounds_the_running_check() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let snapshot = cas.put(b"exact derived Snapshot").unwrap();
    let checks = [
        command("bounded", "sleep 60"),
        command("unstarted", "exit 0"),
    ];
    let binding = binding();
    let runner = IntegrationCheckSequence {
        cas: &cas,
        checks: &checks,
        check_timeout: Duration::from_secs(60),
        binding: &binding,
        container_provider: None,
    };
    let policy = policy(&["bounded", "unstarted"]);
    let error = runner
        .run(
            &policy,
            &Manifest::default(),
            &snapshot,
            Some(Instant::now()),
        )
        .unwrap_err();
    assert!(error.contains("Attempt deadline"), "{error}");
    let started = Instant::now();
    let failure = runner
        .run_recorded(
            &policy,
            &Manifest::default(),
            &snapshot,
            Some(started + Duration::from_millis(500)),
            None,
        )
        .err()
        .unwrap();
    assert!(failure.message.contains("Attempt deadline"));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        failure.result_artifact_ids.len(),
        1,
        "retain the executed Check, never invent the unstarted Check"
    );
    let result: review_check::CheckResult =
        serde_json::from_value(cas.get_json(&failure.result_artifact_ids[0]).unwrap()).unwrap();
    assert_eq!(result.name, "bounded");
    assert_eq!(result.status, CheckStatus::NotRun);
    assert!(
        result
            .reason
            .as_deref()
            .unwrap()
            .contains("the check was killed"),
        "{result:?}"
    );
}

#[test]
fn unconfirmed_container_cleanup_preserves_the_writable_sandbox_and_stops_checks() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let snapshot = cas.put(b"exact derived Snapshot").unwrap();
    let runtime = dir.path().join("runtime");
    std::fs::write(
        &runtime,
        r#"#!/bin/sh
if [ "$1" = info ]; then exit 0; fi
if [ "$1" = rm ]; then echo 'fixture cleanup unavailable' >&2; exit 1; fi
for arg do
  case "$arg" in *:/work:rw) source=${arg%:/work:rw}; printf '%s' "$source" > "$0.sandbox";; esac
done
printf '%s\n' run >> "$0.calls"
sleep 60
"#,
    )
    .unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
    let provider = ContainerProvider::with_runtime(&runtime);
    let mut binding = binding();
    binding.provider = review_config::SandboxProviderSpec::Container;
    binding.required_isolation = review_config::IsolationSpec::Container;
    binding.image = Some(format!("fixture@sha256:{}", "a".repeat(64)));
    let checks = [command("first", "exit 0"), command("never", "exit 0")];
    let runner = IntegrationCheckSequence {
        cas: &cas,
        checks: &checks,
        check_timeout: Duration::from_millis(100),
        binding: &binding,
        container_provider: Some(&provider),
    };
    let failure = runner
        .run_recorded(
            &policy(&["first", "never"]),
            &Manifest::default(),
            &snapshot,
            None,
            None,
        )
        .err()
        .unwrap();
    assert_eq!(failure.result_artifact_ids.len(), 1);
    let result: review_check::CheckResult =
        serde_json::from_value(cas.get_json(&failure.result_artifact_ids[0]).unwrap()).unwrap();
    assert_eq!(result.name, "first");
    assert!(!result.passed());
    let error = failure.message;
    assert!(error.contains("Integration sandbox preserved"), "{error}");
    assert_eq!(
        std::fs::read_to_string(runtime.with_extension("calls")).unwrap(),
        "run\n"
    );
    let path = std::fs::read_to_string(runtime.with_extension("sandbox")).unwrap();
    assert!(std::path::Path::new(&path).is_dir(), "{error}");
    // The fake runtime never creates a daemon process; release its deliberate forensic residue.
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn controlled_check_sequence_retains_interrupted_raw_result_and_stops_before_the_next_check() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let ready = dir.path().join("ready");
    let never = dir.path().join("never");
    let quote = |p: &std::path::Path| p.to_str().unwrap().replace('\'', "'\\''");
    let checks = [
        command(
            "first",
            &format!(
                "printf before; printf diagnostic >&2; pwd >'{}'; sleep 30",
                quote(&ready)
            ),
        ),
        command("next", &format!("touch '{}'", quote(&never))),
    ];
    let binding = binding();
    let sequence = IntegrationCheckSequence {
        cas: &cas,
        checks: &checks,
        check_timeout: Duration::from_secs(20),
        binding: &binding,
        container_provider: None,
    };
    let flag = AtomicBool::new(false);
    let snapshot = cas.put(b"derived").unwrap();
    let failure = std::thread::scope(|scope| {
        let cancel = scope.spawn(|| {
            let until = Instant::now() + Duration::from_secs(3);
            while !ready.is_file() {
                if Instant::now() >= until {
                    flag.store(true, Ordering::Release);
                    panic!("Check never began");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            flag.store(true, Ordering::Release);
        });
        let value = sequence.run_recorded(
            &policy(&["first", "next"]),
            &Manifest::default(),
            &snapshot,
            None,
            Some(&flag),
        );
        cancel.join().unwrap();
        value.err().unwrap()
    });
    assert!(failure.message.contains("cancelled"), "{}", failure.message);
    assert_eq!(failure.result_artifact_ids.len(), 1);
    let result: review_check::CheckResult =
        serde_json::from_value(cas.get_json(&failure.result_artifact_ids[0]).unwrap()).unwrap();
    assert_eq!(result.status, CheckStatus::NotRun);
    assert_eq!(cas.get(&result.stdout.unwrap()).unwrap(), b"before");
    assert_eq!(cas.get(&result.stderr.unwrap()).unwrap(), b"diagnostic");
    assert!(!never.exists());
    let sandbox = std::fs::read_to_string(ready).unwrap();
    assert!(
        !std::path::Path::new(sandbox.trim()).exists(),
        "confirmed local group cleanup releases its one writable clone"
    );
}
