//! Diagnostic tools run under the real authority supervisor (ADR-0175).
//!
//! The performance laboratory's diagnostic child runs (`scripts/performance/perflab/toolrun.py`)
//! never supervise a process themselves: every tool and candidate is one request to the same
//! `perf-supervisor` that brackets verdict samples, so its absolute deadline, bounded stdin,
//! concurrent drains, descendant scan and cancellation are `review_process::sample`'s
//! (ADR-0026). This test builds that example under the shared executable boundary and runs the
//! laboratory's own client through it: a child that never reads its stdin, a child whose
//! descendant holds its pipes, a child that leaves a live descendant, more output than the cap,
//! and a cancellation. None may outlast its deadline, no descendant survives its run, and only
//! a run whose lifecycle the supervisor accepts for its containment may succeed. A stream whose
//! retained prefix parses is still no success, and a diagnostic step's files are bounded all
//! together while they are written, not only each by `RLIMIT_FSIZE` (ADR-0176).

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

#[path = "support/executables.rs"]
mod executables;

fn supervisor() -> PathBuf {
    executables::built(
        "--example",
        "perf-supervisor",
        "review-process",
        env!("REVIEW_PROCESS_TARGET_TRIPLE_DIR"),
    )
}

/// The workspace root, where the laboratory's Python client lives: the run-time
/// `AF_WORKSPACE_ROOT` a warm gate exports, else the compile-time checkout (ADR-0131).
fn repository() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

#[test]
fn diagnostic_tools_get_the_supervisors_bounds_and_never_a_late_success() {
    let root = repository();
    let output = Command::new("python3")
        .arg("-I")
        .arg(root.join("scripts/performance/tests/toolrun_supervised.py"))
        .arg(&root)
        .arg(supervisor())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("LC_ALL", "C")
        .output()
        .expect("python3 runs the laboratory's client");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let case = |name: &str| -> &Value {
        cases
            .iter()
            .find(|case| case["case"] == name)
            .unwrap_or_else(|| panic!("no {name} case in {cases:?}"))
    };
    assert_eq!(case("plain")["ok"], true, "{:?}", case("plain"));
    for name in ["blocked_stdin", "partial_stream", "cancelled"] {
        let found = case(name);
        assert_eq!(found["ok"], false, "{name} reported success: {found:?}");
        assert!(found["failure"].is_string(), "{found:?}");
    }
    for found in &cases {
        assert_eq!(
            found["within_deadline"], true,
            "it outlasted its deadline: {found:?}"
        );
        assert_eq!(found["uncertain"], false, "{found:?}");
    }
    // A stream longer than its cap whose retained prefix is one valid JSON line: the prefix
    // parses, and the run is still no success before any oracle (ADR-0176).
    let prefix = case("parsing_prefix");
    assert_eq!(prefix["ok"], false, "{prefix:?}");
    assert_eq!(prefix["stdout_retained"], 65536, "{prefix:?}");
    assert_eq!(prefix["stdout_total"], 65546, "{prefix:?}");
    let detail = case("parsing_prefix_detail");
    assert_eq!(detail["prefix_parses"], true, "{detail:?}");
    assert!(
        detail["supervision_failure"]
            .as_str()
            .is_some_and(|failure| failure.contains("over its 65536-byte output cap")),
        "{detail:?}"
    );
    // The diagnostic step's files are bounded all together, not only each by RLIMIT_FSIZE.
    assert_eq!(case("aggregate_control")["refused"], Value::Null);
    assert_eq!(case("aggregate_control")["evidence_used"], 3);
    let files = case("aggregate_files");
    assert_eq!(files["refused"], "evidence_over_room", "{files:?}");
    assert_eq!(
        (&files["evidence_used"], &files["evidence_overage"]),
        (&Value::from(35), &Value::from(19)),
        "every byte is kept and counted: {files:?}"
    );
    let running = case("aggregate_running");
    assert_eq!(running["refused"], "evidence_over_room", "{running:?}");
    assert!(
        running["elapsed"].as_f64().unwrap() < 10.0,
        "cancelled while it wrote, not after its 20 s: {running:?}"
    );
    // Scratch kept for recovery halts the run before the step starts: nothing is written.
    let halted = case("aggregate_halted");
    assert_eq!(halted["refused"], "cleanup_uncertain", "{halted:?}");
    assert_eq!(halted["evidence_used"], 0, "{halted:?}");
    assert_eq!(case("blocked_stdin")["timed_out"], true);
    assert!(case("blocked_stdin")["elapsed"].as_f64().unwrap() < 4.0);
    for name in ["inherited_pipes", "live_descendant"] {
        // The supervisor ends the descendant with its child's process group, at once. Where it
        // can enumerate the group it reports the descendant and the run fails; where it cannot
        // (macOS), it says so, and only that explicitly trusted local lifecycle passes.
        let found = case(name);
        assert_eq!(found["code"], 0, "{found:?}");
        assert_eq!(
            found["survived"], false,
            "a descendant outlived its run: {found:?}"
        );
        assert!(found["elapsed"].as_f64().unwrap() < 4.0, "{found:?}");
        if found["ok"] == true {
            assert_eq!(found["status"], "no_observed_descendants", "{found:?}");
        } else {
            assert!(found["lifecycle"].is_string(), "{found:?}");
        }
    }
    let partial = case("partial_stream");
    assert_eq!(partial["stdout_retained"], 1000);
    assert_eq!(partial["stdout_total"], 200_000);
    let cancelled = case("cancelled");
    assert_eq!(cancelled["cancelled"], true, "{cancelled:?}");
    assert!(cancelled["elapsed"].as_f64().unwrap() < 10.0);
}
