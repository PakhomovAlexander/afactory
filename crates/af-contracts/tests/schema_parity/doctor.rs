//! `af doctor --json` (`af/doctor@3`, ADR-0210).
//!
//! It carries no Rust type: the binary builds it from what it reads of the machine. The schema is
//! the contract, and these cases pin both halves of it: the checks an agent may branch on, and
//! the shapes it must never see (a fix or hint on an `ok` check, a `warn` with neither a fix nor
//! a hint, an unknown check or status, a charge, the retired version 1).

use super::{assert_invalid, assert_valid};
use serde_json::{Value, json};

fn doctor() -> Value {
    json!({
        "schema": "af/doctor@3",
        "token_free": true,
        "exit_code": 1,
        "checks": [
            {"id": "binary", "status": "ok", "detail": "af 0.12.0 verified by minisign",
             "fix": null, "hint": null},
            {"id": "path", "status": "warn", "detail": "/home/o/.local/bin is not on PATH",
             "fix": "fish_add_path /home/o/.local/bin", "hint": null},
            {"id": "shell", "status": "warn", "detail": "$SHELL (unset) is not one of bash, zsh",
             "fix": "af self setup-shell --shell SHELL --write",
             "hint": "SHELL: the shell you use, one of bash, zsh, fish, elvish, powershell"},
            {"id": "update", "status": "ok", "detail": "never checked", "fix": null, "hint": null},
            {"id": "tools", "status": "fail", "detail": "git is not installed",
             "fix": "sudo apt install git", "hint": null},
            {"id": "providers", "status": "warn", "detail": "no Provider registered",
             "fix": "af provider detect --apply", "hint": "it registers the logins it finds"},
            {"id": "storage", "status": "warn", "detail": "Spotlight indexes /w/target",
             "fix": null, "hint": "add it in System Settings > Spotlight > Search Privacy"},
            {"id": "project", "status": "warn", "detail": "/w has no .af/",
             "fix": "af onboard --repo /w", "hint": "a token-free preview"},
            {"id": "harness", "status": "ok", "detail": "driving agent harness: Codex",
             "fix": null, "hint": null},
            {"id": "sandbox", "status": "ok", "detail": "agent sandbox: detected",
             "fix": null, "hint": null},
            {"id": "network", "status": "ok", "detail": "agent sandbox network: blocked",
             "fix": null, "hint": null},
        ],
    })
}

#[test]
fn doctor_checks_are_closed_and_every_problem_carries_its_fix() {
    assert_valid("doctor-v3.json", &doctor());
    let mut outside = doctor();
    outside["checks"].as_array_mut().unwrap().remove(7);
    outside["exit_code"] = json!(0);
    assert_valid("doctor-v3.json", &outside);

    for (why, edit) in [
        ("an older kind", json!({"schema": "af/doctor@0"})),
        ("the retired version 1", json!({"schema": "af/doctor@1"})),
        ("the retired version 2", json!({"schema": "af/doctor@2"})),
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
        assert_invalid("doctor-v3.json", &invalid, why);
    }
    for (why, index, key, value) in [
        ("a fix on an ok check", 0, "fix", json!("af self update")),
        ("a hint on an ok check", 0, "hint", json!("nothing to do")),
        ("a warning with neither fix nor hint", 1, "fix", Value::Null),
        ("a failure with neither fix nor hint", 4, "fix", Value::Null),
        ("a problem without its advice", 6, "hint", Value::Null),
        ("an empty hint", 2, "hint", json!("")),
        ("an unknown status", 2, "status", json!("skipped")),
        ("an unknown check", 3, "id", json!("unexpected")),
        ("an empty detail", 6, "detail", json!("")),
    ] {
        let mut invalid = doctor();
        invalid["checks"][index][key] = value;
        assert_invalid("doctor-v3.json", &invalid, why);
    }
    let mut extra = doctor();
    extra["checks"][0]["severity"] = json!("low");
    assert_invalid("doctor-v3.json", &extra, "an unknown check field");
    let mut without = doctor();
    without["checks"][1].as_object_mut().unwrap().remove("hint");
    assert_invalid(
        "doctor-v3.json",
        &without,
        "a version 1 check, without its hint",
    );
    let mut short = doctor();
    short["checks"] = json!([]);
    assert_invalid("doctor-v3.json", &short, "a report without its checks");
}
