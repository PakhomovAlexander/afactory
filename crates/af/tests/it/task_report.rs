//! Report Tasks (ADR-0133). The shipped `builtin/report` starter runs a command author that
//! reads the committed tree in a clone that seals nothing back and cites two paths and one
//! `path:line`; the kernel renders the draft, resolves every citation against the exact source
//! Manifest, and dispatches the independent command verifier only when those checks pass. No
//! credential, Provider or model is involved. Every case edits the starter the way a developer
//! would, re-pins its packages and commits, so the kernel captures the edit as authority.

use review_core::task::report_task::*;
use review_store::Cas;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[allow(dead_code)] // Only `copy_tree` is used: every repository here is built by the starter.
use crate::task_cli;

use crate::schemas;

const TASK: &str = "starter-report";

fn af(repo: &Path, args: &[&str]) -> Output {
    crate::common::af()
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", text(&output));
}

/// The starter, initialized as a repository and committed. Returns the repository and a state
/// directory beside it.
fn starter(root: &Path) -> (PathBuf, PathBuf) {
    let init = af(
        root,
        &[
            "catalog",
            "init",
            "--profile",
            "report",
            "--destination",
            "project",
            "--json",
        ],
    );
    assert!(init.status.success(), "{}", text(&init));
    let init: Value = serde_json::from_slice(&init.stdout).unwrap();
    assert_eq!(init["task"], "report.json");
    assert_eq!(init["attempts"], 0);
    let repo = root.join("project");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "report starter"]);
    (repo, root.join("state"))
}

/// Replace `from` with `to` in one starter Worker's script, re-pin every package and commit.
fn edit_worker(repo: &Path, worker: &str, from: &str, to: &str) {
    let path = package(repo, &format!("builtin/{worker}")).join("worker.py");
    let script = std::fs::read_to_string(&path).unwrap();
    assert!(script.contains(from), "{worker}: {from}");
    std::fs::write(&path, script.replace(from, to)).unwrap();
    repin_and_commit(repo);
}

/// The directory the committed catalog pins `name` to.
fn package(repo: &Path, name: &str) -> PathBuf {
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    repo.join(catalog["packages"][name]["path"].as_str().unwrap())
}

/// One root input port of the Task's recorded revision.
fn revision_input(cas: &Cas, done: &Value, port: &str) -> Value {
    let revision = cas.get_json(done["revision_id"].as_str().unwrap()).unwrap();
    revision["payload"]["inputs"][port].clone()
}

fn repin_and_commit(repo: &Path) {
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
        pin["digest"] = toml::Value::String(
            review_config::lock::package_digest(name, &repo.join(pin["path"].as_str().unwrap()))
                .unwrap(),
        );
    }
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "report variant"]);
}

/// Run one `af task` command with `--json --state`, expecting `code`.
fn run(repo: &Path, state: &Path, args: &[&str], code: i32) -> Value {
    let output = crate::common::af()
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .args(["--json", "--state"])
        .arg(state)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(code), "{}", text(&output));
    serde_json::from_slice(&output.stdout).unwrap()
}

fn start(repo: &Path, state: &Path, code: i32) -> Value {
    let done = run(
        repo,
        state,
        &["task", "start", "--execute", "--file", "report.json"],
        code,
    );
    schemas::valid(&schemas::validator("task-inspection-v11.json"), &done);
    done
}

fn payload(cas: &Cas, id: &str, ty: &str) -> Value {
    let envelope = cas.get_json(id).unwrap();
    assert_eq!(envelope["type"], ty, "{envelope}");
    envelope["payload"].clone()
}

/// The report's public acceptance receipt and the check receipt it names.
fn receipts(cas: &Cas, done: &Value) -> (ReportVerificationV1, ReportCheckReceiptV1) {
    let id = done["result"]["outputs"]["verification"]["artifact_ids"][0]
        .as_str()
        .unwrap_or_else(|| panic!("no verification: {}", done["result"]));
    let value = payload(cas, id, REPORT_VERIFICATION_V1);
    schemas::valid(&schemas::validator("report-verification-v1.json"), &value);
    let verification: ReportVerificationV1 = serde_json::from_value(value).unwrap();
    let value = payload(cas, &verification.check_receipt_id, REPORT_CHECK_RECEIPT_V1);
    schemas::valid(&schemas::validator("report-check-receipt-v1.json"), &value);
    (verification, serde_json::from_value(value).unwrap())
}

/// The rendered context manifest of the one Attempt reserved for `node`.
fn context(cas: &Cas, done: &Value, node: &str) -> Value {
    let records: Vec<&Value> = done["execution_records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| &entry["record"])
        .collect();
    let reserved = records
        .iter()
        .find(|record| {
            record["kind"] == "reserved"
                && cas
                    .get_json(record["invocation_id"].as_str().unwrap())
                    .unwrap()["payload"]["node"]
                    == node
        })
        .unwrap_or_else(|| panic!("no Attempt for {node}"));
    let bound = records
        .iter()
        .find(|record| {
            record["kind"] == "context_bound" && record["attempt_id"] == reserved["attempt_id"]
        })
        .unwrap();
    cas.get_json(bound["context_id"].as_str().unwrap()).unwrap()["payload"].clone()
}

fn manifest_ports(context: &Value) -> Vec<String> {
    let mut ports: Vec<String> = context["manifest"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["required_by"] == "declared input port")
        .map(|entry| entry["name"].as_str().unwrap().to_owned())
        .collect();
    ports.sort();
    ports
}

/// The diagnostic message of `node` in the Task's last run report, when it failed.
fn diagnostic(cas: &Cas, done: &Value, node: &str) -> Option<String> {
    let report = done["run_reports"].as_array()?.last()?;
    let entry = report["report"]["nodes"]
        .as_array()?
        .iter()
        .find(|entry| entry["node"] == node)?;
    let id = entry["outcome"]["diagnostic_id"].as_str()?;
    Some(
        cas.get_json(id).unwrap()["payload"]["message"]
            .as_str()?
            .to_owned(),
    )
}

fn node_outcome(done: &Value, node: &str) -> String {
    let report = done["run_reports"].as_array().unwrap().last().unwrap();
    report["report"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["node"] == node)
        .map(|entry| entry["outcome"]["kind"].as_str().unwrap().to_string())
        .unwrap_or_else(|| "absent".into())
}

#[test]
fn the_report_starter_runs_to_verified_with_three_command_attempts_and_writes_its_markdown() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (repo, state) = starter(&root);
    for (schema, path) in [
        ("task-file-v1.json", "report.json"),
        ("report-sources-v1.json", "sources.json"),
    ] {
        let value: Value =
            serde_json::from_slice(&std::fs::read(repo.join(path)).unwrap()).unwrap();
        schemas::valid(&schemas::validator(schema), &value);
    }
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    schemas::valid(
        &schemas::validator("task-catalog-v2.json"),
        &serde_json::to_value(&catalog).unwrap(),
    );
    let policy: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/report-policy.toml")).unwrap())
            .unwrap();
    schemas::valid(
        &schemas::validator("report-task-policy-v1.json"),
        &serde_json::to_value(&policy).unwrap(),
    );
    let tested = af(&repo, &["catalog", "test", "--source", ".", "--json"]);
    assert!(tested.status.success(), "{}", text(&tested));
    let tested: Value = serde_json::from_slice(&tested.stdout).unwrap();
    assert_eq!(tested["contract_fixtures"], "passed");
    assert_eq!(tested["attempts"], 0);

    let planned = run(&repo, &state, &["task", "plan", "--file", "report.json"], 0);
    assert_eq!(planned["attempts"], 0);
    let inputs: Vec<&String> = planned["plan"]["inputs"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(inputs, ["requirements", "source", "sources"]);
    assert_eq!(
        planned["plan"]["authority"]["allowed_effects"],
        json!(["execute-checks", "read-source"])
    );
    let done = run(&repo, &state, &["task", "run", "--execute", TASK], 0);
    assert_eq!(done["attempts"], 3, "author, checks and verifier");
    assert_eq!(done["result"]["acceptance"], "satisfied");
    assert_eq!(done["result"]["domain_conclusion"], "verified");
    assert!(done["result"]["outputs"]["snapshot"].is_null());

    // Every receipt names the one source Snapshot the report was written and checked against.
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let snapshot = revision_input(&cas, &done, "source")["snapshot_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for port in ["report", "verification"] {
        assert_eq!(done["result"]["outputs"][port]["snapshot_id"], snapshot);
    }
    let (verification, checks) = receipts(&cas, &done);
    assert_eq!(verification.source_snapshot_id, snapshot);
    assert_eq!(checks.source_snapshot_id, snapshot);
    assert!(checks.citation_failures.is_empty());
    assert!(
        checks
            .checks
            .values()
            .all(|outcome| *outcome == review_core::task::pipeline::ReceiptOutcomeV1::Passed)
    );
    let recorded: Value = cas.get_json(&snapshot).unwrap();
    assert_eq!(checks.manifest_id, recorded["manifest_id"]);
    let evaluation = payload(
        &cas,
        verification.evaluation_id.as_deref().unwrap(),
        REPORT_EVALUATION_V1,
    );
    schemas::valid(
        &schemas::validator("report-evaluation-v1.json"),
        &evaluation,
    );
    assert_eq!(evaluation["source_snapshot_id"], snapshot);
    assert_eq!(evaluation["outcome"], "passed");

    // The report cites two paths and one `path:line`, each resolved against that Manifest.
    let document_id = done["result"]["outputs"]["report"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let document = cas.get_json(document_id).unwrap();
    assert_eq!(document["type"], "af/Document@1");
    assert_eq!(document["subject_snapshot_id"], snapshot);
    let draft = payload(
        &cas,
        document["payload"]["draft_id"].as_str().unwrap(),
        "af/DocumentDraft@2",
    );
    schemas::valid(&schemas::validator("document-draft-v2.json"), &draft);
    let lines = std::fs::read_to_string(repo.join("README.md"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(
        draft["repository_citations"],
        json!([{"path":"README.md"},{"path":"README.md","line":lines},{"path":"report.json"}])
    );
    let report = document["payload"]["text"].as_str().unwrap();
    assert!(report.ends_with(&format!(
        "## Repository citations\n\n- `README.md`\n\n- `README.md:{lines}`\n\n- `report.json`\n"
    )));
    let file = root.join("report.md");
    let written = run(
        &repo,
        &state,
        &[
            "task",
            "output",
            TASK,
            "--port",
            "report",
            "--format",
            "markdown",
            "--output",
            file.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(written["artifact_id"], document_id);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), report);

    // Absent `comparison` and `measurements` reach no Worker: the author and the verifier see
    // exactly the root inputs the Task captured, and the verifier the report and its checks.
    assert_eq!(
        manifest_ports(&context(&cas, &done, "root.nodes.author")),
        ["requirements", "source", "sources"]
    );
    assert_eq!(
        manifest_ports(&context(&cas, &done, "root.nodes.verify")),
        ["checks", "document", "requirements", "source", "sources"]
    );

    let shown = af(
        &repo,
        &["task", "show", TASK, "--state", state.to_str().unwrap()],
    );
    assert!(shown.status.success(), "{}", text(&shown));
    let shown = String::from_utf8_lossy(&shown.stdout);
    for line in [
        "report: Starter layout".to_string(),
        "report verifier: passed".to_string(),
        format!("report Snapshot: {snapshot}"),
    ] {
        assert!(shown.contains(&line), "{shown}");
    }

    // A satisfied report is never delivered: the refusal names its only exit and nothing in
    // Git moves.
    let worktree = root.join("worktree");
    let refused = af(
        &repo,
        &[
            "task",
            "deliver",
            TASK,
            "--branch",
            "agent/report",
            "--worktree",
            worktree.to_str().unwrap(),
            "--confirm",
            TASK,
            "--state",
            state.to_str().unwrap(),
        ],
    );
    assert!(!refused.status.success());
    let message = text(&refused);
    assert!(message.contains("report Task"), "{message}");
    assert!(
        message.contains("af task output starter-report --port report"),
        "{message}"
    );
    assert!(!worktree.exists());
    let branches = Command::new("git")
        .current_dir(&repo)
        .args(["branch", "--list"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&branches.stdout).trim(), "* main");
    // Replay spends nothing and reports the same result.
    assert_eq!(
        run(&repo, &state, &["task", "run", "--execute", TASK], 0),
        done
    );
}

#[test]
fn a_citation_that_does_not_name_a_text_line_fails_report_check_before_the_verifier() {
    use ReportCitationFailureReasonV1::*;
    for (case, citation, reason) in [
        ("absent", "{'path': 'MISSING.md'}", Absent),
        ("directory", "{'path': '.af'}", Directory),
        ("symlink", "{'path': 'alias.md'}", Symlink),
        ("binary", "{'path': 'blob.bin'}", Binary),
        (
            "line",
            "{'path': 'report.json', 'line': 100000}",
            LineOutOfRange,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = starter(root.path());
        std::os::unix::fs::symlink("README.md", repo.join("alias.md")).unwrap();
        std::fs::write(repo.join("blob.bin"), b"head\0tail\n").unwrap();
        edit_worker(
            &repo,
            "report-author",
            "        {'path': 'report.json'},\n",
            &format!("        {citation},\n        {{'path': 'report.json'}},\n"),
        );
        let done = start(&repo, &state, 3);
        assert_eq!(done["result"]["acceptance"], "unsatisfied", "{case}");
        assert_eq!(
            done["attempts"], 2,
            "{case}: author and checks, never the verifier"
        );
        let cas = Cas::open_existing(state.join("cas")).unwrap();
        let (verification, checks) = receipts(&cas, &done);
        assert!(verification.evaluation_id.is_none(), "{case}");
        assert_eq!(
            checks.checks["repository_citations"],
            review_core::task::pipeline::ReceiptOutcomeV1::Failed,
            "{case}"
        );
        assert_eq!(checks.citation_failures.len(), 1, "{case}");
        assert_eq!(checks.citation_failures[0].reason, reason, "{case}");
        assert_eq!(
            node_outcome(&done, "root.nodes.verify"),
            "suppressed",
            "{case}"
        );
        assert_eq!(
            run(&repo, &state, &["task", "run", "--execute", TASK], 3),
            done,
            "{case}"
        );
    }
}

#[test]
fn a_report_task_without_report_sources_reads_the_empty_set() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = starter(root.path());
    let path = repo.join("report.json");
    let mut task: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    task.as_object_mut().unwrap().remove("report_sources");
    std::fs::write(&path, serde_json::to_vec_pretty(&task).unwrap()).unwrap();
    git(&repo, &["commit", "-qam", "no sources"]);
    let done = start(&repo, &state, 0);
    assert_eq!(done["result"]["acceptance"], "satisfied");
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let sources = revision_input(&cas, &done, "sources");
    let id = sources["artifact_ids"][0].as_str().unwrap();
    let sources = payload(&cas, id, REPORT_SOURCES_V1);
    assert_eq!(
        sources,
        json!({"schema":"af.document-sources/1","sources":{}})
    );
    let (_, checks) = receipts(&cas, &done);
    assert_eq!(checks.sources_id, id);
}

#[test]
fn a_sources_file_over_its_bound_is_refused_at_capture() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = starter(root.path());
    let entry = |n: usize| {
        (
            format!("task-{n}"),
            json!({"title":"Task","uri":"repo:README.md","revision":"r1","text":"x".repeat(MAX_REPORT_SOURCE_BYTES)}),
        )
    };
    // Three full entries: each fits, the file and the total do not.
    let sources: serde_json::Map<String, Value> = (0..3).map(entry).collect();
    std::fs::write(
        repo.join("sources.json"),
        serde_json::to_vec(&json!({"schema":"af.document-sources/1","sources":sources})).unwrap(),
    )
    .unwrap();
    git(&repo, &["commit", "-qam", "oversized sources"]);
    let refused = af(
        &repo,
        &[
            "task",
            "plan",
            "--file",
            "report.json",
            "--state",
            state.to_str().unwrap(),
        ],
    );
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    let message = text(&refused);
    assert!(message.contains("sources.json"), "{message}");
    assert!(message.contains("KiB bound"), "{message}");
    // Refused before a Task existed: nothing was recorded under its ID.
    let shown = af(
        &repo,
        &["task", "show", TASK, "--state", state.to_str().unwrap()],
    );
    assert!(!shown.status.success());
}

#[test]
fn a_negative_verdict_stays_unsatisfied() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = starter(root.path());
    edit_worker(
        &repo,
        "report-verifier",
        "'passed' if accepted else 'failed'",
        "'failed'",
    );
    let done = start(&repo, &state, 3);
    assert_eq!(done["attempts"], 3);
    assert_eq!(done["result"]["acceptance"], "unsatisfied");
    assert_eq!(done["result"]["domain_conclusion"], "changes_requested");
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let (verification, _) = receipts(&cas, &done);
    assert_eq!(
        verification.outcome,
        review_core::task::pipeline::ReceiptOutcomeV1::Failed
    );
    assert_eq!(
        run(&repo, &state, &["task", "run", "--execute", TASK], 3),
        done
    );
    let shown = af(
        &repo,
        &["task", "show", TASK, "--state", state.to_str().unwrap()],
    );
    assert!(String::from_utf8_lossy(&shown.stdout).contains("report verifier: failed"));
}

#[test]
fn a_verifier_evaluation_that_names_another_snapshot_is_refused_at_output_admission() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = starter(root.path());
    edit_worker(
        &repo,
        "report-verifier",
        "'source_snapshot_id': snapshot,",
        "'source_snapshot_id': 'sha256:' + '0' * 64,",
    );
    let done = start(&repo, &state, 4);
    assert_ne!(done["result"]["acceptance"], "satisfied");
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let message = diagnostic(&cas, &done, "root.nodes.verify").unwrap();
    assert!(
        message.contains("names source Snapshot sha256:000"),
        "{message}"
    );
    let (verification, _) = receipts(&cas, &done);
    assert!(verification.evaluation_id.is_none());
}

#[test]
fn an_author_may_add_scratch_but_an_author_that_edits_its_source_fails_its_attempt() {
    // Scratch the author's shell leaves is discarded with the clone: the source seals unchanged.
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = starter(root.path());
    edit_worker(
        &repo,
        "report-author",
        "import json, sys\n",
        "import json, os, sys\nos.makedirs('target', exist_ok=True)\nopen('target/scratch.txt', 'w').write('x')\n",
    );
    let done = start(&repo, &state, 0);
    assert_eq!(done["result"]["acceptance"], "satisfied");

    let root = tempfile::tempdir().unwrap();
    let (repo, state) = starter(root.path());
    edit_worker(
        &repo,
        "report-author",
        "import json, sys\n",
        "import json, sys\nopen('README.md', 'a').write('edited\\n')\n",
    );
    let done = start(&repo, &state, 4);
    assert_eq!(done["attempts"], 1);
    assert_ne!(done["result"]["acceptance"], "satisfied");
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let message = diagnostic(&cas, &done, "root.nodes.author").unwrap();
    assert!(
        message.contains("Execute-checks reviewer changed its declared source: README.md"),
        "{message}"
    );
}

#[test]
fn a_report_author_that_writes_source_is_refused_at_planning() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = starter(root.path());
    let path = package(&repo, "builtin/report-author").join("worker.toml");
    let mut worker: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    worker["signature"]["effects"] =
        toml::Value::try_from(["execute-checks", "read-source", "write-source"]).unwrap();
    std::fs::write(&path, toml::to_string(&worker).unwrap()).unwrap();
    repin_and_commit(&repo);
    let refused = af(
        &repo,
        &[
            "task",
            "plan",
            "--file",
            "report.json",
            "--state",
            state.to_str().unwrap(),
        ],
    );
    assert!(!refused.status.success(), "{}", text(&refused));
    let message = text(&refused);
    assert!(
        message.contains("does not permit worker/builtin/report-author"),
        "{message}"
    );
}

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

const KERNEL_PACKAGES: [(&str, Option<&str>); 3] = [
    ("kernel/report", None),
    ("kernel/analyst", Some("claude-main")),
    ("kernel/report-verifier", Some("codex-main")),
];

/// Perform the install steps of `fixtures/kernel-report/README.md` on `repo`'s `.af/`, or —
/// where a human has already taken a step there — verify that what they installed is what is
/// staged. Every staged pin is the digest of the staged bytes.
fn install_kernel_report(repo: &Path) {
    let staged_root = repo.join("fixtures/kernel-report");
    let staged: toml::Value =
        toml::from_str(&std::fs::read_to_string(staged_root.join("catalog.toml")).unwrap())
            .unwrap();
    let policy = repo.join(".af/report-policy.toml");
    let staged_policy = std::fs::read(staged_root.join("report-policy.toml")).unwrap();
    if policy.exists() {
        assert_eq!(
            std::fs::read(&policy).unwrap(),
            staged_policy,
            "installed policy"
        );
    } else {
        std::fs::write(&policy, &staged_policy).unwrap();
    }
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    let table = catalog.as_table_mut().unwrap();
    match table.get("report_policy") {
        Some(installed) => assert_eq!(installed.as_str(), Some(".af/report-policy.toml")),
        None => {
            table.insert(
                "report_policy".into(),
                toml::Value::String(".af/report-policy.toml".into()),
            );
        }
    }
    for (name, provider) in KERNEL_PACKAGES {
        let mut pin = staged["packages"][name].clone();
        assert_eq!(
            pin["digest"].as_str().unwrap(),
            review_config::lock::package_digest(name, &repo.join(pin["path"].as_str().unwrap()))
                .unwrap(),
            "{name}: the staged pin is the staged bytes"
        );
        let path = format!(".af/task-packages/{name}");
        if !repo.join(&path).is_dir() {
            task_cli::copy_tree(&repo.join(pin["path"].as_str().unwrap()), &repo.join(&path));
        }
        pin["path"] = toml::Value::String(path);
        let packages = catalog["packages"].as_table_mut().unwrap();
        match packages.get(name) {
            Some(installed) => assert_eq!(
                installed["digest"], pin["digest"],
                "{name}: the committed pin is the staged package"
            ),
            None => {
                packages.insert(name.into(), pin);
            }
        }
        if let Some(provider) = provider {
            let providers = catalog["providers"].as_table_mut().unwrap();
            providers
                .entry(name)
                .or_insert_with(|| toml::Value::String(provider.into()));
        }
    }
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
}

/// A copy of this repository's `.af/` and staged fixture, installed and committed.
fn kernel_repository(root: &Path) -> PathBuf {
    let repo = root.join("kernel");
    task_cli::copy_tree(&workspace().join(".af"), &repo.join(".af"));
    // The release pin would dispatch to (and install) another `af`; this test runs this one.
    std::fs::remove_file(repo.join(".af/af.lock")).unwrap();
    task_cli::copy_tree(
        &workspace().join("fixtures/kernel-report"),
        &repo.join("fixtures/kernel-report"),
    );
    std::fs::write(repo.join("README.md"), "# Kernel\n\nA fixture copy.\n").unwrap();
    install_kernel_report(&repo);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "kernel report"]);
    repo
}

/// A local command Worker replacing `package` with its exact committed contract and schemas:
/// only the runner and the model-token reservation differ, so no Provider is needed to plan.
fn local_replacement(repo: &Path, directory: &Path, package: &str, local: &str) -> toml::Value {
    let target = directory.join(local);
    task_cli::copy_tree(&repo.join(".af/task-packages").join(package), &target);
    let manifest = target.join("worker.toml");
    let mut worker: toml::Value =
        toml::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    worker["name"] = toml::Value::String(local.into());
    worker["runner"] = toml::from_str(
        "kind = \"command\"\n[command]\nprogram = \"/usr/bin/python3\"\n\
         [[command.args]]\nvalue = \"@package/worker.py\"\nprovenance = \"literal\"\n",
    )
    .unwrap();
    worker["signature"]["attempt"]["tokens"] = toml::Value::Integer(0);
    std::fs::write(&manifest, toml::to_string(&worker).unwrap()).unwrap();
    std::fs::write(target.join("worker.py"), "raise SystemExit(1)\n").unwrap();
    let mut pin = toml::map::Map::new();
    pin.insert("version".into(), worker["version"].clone());
    pin.insert(
        "digest".into(),
        toml::Value::String(review_config::lock::package_digest(local, &target).unwrap()),
    );
    pin.insert("path".into(), toml::Value::String(local.into()));
    toml::Value::Table(pin)
}

#[test]
fn this_repositorys_report_pipeline_installs_idempotently_passes_its_catalog_test_and_plans() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let repo = kernel_repository(&root);
    // Installing again over an installed `.af/` verifies and changes nothing.
    let catalog = std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap();
    install_kernel_report(&repo);
    assert_eq!(
        std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap(),
        catalog
    );
    let status = Command::new("git")
        .current_dir(&repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(status.stdout.is_empty(), "{}", text(&status));
    let installed: toml::Value = toml::from_str(&catalog).unwrap();
    assert_eq!(
        installed["providers"]["kernel/analyst"],
        "claude-main".into()
    );
    assert_eq!(
        installed["providers"]["kernel/report-verifier"],
        "codex-main".into()
    );

    let tested = af(
        &repo,
        &[
            "catalog",
            "test",
            "--source",
            ".",
            "--manifest",
            "fixtures/kernel-report/catalog.toml",
            "--json",
        ],
    );
    assert!(tested.status.success(), "{}", text(&tested));
    let tested: Value = serde_json::from_slice(&tested.stdout).unwrap();
    assert_eq!(tested["contract_fixtures"], "passed");
    assert_eq!(tested["pipelines"], json!(["kernel/report"]));
    assert_eq!(
        tested["workers"],
        json!(["kernel/analyst", "kernel/report-verifier"])
    );

    // The staged Workers are what the plan names: the analyst reads source with a shell, the
    // verifier only reads. Local command stand-ins with the exact committed contracts replace
    // both model Workers, so planning needs no Provider.
    let worker = |name: &str| -> toml::Value {
        toml::from_str(
            &std::fs::read_to_string(
                repo.join(".af/task-packages")
                    .join(name)
                    .join("worker.toml"),
            )
            .unwrap(),
        )
        .unwrap()
    };
    let analyst = worker("kernel/analyst");
    assert_eq!(
        analyst["signature"]["effects"],
        toml::Value::try_from(["execute-checks", "read-source"]).unwrap()
    );
    assert_eq!(analyst["runner"]["model"], "claude-opus-5-5".into());
    assert_eq!(analyst["signature"]["attempt"]["tokens"], 1_500_000.into());
    assert_eq!(
        analyst["signature"]["attempt"]["wall_ms"],
        10_800_000.into()
    );
    let verifier = worker("kernel/report-verifier");
    assert_eq!(
        verifier["signature"]["effects"],
        toml::Value::try_from(["read-source"]).unwrap()
    );
    assert_eq!(verifier["runner"]["model"], "gpt-6-sol".into());
    let local = root.join("local");
    std::fs::create_dir_all(&local).unwrap();
    let mut packages = toml::map::Map::new();
    for (package, name) in [
        ("kernel/analyst", "local/analyst"),
        ("kernel/report-verifier", "local/report-verifier"),
    ] {
        packages.insert(name.into(), local_replacement(&repo, &local, package, name));
    }
    let mut bindings = toml::map::Map::new();
    bindings.insert(
        "schema".into(),
        toml::Value::String("af.task-bindings/1".into()),
    );
    bindings.insert("packages".into(), toml::Value::Table(packages));
    bindings.insert(
        "slots".into(),
        toml::from_str(
            "\"root.slots.author\" = \"local/analyst\"\n\
             \"root.slots.verifier\" = \"local/report-verifier\"\n",
        )
        .unwrap(),
    );
    let bindings_path = local.join("bindings.toml");
    std::fs::write(&bindings_path, toml::to_string(&bindings).unwrap()).unwrap();
    std::fs::create_dir_all(root.join("tasks")).unwrap();
    let file = root.join("tasks/cycle.json");
    std::fs::write(
        &file,
        serde_json::to_vec(&json!({
            "schema": "af.task-file/1",
            "task_id": "cycle-report",
            "kind": "report",
            "goal": "Report where the development cycle's time and disk go.",
            "pipeline": {"name": "kernel/report", "fallback": "refuse"},
            "strategy": "small",
            "facts": {},
            "limits": {
                "tokens": 2_000_000,
                "max_attempts": 3,
                "wall_ms": 14_400_000,
                "verification": {"tokens": 400_000, "attempts": 2, "wall_ms": 1_900_000}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let state = root.join("state");
    let planned = af(
        &repo,
        &[
            "task",
            "plan",
            "--file",
            file.to_str().unwrap(),
            "--bindings",
            bindings_path.to_str().unwrap(),
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(planned.status.code(), Some(0), "{}", text(&planned));
    let planned: Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert!(planned["plan_id"].as_str().is_some(), "{planned}");
    assert_eq!(planned["attempts"], 0);
    assert_eq!(
        planned["plan"]["authority"]["allowed_effects"],
        json!(["execute-checks", "read-source"])
    );
}

#[test]
fn a_task_kind_package_maps_another_business_kind_to_the_report_profile() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = starter(root.path());
    let kind = repo.join(".af/packages/research-kind");
    std::fs::create_dir_all(&kind).unwrap();
    std::fs::write(
        kind.join("kind.toml"),
        "schema = \"af.task-kind/1\"\nname = \"fixture/research-kind\"\nversion = \"1.0.0\"\n\
         kind = \"research\"\nprofile = \"report\"\n",
    )
    .unwrap();
    let pipeline = package(&repo, "builtin/report").join("pipeline.toml");
    let text = std::fs::read_to_string(&pipeline).unwrap();
    assert!(text.contains("kinds = [\"report\"]"));
    std::fs::write(
        &pipeline,
        text.replace("kinds = [\"report\"]", "kinds = [\"research\"]"),
    )
    .unwrap();
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    catalog["packages"].as_table_mut().unwrap().insert(
        "fixture/research-kind".into(),
        toml::from_str(
            "version = \"1.0.0\"\ndigest = \"sha256:0\"\npath = \".af/packages/research-kind\"\n",
        )
        .unwrap(),
    );
    catalog.as_table_mut().unwrap().insert(
        "kinds".into(),
        toml::from_str("research = \"fixture/research-kind\"\n").unwrap(),
    );
    std::fs::write(&path, toml::to_string(&catalog).unwrap()).unwrap();
    let file = repo.join("report.json");
    let mut task: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    task["kind"] = json!("research");
    std::fs::write(&file, serde_json::to_vec_pretty(&task).unwrap()).unwrap();
    repin_and_commit(&repo);
    let done = start(&repo, &state, 0);
    assert_eq!(done["attempts"], 3);
    assert_eq!(done["result"]["acceptance"], "satisfied");
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let revision = cas.get_json(done["revision_id"].as_str().unwrap()).unwrap();
    assert_eq!(revision["payload"]["kind"], "research");
    assert_eq!(
        revision["payload"]["required_outputs"]["verification"]["artifact_type"],
        REPORT_VERIFICATION_V1
    );
}
