//! `af doctor --json` (`af/doctor@1`, ADR-0201).
//!
//! It carries no Rust type: the binary builds it from what it reads of the machine. The schema is
//! the contract, and these cases pin both halves of it: the checks an agent may branch on, and
//! the shapes it must never see (a fix on an `ok` check, a `warn` without one, an unknown check
//! or status, a charge).

use super::{assert_invalid, assert_valid};
use serde_json::{Value, json};

fn doctor() -> Value {
    json!({
        "schema": "af/doctor@1",
        "token_free": true,
        "exit_code": 1,
        "checks": [
            {"id": "binary", "status": "ok", "detail": "af 0.12.0 verified by minisign", "fix": null},
            {"id": "path", "status": "warn", "detail": "/home/o/.local/bin is not on PATH",
             "fix": "fish_add_path /home/o/.local/bin"},
            {"id": "shell", "status": "ok", "detail": "fish completions", "fix": null},
            {"id": "update", "status": "ok", "detail": "never checked", "fix": null},
            {"id": "tools", "status": "fail", "detail": "git is not installed",
             "fix": "sudo apt install git"},
            {"id": "providers", "status": "warn", "detail": "no Provider registered",
             "fix": "af provider detect --apply"},
            {"id": "storage", "status": "ok", "detail": "af keeps 0 B", "fix": null},
            {"id": "project", "status": "warn", "detail": "/w has no .af/",
             "fix": "af onboard --repo /w"},
        ],
    })
}

#[test]
fn doctor_checks_are_closed_and_every_problem_carries_its_fix() {
    assert_valid("doctor-v1.json", &doctor());
    let mut outside = doctor();
    outside["checks"].as_array_mut().unwrap().pop();
    outside["exit_code"] = json!(0);
    assert_valid("doctor-v1.json", &outside);

    for (why, edit) in [
        ("an older kind", json!({"schema": "af/doctor@0"})),
        ("a charge", json!({"token_free": false})),
        (
            "an exit code the command never uses",
            json!({"exit_code": 2}),
        ),
        ("an unknown top-level field", json!({"tokens": "0"})),
    ] {
        let mut invalid = doctor();
        for (key, value) in edit.as_object().unwrap() {
            invalid[key] = value.clone();
        }
        assert_invalid("doctor-v1.json", &invalid, why);
    }
    for (why, index, key, value) in [
        ("a fix on an ok check", 0, "fix", json!("af self update")),
        ("a warning without its fix", 1, "fix", Value::Null),
        ("a failure without its fix", 4, "fix", Value::Null),
        ("an unknown status", 2, "status", json!("skipped")),
        ("an unknown check", 3, "id", json!("network")),
        ("an empty detail", 6, "detail", json!("")),
    ] {
        let mut invalid = doctor();
        invalid["checks"][index][key] = value;
        assert_invalid("doctor-v1.json", &invalid, why);
    }
    let mut extra = doctor();
    extra["checks"][0]["severity"] = json!("low");
    assert_invalid("doctor-v1.json", &extra, "an unknown check field");
    let mut short = doctor();
    short["checks"] = json!([]);
    assert_invalid("doctor-v1.json", &short, "a report without its checks");
}
