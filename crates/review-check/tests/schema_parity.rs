//! The live `CheckResult` and `schemas/check-result-v1.json` must not drift.
//!
//! The v1 schemas held no one to account while they sat outside every parity suite — the run-4
//! review found the whole contract layer unexercised. This test pins the one payload this
//! crate emits: what `CheckRunner` records is what `CheckResult@1` describes.

use std::path::PathBuf;

use review_check::{CheckResult, CheckStatus};
use review_core::{Arg, Command};
use serde_json::{Value, json};

fn validator() -> jsonschema::Validator {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/check-result-v1.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn assert_valid(instance: &Value) {
    let v = validator();
    if !v.is_valid(instance) {
        let errors: Vec<String> = v
            .iter_errors(instance)
            .map(|e| format!("{} at {}", e, e.instance_path))
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
