//! The check fingerprint (`af/check-fingerprint@1`, telemetry design §9): its digest pinned,
//! the inputs that change it and the ones that must not, `unknown` when a part is unavailable,
//! the published schema's parity, and its place on the `af.check` span with `af.check.status`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use review_check::fingerprint::{
    CHECK_FINGERPRINT_V1, CheckFingerprintV1, DeclaredCheckV1, Execution, ExecutorV1, UNKNOWN,
    fingerprint,
};
use review_check::{Arg, CheckDefinition, CheckRunner, Command, FingerprintInputs};
use review_store::Cas;
use serde_json::Value;

const SNAPSHOT: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn workspace_root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn validator() -> jsonschema::Validator {
    let path = workspace_root().join("schemas/check-fingerprint-v1.json");
    let schema: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

/// An environment as a check receives it: names and values in the order applied.
type Environment = Vec<(String, String)>;

/// A program file of fixed bytes below `root`, and the environment a check runs it with: a
/// `PATH`, and runtime paths below `root`.
fn fixture(root: &Path) -> (Environment, Vec<(String, PathBuf)>) {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let program = bin.join("probe");
    std::fs::write(&program, b"#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let runtime = root.join("runtime");
    let environment = vec![
        (
            "PATH".to_string(),
            format!("{}:/usr/bin:/bin", bin.display()),
        ),
        ("LC_ALL".into(), "C".into()),
        ("HOME".into(), runtime.join("home").display().to_string()),
        ("TMPDIR".into(), runtime.join("tmp").display().to_string()),
        (
            "CARGO_TARGET_DIR".into(),
            root.join("warm/target").display().to_string(),
        ),
    ];
    let runtime_paths = vec![
        ("runtime".to_string(), runtime),
        (
            "warm:CARGO_TARGET_DIR".to_string(),
            root.join("warm/target"),
        ),
    ];
    (environment, runtime_paths)
}

fn inputs(runtime_paths: Vec<(String, PathBuf)>) -> FingerprintInputs {
    FingerprintInputs {
        snapshot_id: SNAPSHOT.into(),
        toolchain: None,
        runtime_paths,
    }
}

fn of(
    root: &Path,
    args: &[&str],
    environment: &[(String, String)],
    inputs: Option<&FingerprintInputs>,
) -> String {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    fingerprint(
        &Execution {
            program: "probe",
            args: &args,
            timeout: Duration::from_secs(60),
            workdir: &root.join("sandbox"),
            environment,
        },
        inputs,
    )
}

/// Design §9 and §15 P3: the digest is SHA-256 over the canonical document; the same declared
/// check on the same Snapshot, executor and effective environment shares one fingerprint
/// wherever its runtime directories lie; a changed argument, a changed `PATH` value (identical
/// names, different values) or another Snapshot gives another; a toolchain declared without its
/// id, a program that does not resolve, or a caller that named no runtime paths gives `unknown`.
#[test]
fn telemetry_check_fingerprint_pins() {
    // The digest of one fixed document, pinned: SHA-256 over its canonical JSON (keys sorted,
    // no whitespace), which any JSON tool reproduces.
    let document = CheckFingerprintV1 {
        schema: CHECK_FINGERPRINT_V1.into(),
        check: DeclaredCheckV1 {
            program: "cargo".into(),
            args: vec!["test".into(), "--workspace".into()],
            timeout_ms: 3_600_000,
            working_directory: "sandbox_root".into(),
        },
        executor: ExecutorV1 {
            platform: "aarch64-apple-darwin".into(),
            program_digest: format!("sha256:{}", "2".repeat(64)),
            toolchain: None,
        },
        environment_digest: format!("sha256:{}", "3".repeat(64)),
        snapshot_id: SNAPSHOT.into(),
    };
    assert_eq!(
        document.fingerprint().unwrap(),
        "sha256:ef4721516ba2dfc7de97b49954e38c940e457bd561cae1e2b07c85d2b024ad89"
    );

    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (environment, paths) = fixture(first.path());
    let base = of(
        first.path(),
        &["--all"],
        &environment,
        Some(&inputs(paths.clone())),
    );
    assert!(base.starts_with("sha256:"), "{base}");
    // The same check elsewhere: another sandbox and runtime directory, the same program bytes.
    let (other_environment, other_paths) = fixture(second.path());
    let mut moved = other_environment.clone();
    // The PATH names the program's own directory: point it where the first one resolves.
    moved[0].1 = environment[0].1.clone();
    assert_eq!(
        base,
        of(
            second.path(),
            &["--all"],
            &moved,
            Some(&inputs(other_paths))
        ),
        "runtime paths are placeholders"
    );
    // A changed execution input.
    assert_ne!(
        base,
        of(
            first.path(),
            &["--none"],
            &environment,
            Some(&inputs(paths.clone()))
        )
    );
    // Identical environment names with different values: two fingerprints.
    let mut path_changed = environment.clone();
    path_changed[0].1 = format!("{}:/opt/bin", path_changed[0].1);
    assert_ne!(
        base,
        of(
            first.path(),
            &["--all"],
            &path_changed,
            Some(&inputs(paths.clone()))
        )
    );
    let mut locale = environment.clone();
    locale[1].1 = "en_US.UTF-8".into();
    assert_ne!(
        base,
        of(
            first.path(),
            &["--all"],
            &locale,
            Some(&inputs(paths.clone()))
        )
    );
    // Another Snapshot.
    let mut other_snapshot = inputs(paths.clone());
    other_snapshot.snapshot_id = format!("sha256:{}", "9".repeat(64));
    assert_ne!(
        base,
        of(
            first.path(),
            &["--all"],
            &environment,
            Some(&other_snapshot)
        )
    );
    // A declared toolchain is part of the executor; one declared without its id is unknown.
    let mut declared = inputs(paths.clone());
    declared.toolchain = Some(Some(format!("sha256:{}", "7".repeat(64))));
    let with_toolchain = of(first.path(), &["--all"], &environment, Some(&declared));
    assert!(with_toolchain.starts_with("sha256:") && with_toolchain != base);
    declared.toolchain = Some(None);
    assert_eq!(
        of(first.path(), &["--all"], &environment, Some(&declared)),
        UNKNOWN
    );
    // Without the executor identity: a program that does not resolve.
    let mut unresolved = environment.clone();
    unresolved[0].1 = "/nonexistent".into();
    assert_eq!(
        of(first.path(), &["--all"], &unresolved, Some(&inputs(paths))),
        UNKNOWN
    );
    // A caller that named no runtime paths.
    assert_eq!(of(first.path(), &["--all"], &environment, None), UNKNOWN);
}

/// ADR-0145: the published schema accepts the document the crate hashes and refuses a member
/// it does not name, a missing one and a plaintext environment.
#[test]
fn the_check_fingerprint_schema_matches_its_document() {
    let validator = validator();
    let document = CheckFingerprintV1 {
        schema: CHECK_FINGERPRINT_V1.into(),
        check: DeclaredCheckV1 {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "true".into()],
            timeout_ms: 1000,
            working_directory: "sandbox_root".into(),
        },
        executor: ExecutorV1 {
            platform: review_check::fingerprint::platform_triple(),
            program_digest: format!("sha256:{}", "2".repeat(64)),
            toolchain: Some(format!("sha256:{}", "5".repeat(64))),
        },
        environment_digest: format!("sha256:{}", "3".repeat(64)),
        snapshot_id: SNAPSHOT.into(),
    };
    let value = serde_json::to_value(&document).unwrap();
    assert!(validator.is_valid(&value), "{value:#}");
    let mut extra = value.clone();
    extra["environment"] = serde_json::json!({"PATH": "/usr/bin"});
    assert!(!validator.is_valid(&extra));
    let mut missing = value.clone();
    missing.as_object_mut().unwrap().remove("snapshot_id");
    assert!(!validator.is_valid(&missing));
    let mut other = value;
    other["schema"] = "af/check-fingerprint@2".into();
    assert!(!validator.is_valid(&other));
}

/// The fingerprint and `af.check.status` on the `af.check` span of a real check inside a run.
#[test]
fn the_check_span_carries_its_fingerprint_and_status() {
    use review_telemetry::level::Level;
    use review_telemetry::record::SealExit;
    use review_telemetry::segment::writer::{Writer, WriterOptions};
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let sandbox = temp.path().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let writer = Writer::new(WriterOptions::new(
        temp.path().join("telemetry"),
        Level::Spans,
    ))
    .unwrap();
    let dispatch = tracing::Dispatch::new(review_telemetry::subscriber::TelemetrySubscriber::new(
        writer.clone(),
    ));
    tracing::dispatcher::with_default(&dispatch, || {
        let run = tracing::info_span!("af.run", af.run_id = "run-1");
        let _run = run.enter();
        let runner = CheckRunner::new(&cas, &sandbox).with_fingerprint(FingerprintInputs {
            snapshot_id: SNAPSHOT.into(),
            toolchain: None,
            runtime_paths: vec![],
        });
        let check = |name: &str, script: &str| {
            CheckDefinition::new(
                name,
                Command::new("/bin/sh", vec![Arg::literal("-c"), Arg::literal(script)]),
            )
        };
        assert!(runner.run(&check("passes", "true")).passed());
        assert!(!runner.run(&check("fails", "exit 3")).passed());
        // Without declared inputs the runner cannot name the runtime paths: `unknown`.
        CheckRunner::new(&cas, &sandbox).run(&check("unnamed", "true"));
    });
    writer.flush();
    let text = std::fs::read_to_string(writer.path().unwrap()).unwrap();
    writer.finish(SealExit::Ok).unwrap();
    let ends: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["t"] == "span_end" && line["attributes"]["af.check.status"].is_string())
        .collect();
    assert_eq!(ends.len(), 3, "{text}");
    let passed = &ends[0]["attributes"];
    assert_eq!(passed["af.check.status"], "passed");
    let fingerprint = passed["af.check.fingerprint"].as_str().unwrap();
    assert!(
        fingerprint.starts_with("sha256:") && fingerprint.len() == 71,
        "{passed}"
    );
    assert_eq!(ends[0]["status"], "ok");
    let failed = &ends[1]["attributes"];
    assert_eq!(failed["af.check.status"], "failed");
    assert_eq!(failed["af.failure.class"], "gate_failed");
    assert_eq!(failed["process.exit_code"], 3);
    assert_ne!(
        failed["af.check.fingerprint"],
        passed["af.check.fingerprint"]
    );
    assert_eq!(ends[1]["status"], "error");
    assert_eq!(ends[2]["attributes"]["af.check.fingerprint"], UNKNOWN);
    // The environment is never recorded: no value of it is anywhere in the segment.
    assert!(
        !text.contains("/usr/bin:/bin") && !text.contains("\"PATH\""),
        "{text}"
    );
}
