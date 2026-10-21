//! Telemetry's full span coverage end to end (design §15 P3, §16): on the Task fixture,
//! `af telemetry show` prints every phase the run executed with its self time, every process
//! span carries its CPU time, every emitted span name is a row of the catalogue in
//! `docs/architecture.md`, and `af telemetry config` prints the execution's configuration
//! record with an environment-origin value as `<env>` — each over a private HOME and XDG roots.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

use crate::task_gc;
use crate::telemetry_runs::{Machine, json, text};

/// Every span name the catalogue in `docs/architecture.md` lists.
pub(crate) fn catalogue() -> BTreeSet<String> {
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let text = std::fs::read_to_string(workspace.join("docs/architecture.md")).unwrap();
    let section = text
        .split("### Telemetry span catalogue")
        .nth(1)
        .expect("docs/architecture.md has the telemetry span catalogue");
    section
        .lines()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .filter_map(|line| line.split('|').nth(1))
        .flat_map(|cell| {
            cell.split(',')
                .filter_map(|name| {
                    name.trim()
                        .strip_prefix('`')
                        .and_then(|name| name.strip_suffix('`'))
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Design §16 and §15 P3, on the Task fixture at `spans` with the level set from the
/// environment: `show` prints every phase the run executed — the authority load and plan, each
/// node and Attempt, the admission, the composed context, the harness, the gate and its checks,
/// materialization, the capture, the usage accounting, the output parse and the Store's appends
/// and reads — each with total and self time; every process span carries the user and system
/// CPU time the supervisor delivered; every emitted name is in the catalogue; and `config`
/// prints the record with `telemetry.level = <env>` from `AF_TELEMETRY__LEVEL`, never its value,
/// its `--json` valid against `schemas/telemetry-config-v1.json`.
#[test]
fn telemetry_task_run_records_every_phase() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let output = machine.af(
        &repo,
        &[("AF_TELEMETRY__LEVEL", "spans")],
        &[
            "task",
            "start",
            "--execute",
            "--file",
            "gc-older.json",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    let (stdout, stderr) = text(&output);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(!stderr.contains("telemetry problem"), "{stderr}");
    let runs = json(&machine.af(&repo, &[], &["telemetry", "runs", "--json"]));
    let run_id = runs["runs"][0]["run_id"].as_str().unwrap().to_string();
    let show = json(&machine.af(&repo, &[], &["telemetry", "show", &run_id, "--json"]));
    crate::schemas::valid(&crate::schemas::validator("telemetry-show-v1.json"), &show);
    let spans = show["spans"].as_array().unwrap();
    let names: BTreeSet<&str> = spans
        .iter()
        .map(|span| span["name"].as_str().unwrap())
        .collect();
    for phase in [
        "af.command",
        "af.run",
        "af.authority.load",
        "af.plan",
        "af.node",
        "af.attempt",
        "af.admission",
        "af.context.compose",
        "af.harness",
        "af.gate",
        "af.check",
        "af.cas.materialize",
        "af.snapshot.capture",
        "af.usage.account",
        "af.output.parse",
        "af.store.append",
        "af.store.read",
        "af.sweep",
    ] {
        assert!(names.contains(phase), "{phase} missing: {names:?}");
    }
    // Every emitted name is a row of the catalogue (`telemetry_span_catalogue` holds the
    // source to the same table).
    let catalogue = catalogue();
    for name in &names {
        assert!(catalogue.contains(*name), "{name} is not in the catalogue");
    }
    for span in spans {
        assert!(
            span["total_us"].is_u64() && span["self_us"].is_u64(),
            "{span}"
        );
    }
    // Every process span carries its process's CPU time.
    let processes: Vec<&Value> = spans
        .iter()
        .filter(|span| {
            matches!(
                span["name"].as_str(),
                Some("af.harness" | "af.check" | "af.git" | "af.process")
            )
        })
        .collect();
    assert!(processes.len() >= 2, "{names:?}");
    for span in &processes {
        let attributes = &span["attributes"];
        assert!(attributes["af.rusage.user_ms"].is_u64(), "{span}");
        assert!(attributes["af.rusage.system_ms"].is_u64(), "{span}");
        if let Some(peak) = attributes.get("af.rusage.max_rss_bytes") {
            assert!(peak.as_u64().unwrap() > 0, "{span}");
        }
    }
    let check = spans
        .iter()
        .find(|span| span["name"] == "af.check")
        .unwrap();
    assert_eq!(check["attributes"]["af.check.status"], "passed", "{check}");
    let fingerprint = check["attributes"]["af.check.fingerprint"]
        .as_str()
        .unwrap();
    assert!(
        fingerprint == "unknown" || fingerprint.starts_with("sha256:"),
        "{check}"
    );
    // The text names the same phases with their self time.
    let (shown, _) = text(&machine.af(&repo, &[], &["telemetry", "show", &run_id]));
    assert!(shown.contains("self"), "{shown}");
    for phase in [
        "af.harness",
        "af.check",
        "af.usage.account",
        "af.store.append",
    ] {
        assert!(shown.contains(phase), "{phase}: {shown}");
    }
    assert!(shown.contains("af.rusage.user_ms="), "{shown}");

    // The configuration record: the level is an environment-origin value, `<env>`.
    let config = json(&machine.af(&repo, &[], &["telemetry", "config", &run_id, "--json"]));
    crate::schemas::valid(
        &crate::schemas::validator("telemetry-config-v1.json"),
        &config,
    );
    assert_eq!(config["state"], "complete", "{config}");
    let rows = config["rows"].as_array().unwrap();
    let level = rows
        .iter()
        .find(|row| row["key"] == "telemetry.level")
        .unwrap_or_else(|| panic!("{config}"));
    assert_eq!(level["value"], "<env>");
    assert_eq!(level["layer"], "environment");
    assert_eq!(level["origin"], "AF_TELEMETRY__LEVEL");
    let variable = rows
        .iter()
        .find(|row| row["layer"] == "variable" && row["key"] == "AF_TELEMETRY__LEVEL")
        .unwrap_or_else(|| panic!("{config}"));
    assert_eq!(variable["value"], "");
    for layer in ["version", "policy", "binding", "host", "harness"] {
        assert!(
            rows.iter().any(|row| row["layer"] == layer),
            "{layer}: {config}"
        );
    }
    let policy = rows
        .iter()
        .find(|row| row["key"] == "af.policy_rev")
        .unwrap();
    assert!(policy["value"].as_str().unwrap().starts_with("sha256:"));
    let (printed, _) = text(&machine.af(&repo, &[], &["telemetry", "config", &run_id]));
    assert!(printed.contains("telemetry.level = <env>"), "{printed}");
    assert!(printed.contains("# AF_TELEMETRY__LEVEL"), "{printed}");
    assert!(printed.contains("complete"), "{printed}");
    // An execution the run does not have exits 1, naming it.
    let unknown = machine.af(
        &repo,
        &[],
        &["telemetry", "config", &run_id, "--execution", "nope"],
    );
    assert_eq!(unknown.status.code(), Some(1));
    assert!(text(&unknown).1.contains("no execution nope"));
}
