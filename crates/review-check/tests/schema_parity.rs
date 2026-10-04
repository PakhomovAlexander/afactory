//! The live `CheckResult` and `schemas/check-result-v1.json` must not drift.
//!
//! The v1 schemas held no one to account while they sat outside every parity suite — the run-4
//! review found the whole contract layer unexercised. This test pins the one payload this
//! crate emits: what `CheckRunner` records is what `CheckResult@1` describes.

use std::path::PathBuf;

use review_check::{CheckResult, CheckStatus};
use review_core::{Arg, Command};
use serde_json::{Value, json};

fn workspace_root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn validator() -> jsonschema::Validator {
    let path = workspace_root().join("schemas/check-result-v1.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn assert_valid(instance: &Value) {
    let v = validator();
    if !v.is_valid(instance) {
        let errors: Vec<String> = v
            .iter_errors(instance)
            .map(|e| format!("{} at {}", e, e.instance_path()))
            .collect();
        panic!(
            "check-result-v1 rejected a live value: {}",
            errors.join("; ")
        );
    }
}

fn base(status: CheckStatus) -> CheckResult {
    CheckResult {
        name: "build".to_string(),
        status,
        exit_code: None,
        reason: None,
        program: Some("/bin/sh".to_string()),
        args: Command::new(
            "/bin/sh",
            vec![Arg::literal("-c"), Arg::untrusted("src/a.rs")],
        )
        .args,
        stdout: None,
        stderr: None,
        required: true,
        remote: None,
    }
}

#[test]
fn a_passed_check_satisfies_the_contract() {
    let digest = format!("sha256:{}", "ab".repeat(32));
    let result = CheckResult {
        exit_code: Some(0),
        stdout: Some(digest.clone()),
        stderr: Some(digest),
        ..base(CheckStatus::Passed)
    };
    assert_valid(&serde_json::to_value(&result).unwrap());
}

#[test]
fn a_failed_check_satisfies_the_contract() {
    let result = CheckResult {
        exit_code: Some(1),
        reason: Some("terminated by a signal".to_string()),
        ..base(CheckStatus::Failed)
    };
    assert_valid(&serde_json::to_value(&result).unwrap());
}

#[test]
fn a_not_run_check_satisfies_the_contract() {
    // Both live not_run shapes: could-not-start, and evidence-not-preserved. Neither carries
    // an exit code — the contract reserves it for checks that ran to a verdict.
    for reason in [
        "could not start `/bin/sh`: no such file",
        "evidence was not preserved: cas io: disk full",
    ] {
        let result = CheckResult {
            reason: Some(reason.to_string()),
            ..base(CheckStatus::NotRun)
        };
        assert_valid(&serde_json::to_value(&result).unwrap());
    }
}

#[test]
fn the_contract_still_rejects_what_it_must() {
    let v = validator();
    assert!(
        !v.is_valid(&json!({ "name": "build", "status": "ok", "args": [] })),
        "an unknown status must be refused"
    );
    assert!(
        !v.is_valid(&json!({ "name": "build", "status": "not_run", "args": [] })),
        "a not_run without a reason must be refused"
    );
}

/// The second shape (ADR-0136): a remote result carries its evidence artifact and nothing a
/// local command would have produced, and the contract refuses every mixture.
#[test]
fn a_remote_result_is_its_own_shape_and_mixtures_are_refused() {
    let evidence = format!("sha256:{}", "cd".repeat(32));
    let definition = review_check::CheckDefinition::new(
        "kernel",
        Command::new("bash", vec![Arg::literal("scripts/verify.sh")]),
    );
    for (status, reason) in [
        (CheckStatus::Passed, None),
        (
            CheckStatus::Failed,
            Some("required job `lint` concluded failure".to_string()),
        ),
        (
            CheckStatus::NotRun,
            Some("remote_check_missing: no run".to_string()),
        ),
    ] {
        let result = CheckResult::remote(&definition, status, reason, evidence.clone(), None);
        assert!(result.has_one_shape());
        let value = serde_json::to_value(&result).unwrap();
        assert_valid(&value);
        for absent in ["program", "exit_code", "stdout", "stderr"] {
            assert!(value.get(absent).is_none(), "{absent}");
        }
        assert_eq!(value["args"], json!([]));
        assert_eq!(
            serde_json::from_value::<CheckResult>(value).unwrap(),
            result
        );
    }
    let v = validator();
    let digest = format!("sha256:{}", "ab".repeat(32));
    // A remote result that did not pass may carry the unsuccessful jobs' log excerpt as
    // `stdout`; one that passed has none to carry.
    for (status, reason) in [
        (CheckStatus::Failed, None),
        (
            CheckStatus::NotRun,
            Some("remote_check_inconclusive: job cancelled".to_string()),
        ),
    ] {
        let with_log = CheckResult::remote(
            &definition,
            status,
            reason,
            evidence.clone(),
            Some(digest.clone()),
        );
        assert!(with_log.has_one_shape());
        let value = serde_json::to_value(&with_log).unwrap();
        assert_valid(&value);
        assert_eq!(value["stdout"], json!(digest));
    }
    let remote = serde_json::to_value(CheckResult::remote(
        &definition,
        CheckStatus::Passed,
        None,
        evidence.clone(),
        None,
    ))
    .unwrap();
    for (field, value) in [
        ("program", json!("bash")),
        ("exit_code", json!(0)),
        ("stdout", json!(digest)),
        ("stderr", json!(digest)),
        (
            "args",
            json!([{"value": "scripts/verify.sh", "provenance": "literal"}]),
        ),
    ] {
        let mut mixed = remote.clone();
        mixed[field] = value;
        assert!(
            !v.is_valid(&mixed),
            "a remote result with {field} is a mixture"
        );
        let parsed: CheckResult = serde_json::from_value(mixed).unwrap();
        assert!(!parsed.has_one_shape(), "{field}");
    }
    let mut unexplained = remote.clone();
    unexplained["status"] = json!("not_run");
    assert!(
        !v.is_valid(&unexplained),
        "a not_run remote result names why"
    );
    let local = CheckResult {
        exit_code: Some(0),
        ..base(CheckStatus::Passed)
    };
    assert!(local.has_one_shape());
    assert!(
        serde_json::to_value(&local)
            .unwrap()
            .get("remote")
            .is_none(),
        "a local result serializes as before"
    );
}
