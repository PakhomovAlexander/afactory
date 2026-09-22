//! The fix-and-re-review loop, end to end through the real binary.
//!
//! Round 1 reviews a tree with a defect and fails to converge. The operator fixes the code,
//! commits, records the resolution, and runs again. Round 2's reviewer — a script that answers
//! from the sandbox's actual content — finds nothing, and the campaign converges. This is the
//! whole fix-and-re-review loop of a heavy Campaign, with none of the model spend.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "campaign_loop/contracts.rs"]
mod contracts;
#[path = "campaign_loop/heartbeat.rs"]
mod heartbeat;
#[path = "campaign_loop/provider_admission.rs"]
mod provider_admission;
#[path = "campaign_loop/provider_currentness.rs"]
mod provider_currentness;

fn git(repo: &Path, home: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn invoke_af(
    repo: &Path,
    home: &Path,
    args: &[&str],
    default_mode: Option<&str>,
) -> (i32, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_af"));
    command
        .arg("review")
        .current_dir(repo)
        .env("HOME", home)
        .env("USER", "loop-test");
    let provider_registry = home.join(".config/af/providers.toml");
    if provider_registry.is_file() {
        command.env("AF_PROVIDERS_FILE", provider_registry);
    }
    if home.join("codex").is_file() {
        let mut paths = vec![home.to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command.env("PATH", std::env::join_paths(paths).unwrap());
    }
    let mut actual = args.to_vec();
    if actual.first() == Some(&"run") {
        let mut authority = vec!["--pipeline", PIPELINE];
        if !actual.contains(&"--policy-rev") {
            authority.extend(["--policy-rev", "HEAD"]);
        }
        if let Some(mode) = default_mode {
            authority.push(mode);
        }
        actual.splice(1..1, authority);
    }
    let out = command.args(actual).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Existing Campaign lifecycle tests exercise the explicit convergence workflow.
fn af(repo: &Path, home: &Path, args: &[&str]) -> (i32, String, String) {
    invoke_af(repo, home, args, Some("--heavy"))
}

fn af_light(repo: &Path, home: &Path, args: &[&str]) -> (i32, String, String) {
    invoke_af(repo, home, args, None)
}

/// A pipeline whose one reviewer answers from the sandbox content: a finding while the
/// defect marker is present, a clean verdict once it is gone.
const PIPELINE: &str = ".af/pipelines/review.toml";

fn pipeline_path(repo: &Path) -> PathBuf {
    repo.join(PIPELINE)
}

/// `.af/` authority is digest-pinned: every pipeline edit re-pins the lock, keeping Worker pins.
fn set_pipeline(repo: &Path, text: &str) {
    std::fs::create_dir_all(repo.join(".af/pipelines")).unwrap();
    std::fs::write(pipeline_path(repo), text).unwrap();
    repin(repo);
}

fn repin(repo: &Path) {
    let lock_path = repo.join(".af/af.lock");
    let mut lock = std::fs::read_to_string(&lock_path)
        .ok()
        .map(|text| review_config::lock::Lockfile::from_toml(&text).unwrap())
        .unwrap_or_else(review_config::lock::Lockfile::empty);
    let bytes = std::fs::read(pipeline_path(repo)).unwrap();
    lock.pipelines.insert(
        "review".into(),
        review_config::lock::Pin {
            version: "1.0.0".into(),
            digest: review_store::canonical::blob_content_id(&bytes),
        },
    );
    std::fs::write(lock_path, lock.to_toml()).unwrap();
}

fn write_review_config(repo: &Path) {
    let finding = r#"{\"findings\":[{\"severity\":\"major\",\"file\":\"src/main.rs\",\"line\":1,\"title\":\"Unbounded loop\",\"body\":\"spins\",\"fix\":\"bound it\",\"confidence\":0.9,\"rule_id\":\"test.rules/loop-safety@1\",\"occurrence_key\":\"main-loop\"}],\"benchmark_demands\":[],\"dispositions\":[$d]}"#;
    let blocker = r#"{\"findings\":[{\"severity\":\"blocker\",\"file\":\"src/main.rs\",\"line\":1,\"title\":\"Unbounded loop\",\"body\":\"spins and prevents shutdown\",\"fix\":\"bound it\",\"confidence\":0.99,\"rule_id\":\"test.rules/loop-safety@1\",\"occurrence_key\":\"main-loop\"}],\"benchmark_demands\":[],\"dispositions\":[$d]}"#;
    let demand = r#"{\"findings\":[],\"benchmark_demands\":[{\"claim\":\"the loop terminates\",\"why\":\"termination is not demonstrated\",\"suggested_method\":\"run a bounded integration test\"}],\"dispositions\":[$d]}"#;
    let clean = r#"{\"findings\":[],\"benchmark_demands\":[],\"dispositions\":[$d]}"#;
    // A committed `FAIL` marker makes the reviewer exit non-zero, so a test can produce an
    // incomplete run on demand. Absent in every other test, so it changes nothing there.
    // Every prior Finding the input assigns is answered `not_reproduced`, the disposition that
    // changes no Ledger state, so each answer covers its assignment exactly.
    let script = format!(
        "input=$(cat); \
         ids=$(printf '%s' \"$input\" | grep -o '\"finding_id\":\"sha256:[0-9a-f]*\"' | cut -d'\"' -f4 | sort -u); \
         d=; for id in $ids; do \
         d=\"$d${{d:+,}}{{\\\"finding_id\\\":\\\"$id\\\",\\\"position\\\":\\\"not_reproduced\\\",\\\"reason\\\":\\\"not re-examined by the fixture\\\"}}\"; \
         done; \
         if [ -f FAIL ]; then exit 7; fi; \
         if [ -f DEMAND ]; then printf '%s' \"{demand}\"; \
         elif [ -f BLOCKER ]; then printf '%s' \"{blocker}\"; \
         elif grep -q 'loop {{}}' src/main.rs; then printf '%s' \"{finding}\"; \
         else printf '%s' \"{clean}\"; fi"
    );
    let pipeline = format!(
        r#"version = 2

[subject]
kind = "whole-tree"

[[checks]]
name = "noop"
program = "/bin/sh"
args = [{{ value = "-c" }}, {{ value = "true" }}]

[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]

[[nodes]]
id = "generation"
kind = "generation"
outputs = [{{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }}]

[[nodes]]
id = "architecture"
kind = "reviewer"
inputs = ["gate", {{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }}]
outputs = [{{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }}]
gated_by = "gate"
[nodes.runner]
program = "/bin/sh"
args = [{{ value = "-c" }}, {{ value = '''{script}''' }}]

[[nodes]]
id = "gather"
kind = "gather"
inputs = [{{ name = "architecture", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }}]
outputs = ["reports"]

[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = [
  {{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }},
  {{ name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }},
]

[[edges]]
from = {{ node = "gate", port = "decision" }}
to = {{ node = "architecture", port = "gate" }}

[[edges]]
from = {{ node = "generation", port = "findings" }}
to = {{ node = "architecture", port = "prior_findings" }}

[[edges]]
from = {{ node = "architecture", port = "result" }}
to = {{ node = "gather", port = "architecture" }}

[[edges]]
from = {{ node = "gather", port = "reports" }}
to = {{ node = "ledger", port = "reports" }}

[convergence]
clean_rounds = 1
max_rounds = 3
gate = "major"
"#
    );
    std::fs::create_dir_all(repo.join(".af/pipelines")).unwrap();
    std::fs::write(
        repo.join(".af/af.toml"),
        "version = 1\n[project]\nname = \"fixture\"\nmin_af = \"0.6\"\n[defaults]\npipeline = \"review\"\n",
    )
    .unwrap();
    set_pipeline(repo, &pipeline);
}

fn write_disposition_config(repo: &Path) {
    let reviewer = repo.join("disposition-reviewer.sh");
    std::fs::write(
        &reviewer,
        r#"#!/bin/sh
input=$(cat)
if [ -f OMIT ]; then
  printf '%s' '{"findings":[],"benchmark_demands":[],"dispositions":[]}'
elif grep -q 'loop {}' src/main.rs; then
  printf '%s' '{"findings":[{"severity":"major","file":"src/main.rs","line":1,"title":"Unbounded loop","body":"spins","fix":"bound it","confidence":0.9}],"benchmark_demands":[],"dispositions":[]}'
else
  finding_id=$(printf '%s' "$input" | sed -n 's/.*"finding_id":"\([^"]*\)".*/\1/p')
  printf '{"findings":[],"benchmark_demands":[],"dispositions":[{"finding_id":"%s","position":"not_reproduced","reason":"the unbounded loop is absent from the current Subject"}]}' "$finding_id"
fi
"#,
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&reviewer, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    set_pipeline(
        repo,
        &format!(
            r#"version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }}]
[[nodes]]
id = "correctness"
kind = "reviewer"
inputs = [{{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }}]
outputs = [{{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }}]
runner = {{ program = "{}" }}
[[nodes]]
id = "gather"
kind = "gather"
inputs = [{{ name = "correctness", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }}]
outputs = [{{ name = "reports", type = "review.kernel/ReportSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }}]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [{{ name = "reports", type = "review.kernel/ReportSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }}]
outputs = [
  {{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }},
  {{ name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }},
]
[[edges]]
from = {{ node = "generation", port = "findings" }}
to = {{ node = "correctness", port = "prior_findings" }}
[[edges]]
from = {{ node = "correctness", port = "result" }}
to = {{ node = "gather", port = "correctness" }}
[[edges]]
from = {{ node = "gather", port = "reports" }}
to = {{ node = "ledger", port = "reports" }}
[convergence]
clean_rounds = 1
max_rounds = 3
gate = "major"
"#,
            reviewer.display()
        ),
    );
}

fn fixture(dir: &Path) -> (PathBuf, PathBuf, String) {
    let repo = dir.join("repo");
    let home = dir.join("home");
    let state = dir.join("state");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(repo.join("src/main.rs"), "fn main() { loop {} }\n").unwrap();
    write_review_config(&repo);
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "t@t.invalid"]);
    git(&repo, &home, &["config", "user.name", "T"]);
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-q", "-m", "initial"]);
    let state_flag = state.to_string_lossy().into_owned();
    (repo, home, state_flag)
}

#[test]
fn light_is_default_single_round_and_refuses_repeat_before_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());

    let (code, stdout, stderr) = af_light(
        &repo,
        &home,
        &["run", "--campaign", "light", "--state", &state],
    );
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    assert!(stdout.contains("mode     light (1 clean, 1 max Round)"));
    assert!(stdout.contains("fix the findings"), "{stdout}");
    assert!(stdout.contains("do not start another Campaign"), "{stdout}");

    let (code, stdout, stderr) = af_light(
        &repo,
        &home,
        &["run", "--campaign", "light", "--state", &state],
    );
    assert_eq!(code, 1, "{stdout}\n{stderr}");
    assert!(stderr.contains("already completed its single review Round"));
    assert!(stderr.contains("deterministic project gate"));

    let (code, report, stderr) = af(
        &repo,
        &home,
        &[
            "report",
            "--campaign",
            "light",
            "--state",
            &state,
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 0, "{report}\n{stderr}");
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["runs_recorded"], 1);

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "light", "--state", &state],
    );
    assert_eq!(code, 1, "{stdout}\n{stderr}");
    assert!(stderr.contains("differs from requested heavy mode"));
}

#[test]
fn default_light_reports_machine_readable_stop_guidance() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, _) = fixture(dir.path());
    let state = dir.path().join("default-light");
    let state = state.to_string_lossy().into_owned();

    let (code, stdout, stderr) = af_light(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "default-light",
            "--state",
            &state,
            "--json",
        ],
    );
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(outcome["campaign_mode"], "light");
    assert_eq!(outcome["next_action"]["kind"], "fix_then_gate");
    assert_eq!(outcome["next_action"]["start_another_campaign"], false);
    assert!(stderr.contains("do not start another Campaign"));
}

#[test]
fn campaign_enumeration_reads_label_named_explicit_state_and_round_history() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, _) = fixture(dir.path());
    let root = dir.path().join("campaigns");
    let state = root.join("loop").to_string_lossy().into_owned();

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 3, "round 1 must close as fail\n{stdout}\n{stderr}");

    let root = root.to_string_lossy().into_owned();
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["campaigns", "--state-root", &root, "--format", "json"],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let campaigns: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(campaigns["schema"], "af/review-campaigns@1");
    assert_eq!(campaigns["problems"], serde_json::json!([]));
    assert_eq!(campaigns["campaigns"][0]["label"], "loop");
    assert_eq!(campaigns["campaigns"][0]["subject_kind"], "whole-tree");
    assert_eq!(campaigns["campaigns"][0]["last_closed_round"], 1);
    assert_eq!(campaigns["campaigns"][0]["last_closed_epoch"], 1);
    assert_eq!(campaigns["campaigns"][0]["verdict"], "fail (not_converged)");
    assert_eq!(campaigns["campaigns"][0]["rounds"][0]["round"], 1);
    assert!(campaigns["campaigns"][0]["wall_ms"].is_u64());
    assert!(campaigns["campaigns"][0]["findings"]["open"].is_u64());
    let id = campaigns["campaigns"][0]["id"].as_str().unwrap();
    assert!(id.starts_with("c-") && id.len() == 66, "{id}");
    assert_eq!(campaigns["campaigns"][0]["state_dir"], "loop");
    assert!(
        campaigns["campaigns"][0]["last_activity_unix_ms"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        campaigns["campaigns"][0].get("state_bytes").is_none(),
        "sizes are opt-in: walking every state directory costs seconds on a large root"
    );
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "campaigns",
            "--state-root",
            &root,
            "--format",
            "json",
            "--sizes",
        ],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let sized: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(sized["campaigns"][0]["state_bytes"].as_u64().unwrap() > 0);

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["campaigns", "--state-root", &root, "--format", "text"],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("Campaigns: 1"), "{stdout}");
    assert!(stdout.contains("Problems: 0"), "{stdout}");
    assert!(
        stdout.contains("last closed: round 1 epoch 1; fail (not_converged)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("run 1: round 1 epoch 1; fail (not_converged)"),
        "{stdout}"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .args([
            "review",
            "campaigns",
            "--state-root",
            &root,
            "--format",
            "json",
        ])
        .current_dir(&repo)
        .env_remove("HOME")
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "explicit state root unexpectedly required HOME: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn final_local_review_uses_af_authority_and_one_json_result() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["--policy-rev", "HEAD", "--state", &state, "--json"],
    );

    assert_eq!(code, 3, "{stderr}");
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    contracts::valid("review-outcome-v2.json", &outcome);
    assert_eq!(outcome["schema"], "af/review-outcome@2");
    assert_eq!(outcome["campaign_mode"], "light");
    assert_eq!(outcome["next_action"]["kind"], "fix_then_gate");
    assert_eq!(outcome["next_action"]["start_another_campaign"], false);
    assert_eq!(outcome["outcome"]["kind"], "fail");
    assert_eq!(outcome["findings"].as_array().unwrap().len(), 1);
    let attempts = outcome["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["node"], "architecture");
    assert_eq!(attempts[0]["cost_tokens"], "0");
    assert!(
        attempts[0]["context_manifest"]["rendered_bytes"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > 0
    );
    assert_eq!(
        outcome["totals"]["selected_attempts"]["usage"]["chargeable_tokens"],
        "0"
    );
    assert_eq!(outcome["task"]["committed_tokens"], "0");
    assert_eq!(outcome["task"]["begun_attempts"], "2");
    assert_eq!(outcome["task"]["phase"]["kind"], "finished");
    assert_eq!(
        attempts[0]["context_manifest"]["entries"][0]["name"],
        "worker_input"
    );
    assert!(stderr.contains("authority sha256:"));
    assert!(Path::new(&state).join("events.sqlite").exists());
    assert!(!repo.join(".af/runs").exists());
}

#[test]
fn required_demands_are_visible_in_run_ledger_and_json_output() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());
    std::fs::write(repo.join("DEMAND"), b"required\n").unwrap();
    git(&repo, &home, &["add", "DEMAND"]);
    git(&repo, &home, &["commit", "-qm", "request evidence"]);

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "demand-human", "--state", &state],
    );
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    assert!(
        stdout.contains("demands  1 open/stale (required)"),
        "{stdout}"
    );
    let (code, _, ledger_err) = af(
        &repo,
        &home,
        &["ledger", "--campaign", "demand-human", "--state", &state],
    );
    assert_eq!(code, 0, "{ledger_err}");
    assert!(
        ledger_err.contains("1 required demands open/stale"),
        "{ledger_err}"
    );
    assert!(ledger_err.contains("rejected"), "{ledger_err}");
    assert!(ledger_err.contains("wall "), "{ledger_err}");

    let json_state = dir.path().join("json-state");
    let json_state = json_state.to_string_lossy().into_owned();
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "demand-json",
            "--state",
            &json_state,
            "--json",
        ],
    );
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(outcome["totals"]["open_required_demands"], "1");
    assert_eq!(
        outcome["totals"]["open_or_stale_demand_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn pipeline_policy_can_classify_reviewer_demands_as_advisory() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());
    let pipeline_path = pipeline_path(&repo);
    let pipeline = std::fs::read_to_string(&pipeline_path).unwrap();
    let advisory = pipeline.replacen(
        "id = \"architecture\"\nkind = \"reviewer\"\n",
        "id = \"architecture\"\nkind = \"reviewer\"\ndemands = \"advisory\"\n",
        1,
    );
    assert_ne!(advisory, pipeline);
    set_pipeline(&repo, &advisory);
    std::fs::write(repo.join("DEMAND"), b"advisory\n").unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-qm", "classify advisory demand"]);

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "demand-advisory", "--state", &state],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("demands  0 open/stale (required)"));
}

#[test]
fn waiver_remains_current_when_the_subject_advances() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());
    let pipeline_path = pipeline_path(&repo);
    let pipeline = std::fs::read_to_string(&pipeline_path).unwrap();
    set_pipeline(&repo, &pipeline.replace("max_rounds = 3", "max_rounds = 2"));
    std::fs::write(repo.join("DEMAND"), b"required\n").unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-qm", "request evidence"]);

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "demand-waiver",
            "--state",
            &state,
            "--json",
        ],
    );
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let demand_id = outcome["totals"]["open_or_stale_demand_ids"][0]
        .as_str()
        .unwrap();
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "demand",
            "waive",
            "--campaign",
            "demand-waiver",
            "--state",
            &state,
            demand_id,
            "--policy",
            "waiver-policy@1",
            "--reason",
            "authenticated campaign exception",
            "--actor",
            "operator",
        ],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");

    std::fs::remove_file(repo.join("DEMAND")).unwrap();
    std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-qm", "advance subject"]);
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "demand-waiver", "--state", &state],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
}

#[test]
fn trusted_reuse_keeps_evidence_satisfaction_current_after_head_change() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());
    let pipeline_path = pipeline_path(&repo);
    let pipeline = std::fs::read_to_string(&pipeline_path).unwrap();
    set_pipeline(&repo, &pipeline.replace("max_rounds = 3", "max_rounds = 2"));
    std::fs::write(repo.join("DEMAND"), b"required\n").unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(
        &repo,
        &home,
        &["commit", "-qm", "request reusable evidence"],
    );

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "demand-reuse",
            "--state",
            &state,
            "--json",
        ],
    );
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let demand_id = outcome["totals"]["open_or_stale_demand_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    let evidence_path = dir.path().join("measurement.txt");
    std::fs::write(&evidence_path, b"bounded integration test passed\n").unwrap();
    let evidence_path = evidence_path.to_string_lossy().into_owned();
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "evidence",
            "add",
            "--campaign",
            "demand-reuse",
            "--state",
            &state,
            &demand_id,
            &evidence_path,
            "--actor",
            "operator",
        ],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let evidence_id = stdout.split_whitespace().nth(1).unwrap().to_string();
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "evidence",
            "satisfy",
            "--campaign",
            "demand-reuse",
            "--state",
            &state,
            &demand_id,
            &evidence_id,
            "--policy",
            "reuse-policy@1",
            "--reason",
            "measurement is independent of source bytes",
            "--admit-reuse",
            "--actor",
            "operator",
        ],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("future-Subject Evidence reuse admitted"));

    std::fs::remove_file(repo.join("DEMAND")).unwrap();
    std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-qm", "advance subject"]);
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "demand-reuse", "--state", &state],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
}

#[test]
fn canonical_campaign_refuses_a_pipeline_without_a_demand_set_output() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());
    let pipeline_path = pipeline_path(&repo);
    let pipeline = std::fs::read_to_string(&pipeline_path).unwrap();
    let demand_port = "  { name = \"demands\", type = \"review.kernel/DemandSet@1\", cardinality = \"one\", optional = false, snapshot_affinity = \"same_subject\" },\n";
    let without_demand = pipeline.replace(demand_port, "");
    assert_ne!(without_demand, pipeline, "fixture must declare DemandSet");
    set_pipeline(&repo, &without_demand);
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-qm", "remove demand output"]);

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "missing-demand", "--state", &state],
    );
    assert_eq!(code, 1, "{stdout}\n{stderr}");
    assert!(
        stderr.contains(
            "canonical pipeline `.af/pipelines/review.toml` Ledger node must declare a review.kernel/DemandSet@1 output"
        ),
        "{stderr}"
    );
}

#[test]
fn exact_prior_set_requires_and_persists_explicit_disposition() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());
    write_disposition_config(&repo);
    git(&repo, &home, &["add", "-A"]);
    git(
        &repo,
        &home,
        &["commit", "-qm", "use explicit dispositions"],
    );

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "dispositions", "--state", &state],
    );
    assert_eq!(
        code, 3,
        "round 1 must retain the major Finding\n{stdout}\n{stderr}"
    );

    std::fs::write(repo.join("src/main.rs"), "fn main() { /* bounded */ }\n").unwrap();
    git(&repo, &home, &["commit", "-qam", "bound the loop"]);
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "dispositions", "--state", &state],
    );
    assert_eq!(
        code, 3,
        "a Drop is not trusted fixed authority\n{stdout}\n{stderr}"
    );
    assert!(stdout.contains("round    2"), "{stdout}");
    assert!(!stdout.contains("Incomplete"), "{stdout}");

    let state = Path::new(&state);
    let cas = review_store::Cas::open(state.join("cas")).unwrap();
    let store = review_store::EventStore::open(state.join("events.sqlite")).unwrap();
    let events = store.replay("campaign-dispositions").unwrap();
    let ledger_receipt = events
        .iter()
        .rev()
        .find(|event| {
            event.event_type == review_core::EventType::NodeOutputReceiptV1
                && event.node_id.as_deref() == Some("ledger")
        })
        .expect("Round 2 ledger receipt");
    let receipt: review_core::NodeOutputReceiptPayloadV1 =
        serde_json::from_value(ledger_receipt.payload.clone()).unwrap();
    let set_record_id = &receipt.outputs[0].artifact_ids[0];
    let set_envelope: review_core::ArtifactEnvelope =
        serde_json::from_value(cas.get_json(set_record_id).unwrap()).unwrap();
    let set: review_core::FindingSetV1 =
        serde_json::from_value(set_envelope.payload.clone()).unwrap();
    assert_eq!(set.round, 2);
    assert_eq!(set.reducer_version, review_core::FINDING_REDUCER_VERSION_V2);
    assert_eq!(set.relation_ids.len(), 1);
    assert_eq!(set.findings[0].status, "open");

    let disposition_envelope = set_envelope
        .input_artifacts
        .iter()
        .filter_map(|id| cas.get_json(id).ok())
        .filter_map(|value| serde_json::from_value::<review_core::ArtifactEnvelope>(value).ok())
        .find(|envelope| envelope.artifact_type == review_core::contract::FINDING_DISPOSITION_V1)
        .expect("immutable disposition reducer input");
    assert_eq!(disposition_envelope.artifact_id, set.relation_ids[0]);
    let disposition: review_core::FindingDispositionV1 =
        serde_json::from_value(disposition_envelope.payload).unwrap();
    assert_eq!(
        disposition.position,
        review_core::FindingDispositionPosition::NotReproduced
    );
    assert_eq!(disposition.round, 2);
    assert_eq!(disposition.subject_id, set.subject_id);

    let invocation = events
        .iter()
        .rev()
        .find(|event| {
            event.event_type == review_core::EventType::NodeInvocationV1
                && event.node_id.as_deref() == Some("correctness")
        })
        .expect("Round 2 reviewer invocation");
    let invocation_event_id = invocation.event_id.clone();
    let invocation: review_core::NodeInvocationPayloadV1 =
        serde_json::from_value(invocation.payload.clone()).unwrap();
    let assigned = invocation
        .inputs
        .iter()
        .find(|port| port.artifact_type == review_core::contract::FINDING_SET_V1)
        .expect("exact prior FindingSet@1 input");
    assert_eq!(
        assigned.artifact_ids,
        vec![set.prior_finding_set_id.clone()]
    );

    let selected = events
        .iter()
        .rev()
        .find(|event| {
            event.event_type == review_core::EventType::TaskReviewResultSelectedV1
                && event.node_id.as_deref() == Some("correctness")
        })
        .expect("Round 2 common Reviewer selection");
    let selected: review_core::task::review_compat::TaskReviewResultSelectedV1 =
        serde_json::from_value(selected.payload.clone()).unwrap();
    let context: review_core::task::review_compat::TaskReviewContextV1 =
        serde_json::from_value(cas.get_artifact(&selected.context_id).unwrap().payload).unwrap();
    assert_eq!(
        context.invocation_event_id, invocation_event_id,
        "common Attempt context must bind the canonical invocation with the exact assigned FindingSet"
    );
    assert_eq!(context.task_invocation_id, selected.invocation_id);
    let frame = cas.get_artifact(&selected.context_id).unwrap();
    assert!(frame.input_artifacts.contains(&context.reviewer_inputs_id));
    assert!(frame.input_artifacts.contains(&context.rendered_input_id));
    let task = store
        .task_projection(&cas, &selected.task_id)
        .unwrap()
        .unwrap();
    assert!(
        task.execution
            .unwrap()
            .attempt_accounting()
            .iter()
            .any(|attempt| attempt.attempt_id == context.attempt_id && attempt.started)
    );
}

#[test]
fn missing_disposition_coverage_makes_the_round_structurally_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());
    write_disposition_config(&repo);
    git(&repo, &home, &["add", "-A"]);
    git(
        &repo,
        &home,
        &["commit", "-qm", "use explicit dispositions"],
    );

    let (code, ..) = af(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "missing-disposition",
            "--state",
            &state,
        ],
    );
    assert_eq!(code, 3);
    std::fs::write(repo.join("src/main.rs"), "fn main() { /* bounded */ }\n").unwrap();
    std::fs::write(repo.join("OMIT"), "force silence\n").unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(
        &repo,
        &home,
        &["commit", "-qm", "omit required disposition"],
    );

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "missing-disposition",
            "--state",
            &state,
        ],
    );
    assert_eq!(
        code, 4,
        "missing semantic output must be incomplete\n{stdout}\n{stderr}"
    );

    let state = Path::new(&state);
    let store = review_store::EventStore::open(state.join("events.sqlite")).unwrap();
    let report = store
        .replay("campaign-missing-disposition")
        .unwrap()
        .into_iter()
        .rev()
        .find(|event| event.event_type == review_core::EventType::RunReportV6)
        .expect("durable incomplete Task-backed RunReport@6");
    let report: review_core::RunReportPayloadV6 = serde_json::from_value(report.payload).unwrap();
    let review_core::RunVerdictV3::Incomplete { missing_nodes } = report.verdict else {
        panic!("missing dispositions did not produce an incomplete verdict");
    };
    let correctness = missing_nodes
        .iter()
        .find(|missing| missing.node == "correctness")
        .expect("structured missing correctness output");
    assert!(
        correctness.reason.contains("MissingDispositionCoverage"),
        "{}",
        correctness.reason
    );
}

#[test]
fn a_campaign_converges_after_a_scoped_nonfixed_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());

    // Round 1: the defect is found; the campaign must not converge.
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 3, "round 1 must fail to converge\n{stdout}\n{stderr}");
    assert!(stdout.contains("round    1"), "{stdout}");
    assert!(stdout.contains("Unbounded loop"), "{stdout}");

    // The operator reads the ledger and takes the finding's key.
    let (code, ledger_out, _) = af(
        &repo,
        &home,
        &["ledger", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 0);
    let row = ledger_out
        .lines()
        .find(|l| l.contains("Unbounded loop"))
        .expect("the finding is in the ledger");
    assert!(row.contains("\tmajor\topen\t"), "{row}");
    let key = row.split('\t').next().unwrap().to_string();

    let (code, long_out, long_err) = af(
        &repo,
        &home,
        &["ledger", "--campaign", "loop", "--state", &state, "--long"],
    );
    assert_eq!(code, 0, "{long_out}\n{long_err}");
    assert!(long_out.contains("body: spins"), "{long_out}");
    assert!(long_out.contains("fix: bound it"), "{long_out}");

    let (code, show_out, show_err) = af(
        &repo,
        &home,
        &["show", "--campaign", "loop", "--state", &state, &key],
    );
    assert_eq!(code, 0, "{show_out}\n{show_err}");
    assert!(
        show_out.contains("reviewer=architecture round=1"),
        "{show_out}"
    );
    assert!(show_out.contains(r#""fix": "bound it""#), "{show_out}");
    assert!(show_out.contains("Reported"), "{show_out}");

    let (code, report_out, report_err) = af(
        &repo,
        &home,
        &[
            "report",
            "--campaign",
            "loop",
            "--state",
            &state,
            "--format",
            "md",
        ],
    );
    assert_eq!(code, 0, "{report_out}\n{report_err}");
    assert!(
        report_out.contains("# Review campaign `loop`"),
        "{report_out}"
    );
    assert!(report_out.contains("Fix: bound it"), "{report_out}");
    assert!(report_out.contains("## Task accounting:"), "{report_out}");
    assert!(!report_out.contains("## Spend"), "{report_out}");
    assert!(report_out.contains("architecture"), "{report_out}");

    let (code, report_json, report_err) = af(
        &repo,
        &home,
        &[
            "report",
            "--campaign",
            "loop",
            "--state",
            &state,
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 0, "{report_json}\n{report_err}");
    let report: serde_json::Value = serde_json::from_str(&report_json).unwrap();
    assert_eq!(report["schema"], "af/review-report@3");
    assert_eq!(report["rounds"][0]["round"], 1);
    assert!(report.get("spend").is_none(), "{report_json}");
    let task = &report["task_accounting"][0];
    assert_eq!(task["attempts_started"], "2");
    assert_eq!(task["chargeable_tokens"], "0");
    let attempt = task["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|attempt| attempt["review_node"] == "architecture")
        .unwrap();
    assert_eq!(attempt["outcome"], "selected");
    // The common wall-clock sidecar remains observational and is never execution authority.
    let wall = &attempt["wall"];
    assert!(wall["elapsed_ms"].is_u64(), "{wall}");
    assert!(wall["started_unix_ms"].as_u64().unwrap() > 0, "{wall}");
    assert_eq!(wall["usage"]["chargeable_tokens"], "0", "{wall}");
    assert!(task["wall_ms"].is_u64(), "{task}");
    assert!(report["wall_ms"].is_u64());
    assert!(report["findings_summary"]["open"].is_u64());

    let (code, report_text, report_err) = af(
        &repo,
        &home,
        &[
            "report",
            "--campaign",
            "loop",
            "--state",
            &state,
            "--format",
            "text",
        ],
    );
    assert_eq!(code, 0, "{report_text}\n{report_err}");
    assert!(
        report_text.contains("Review campaign: loop"),
        "{report_text}"
    );
    assert!(report_text.contains("architecture:"), "{report_text}");
    assert!(report_text.contains("Attempt"), "{report_text}");

    // Change the code, then record an authenticated, scoped non-fixed disposition. The separate
    // attestation/verification path is covered by the canonical projection tests.
    std::fs::write(repo.join("src/main.rs"), "fn main() { /* bounded */ }\n").unwrap();
    git(&repo, &home, &["commit", "-qam", "bound the loop"]);
    let (code, resolve_out, resolve_err) = af(
        &repo,
        &home,
        &[
            "resolve",
            "--campaign",
            "loop",
            "--state",
            &state,
            &key,
            "rejected",
            "--policy",
            "test-policy@1",
            "--reason",
            "operator rejected the original claim after inspecting src/main.rs",
        ],
    );
    assert_eq!(code, 0, "{resolve_out}\n{resolve_err}");
    assert!(resolve_out.contains("-> rejected"), "{resolve_out}");
    let (code, duplicate_out, duplicate_err) = af(
        &repo,
        &home,
        &[
            "resolve",
            "--campaign",
            "loop",
            "--state",
            &state,
            &key,
            "rejected",
            "--policy",
            "test-policy@1",
            "--reason",
            "operator rejected the original claim after inspecting src/main.rs",
        ],
    );
    assert_eq!(code, 0, "{duplicate_out}\n{duplicate_err}");
    assert_eq!(duplicate_out, resolve_out, "exact duplicate is idempotent");

    // Round 2: prior findings travel to the reviewer; the clean round converges.
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 0, "round 2 must converge\n{stdout}\n{stderr}");
    assert!(stdout.contains("round    2"), "{stdout}");
    assert!(!stdout.contains("findings carried"), "{stdout}");
    assert!(stdout.contains("verdict  Pass"), "{stdout}");

    // The Ledger's final state remains the exact scoped operator decision.
    let (_, ledger_out, ledger_err) = af(
        &repo,
        &home,
        &["ledger", "--campaign", "loop", "--state", &state],
    );
    assert!(ledger_out.contains("\trejected\t"), "{ledger_out}");
    assert!(ledger_err.contains("0 open"), "{ledger_err}");

    let (code, report_out, report_err) = af(
        &repo,
        &home,
        &["report", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 0, "{report_out}\n{report_err}");
    assert!(report_out.contains("Final verdict: pass"), "{report_out}");
    assert!(
        report_out.contains("operator rejected the original claim"),
        "{report_out}"
    );
    assert!(report_out.contains("## Task accounting:"), "{report_out}");
    assert!(report_out.contains("Round 1 epoch 1"), "{report_out}");
    assert!(report_out.contains("Round 2 epoch 1"), "{report_out}");
    assert_eq!(
        report_out.matches("architecture: Attempt ").count(),
        2,
        "{report_out}"
    );
}

#[test]
fn a_whole_tree_campaign_reaches_fixed_only_through_attestation_and_verification() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "verified-fix", "--state", &state],
    );
    assert_eq!(code, 3, "round 1 must find the defect\n{stdout}\n{stderr}");
    let (_, ledger_out, _) = af(
        &repo,
        &home,
        &["ledger", "--campaign", "verified-fix", "--state", &state],
    );
    let key = ledger_out.split('\t').next().unwrap().to_string();

    std::fs::write(repo.join("src/main.rs"), "fn main() { /* bounded */ }\n").unwrap();
    git(&repo, &home, &["commit", "-qam", "bound the loop"]);

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "verified-fix", "--state", &state],
    );
    assert_eq!(
        code, 3,
        "the clean reviewer result alone cannot fix the prior claim\n{stdout}\n{stderr}"
    );

    let (code, attestation_out, attestation_err) = af(
        &repo,
        &home,
        &[
            "attest-change",
            "--campaign",
            "verified-fix",
            "--state",
            &state,
            &key,
            "--region",
            "src/main.rs:1-1",
            "--reason",
            "bounded the loop in the active whole-tree Snapshot",
        ],
    );
    assert_eq!(code, 0, "{attestation_out}\n{attestation_err}");
    assert!(
        attestation_out.contains("pending-verification"),
        "{attestation_out}"
    );
    let attestation_id = attestation_out
        .split_once('(')
        .and_then(|(_, tail)| tail.trim().strip_suffix(')'))
        .expect("attestation artifact ID");

    let (code, verification_out, verification_err) = af(
        &repo,
        &home,
        &[
            "verify-fix",
            "--campaign",
            "verified-fix",
            "--state",
            &state,
            &key,
            attestation_id,
            "--positive",
            "--policy",
            "fix-policy@1",
            "--reason",
            "the current whole-tree Snapshot no longer contains the unbounded loop",
        ],
    );
    assert_eq!(code, 0, "{verification_out}\n{verification_err}");
    assert!(verification_out.contains("-> fixed"), "{verification_out}");

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "verified-fix", "--state", &state],
    );
    assert_eq!(
        code, 0,
        "verified fixed state must converge\n{stdout}\n{stderr}"
    );
    assert!(stdout.contains("round    3"), "{stdout}");
    assert!(stdout.contains("verdict  Pass"), "{stdout}");

    let (_, ledger_out, ledger_err) = af(
        &repo,
        &home,
        &["ledger", "--campaign", "verified-fix", "--state", &state],
    );
    assert!(ledger_out.contains("\tfixed\t"), "{ledger_out}");
    assert!(ledger_err.contains("0 open"), "{ledger_err}");
}

#[test]
fn a_report_above_a_tracked_wontfix_ceiling_is_explicitly_challenged() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "wontfix-ceiling", "--state", &state],
    );
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    let (_, ledger_out, _) = af(
        &repo,
        &home,
        &["ledger", "--campaign", "wontfix-ceiling", "--state", &state],
    );
    let key = ledger_out.split('\t').next().unwrap().to_string();
    let (code, policy_out, policy_err) = af(
        &repo,
        &home,
        &[
            "policy-time",
            "advance",
            "--campaign",
            "wontfix-ceiling",
            "--state",
            &state,
            "50",
            "--reason",
            "set the deterministic test clock",
        ],
    );
    assert_eq!(code, 0, "{policy_out}\n{policy_err}");
    let (code, _, expired_err) = af(
        &repo,
        &home,
        &[
            "resolve",
            "--campaign",
            "wontfix-ceiling",
            "--state",
            &state,
            &key,
            "wontfix-tracked",
            "--policy",
            "risk-policy@1",
            "--reason",
            "already expired exception",
            "--max-severity",
            "major",
            "--tracking",
            "ISSUE-EXPIRED",
            "--expires-at-policy-time",
            "50",
        ],
    );
    assert_eq!(code, 1);
    assert!(
        expired_err.contains("persisted policy time 50"),
        "{expired_err}"
    );
    let (code, resolve_out, resolve_err) = af(
        &repo,
        &home,
        &[
            "resolve",
            "--campaign",
            "wontfix-ceiling",
            "--state",
            &state,
            &key,
            "wontfix-tracked",
            "--policy",
            "risk-policy@1",
            "--reason",
            "temporarily accept only the current major risk",
            "--max-severity",
            "major",
            "--tracking",
            "ISSUE-42",
            "--expires-at-policy-time",
            "99",
        ],
    );
    assert_eq!(code, 0, "{resolve_out}\n{resolve_err}");
    assert!(resolve_out.contains("-> wontfix"), "{resolve_out}");

    std::fs::write(repo.join("BLOCKER"), "escalate the stable occurrence\n").unwrap();
    git(&repo, &home, &["add", "BLOCKER"]);
    git(&repo, &home, &["commit", "-qm", "escalate finding"]);
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "wontfix-ceiling", "--state", &state],
    );
    assert_eq!(
        code, 3,
        "an above-ceiling claim must block\n{stdout}\n{stderr}"
    );

    let (_, ledger_out, _) = af(
        &repo,
        &home,
        &["ledger", "--campaign", "wontfix-ceiling", "--state", &state],
    );
    assert!(
        ledger_out.contains("\tblocker\tcontested\t"),
        "{ledger_out}"
    );
    let store = review_store::EventStore::open(Path::new(&state).join("events.sqlite")).unwrap();
    assert!(
        store
            .replay("campaign-wontfix-ceiling")
            .unwrap()
            .iter()
            .any(|event| event.event_type == review_core::EventType::FindingResolutionChallengedV1),
        "the higher-severity report must emit an explicit challenge artifact/event"
    );
}

/// A "fix" that does not actually fix reopens the finding, and the campaign refuses to pass.
#[test]
fn a_direct_fixed_assertion_is_refused_and_the_claim_remains_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());

    let (code, ..) = af(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 3);
    let (_, ledger_out, _) = af(
        &repo,
        &home,
        &["ledger", "--campaign", "loop", "--state", &state],
    );
    let key = ledger_out.split('\t').next().unwrap().to_string();

    // A bare operator assertion cannot make the Finding fixed.
    let (code, _, stderr) = af(
        &repo,
        &home,
        &[
            "resolve",
            "--campaign",
            "loop",
            "--state",
            &state,
            &key,
            "fixed",
            "--policy",
            "test-policy@1",
            "--reason",
            "unsupported bare assertion",
        ],
    );
    assert_eq!(code, 1);
    assert!(stderr.contains("attest-change"), "{stderr}");

    // Round 2 re-finds it: reopened, and the run must not pass.
    let (code, stdout, _) = af(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 3, "a hollow resolution must not converge\n{stdout}");
    let (_, ledger_out, _) = af(
        &repo,
        &home,
        &["ledger", "--campaign", "loop", "--state", &state],
    );
    assert!(ledger_out.contains("\topen\t"), "reopened: {ledger_out}");
}

/// Bugbot High: an incomplete run (a crash, a failed reviewer, an exit-4 run) must not consume
/// a campaign round. Only a run that closed on a real verdict advances the generation.
#[test]
fn an_incomplete_run_does_not_burn_a_round() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());

    // Round 1, forced incomplete: the reviewer exits non-zero, so gather/ledger are suppressed.
    std::fs::write(repo.join("FAIL"), b"x").unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-qm", "force an incomplete run"]);
    let (code, stdout, _) = af(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(
        code, 4,
        "a suppressed reviewer makes the run incomplete\n{stdout}"
    );
    assert!(stdout.contains("round    1"), "{stdout}");

    // Removing the marker does not silently change an incomplete Round's immutable input.
    std::fs::remove_file(repo.join("FAIL")).unwrap();
    git(&repo, &home, &["commit", "-qam", "let the reviewer run"]);
    let (code, stdout, _) = af(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(
        code, 4,
        "the original Subject still contains FAIL\n{stdout}"
    );
    assert!(
        stdout.contains("snapshot") && stdout.contains("(reused)"),
        "{stdout}"
    );

    // Explicit supersession captures the changed head under a new epoch of the same Round.
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "loop",
            "--state",
            &state,
            "--restart-round",
        ],
    );
    assert_eq!(
        code, 3,
        "the defect is found; not converged\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("round    1"),
        "the incomplete run must not have burned round 1:\n{stdout}"
    );

    // Now that a round has closed, the next run advances.
    let (_, stdout, _) = af(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert!(
        stdout.contains("round    2"),
        "a closed round advances:\n{stdout}"
    );
}

/// Bugbot Medium: a declined finding (rejected / wontfix) is the operator's terminal decision
/// and the ledger never reopens it — so it must not be packaged back to reviewers.
#[test]
fn a_declined_finding_is_not_sent_back_to_reviewers() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());

    // Round 1 finds the defect.
    let (code, ..) = af(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 3);
    let (_, ledger_out, _) = af(
        &repo,
        &home,
        &["ledger", "--campaign", "loop", "--state", &state],
    );
    let key = ledger_out.split('\t').next().unwrap().to_string();

    // The operator rejects it (disagrees with the finding).
    let (code, ..) = af(
        &repo,
        &home,
        &[
            "resolve",
            "--campaign",
            "loop",
            "--state",
            &state,
            &key,
            "rejected",
            "--policy",
            "test-policy@1",
            "--reason",
            "operator rejects the claim",
        ],
    );
    assert_eq!(code, 0);

    // Round 2: the only finding is declined, so nothing is carried back to the reviewers.
    let (_, stdout, _) = af(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert!(
        !stdout.contains("findings carried"),
        "a rejected finding must not be sent back:\n{stdout}"
    );
}

fn native_diff_fixture(directory: &Path) -> (PathBuf, PathBuf, String) {
    let (repo, home, state) = fixture(directory);
    let codex = home.join("codex");
    std::fs::write(
        &codex,
        r#"#!/bin/sh
if [ "$1" = "app-server" ]; then
  while IFS= read -r request; do
    case "$request" in
      *'"id":1'*) printf '%s\n' '{"id":1,"result":{}}' ;;
      *'"id":2'*) printf '%s\n' '{"id":2,"result":{"account":{"type":"chatgpt","email":"fixture@example.test"}}}' ;;
    esac
  done
  exit 0
fi
if [ "$1" = "login" ]; then
  printf '%s\n' 'Logged in using ChatGPT' >&2
  exit 0
fi
out=
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then out=$2; shift 2; else shift; fi
done
input=$(cat)
if [ -n "$out" ] && [ "$input" = 'Reply with exactly: OK' ]; then
  printf '%s' 'OK' >"$out"
elif [ -n "$out" ]; then
  printf '%s' '{"findings":[],"benchmark_demands":[],"dispositions":[]}' >"$out"
fi
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"OK"}}'
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1}}'
"#,
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let auth_dir = home.join("codex-auth");
    std::fs::create_dir_all(&auth_dir).unwrap();
    let provider_registry = home.join(".config/af/providers.toml");
    std::fs::create_dir_all(provider_registry.parent().unwrap()).unwrap();
    std::fs::write(
        &provider_registry,
        format!(
            "version = 1\n[[providers]]\nid = \"test-codex\"\nkind = \"codex\"\nauth_dir = \"{}\"\n",
            auth_dir.display()
        ),
    )
    .unwrap();
    let reviewers = repo.join(".af/workers");
    let package = reviewers.join("tester");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("reviewer.toml"),
        format!(
            r#"name = "tester"
version = "1.0.0"
subjects = ["diff"]

[runner]
program = "{}"
args = [{{ value = "--model" }}, {{ value = "gpt-fixture-1" }}, {{ value = "-c" }}, {{ value = 'model_reasoning_effort="high"' }}]
"#,
            codex.display()
        ),
    )
    .unwrap();
    std::fs::write(
        package.join("reviewer.md"),
        "Review the exact Change Set.\n",
    )
    .unwrap();
    let registry = review_config::lock::Registry::new(&reviewers);
    let mut lockfile = review_config::lock::Lockfile::empty();
    lockfile.workers.insert(
        "tester".into(),
        review_config::lock::Lockfile::pin("tester", &registry).unwrap(),
    );
    std::fs::write(repo.join(".af/af.lock"), lockfile.to_toml()).unwrap();
    let pipeline = r#"
version = 2
[subject]
kind = "diff"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [
  { name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" },
  { name = "change_set", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
[[nodes]]
id = "reviewer"
kind = "reviewer"
package = "tester"
inputs = [
  { name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" },
  { name = "change_set", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[nodes]]
id = "gather"
kind = "gather"
inputs = [{ name = "reviewer", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [{ name = "reports", type = "review.kernel/ReportSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [{ name = "reports", type = "review.kernel/ReportSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [
  { name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "reviewer", port = "prior_findings" }
[[edges]]
from = { node = "generation", port = "change_set" }
to = { node = "reviewer", port = "change_set" }
[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "gather", port = "reviewer" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
[convergence]
clean_rounds = 1
max_rounds = 2
gate = "major"
"#
    .to_string();
    set_pipeline(&repo, &pipeline);
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-qm", "declare diff subject"]);

    (repo, home, state)
}

#[test]
fn committed_and_dirty_diff_subjects_execute_the_wired_change_set() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = native_diff_fixture(dir.path());
    let codex = home.join("codex");
    let provider_registry = home.join(".config/af/providers.toml");

    let (code, plan_stdout, plan_stderr) = invoke_af(
        &repo,
        &home,
        &[
            "plan",
            "--pipeline",
            PIPELINE,
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD^",
            "--candidate",
            "HEAD",
            "--provider",
            "reviewer=test-codex",
            "--json",
        ],
        None,
    );
    assert_eq!(code, 0, "{plan_stdout}\n{plan_stderr}");
    let plan: serde_json::Value = serde_json::from_str(&plan_stdout).unwrap();
    assert_eq!(plan["schema"], "af/review-plan@1");
    assert_eq!(plan["token_free"], true);
    assert_eq!(plan["subject"]["empty"], false);
    assert!(
        !plan["subject"]["changed_paths"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(plan["external_effects"]["campaign_state"], false);
    assert_eq!(plan["providers"][0]["ready"], true);

    let (code, empty_stdout, empty_stderr) = invoke_af(
        &repo,
        &home,
        &[
            "plan",
            "--pipeline",
            PIPELINE,
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD",
            "--candidate",
            "HEAD",
            "--json",
        ],
        None,
    );
    assert_eq!(code, 0, "{empty_stdout}\n{empty_stderr}");
    let empty: serde_json::Value = serde_json::from_str(&empty_stdout).unwrap();
    assert_eq!(empty["subject"]["empty"], true);

    let (code, empty_run_stdout, empty_run_stderr) = af(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "empty-diff",
            "--state",
            &state,
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD",
            "--candidate",
            "HEAD",
            "--provider",
            "reviewer=test-codex",
        ],
    );
    assert_eq!(code, 1, "{empty_run_stdout}\n{empty_run_stderr}");
    assert!(
        empty_run_stderr.contains("refusing empty Diff before Gates, Provider admission"),
        "{empty_run_stderr}"
    );

    let (code, missing_stdout, missing_stderr) = af(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "missing-provider",
            "--state",
            &state,
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD^",
            "--candidate",
            "HEAD",
        ],
    );
    assert_eq!(code, 1, "{missing_stdout}\n{missing_stderr}");
    assert!(
        missing_stderr.contains("requires explicit `--provider reviewer=PROVIDER_ID`"),
        "{missing_stderr}"
    );

    let run_doctor = |campaign: &str| {
        Command::new(env!("CARGO_BIN_EXE_af"))
            .args([
                "provider",
                "doctor",
                "--repo",
                repo.to_str().unwrap(),
                "--pipeline",
                PIPELINE,
                "--campaign",
                campaign,
                "--state",
                &state,
                "--heavy",
                "--policy-rev",
                "HEAD",
                "--base",
                "HEAD^",
                "--candidate",
                "HEAD",
                "--provider",
                "reviewer=test-codex",
                "--json",
            ])
            .env("PATH", {
                let mut paths = vec![home.clone()];
                paths.extend(std::env::split_paths(
                    &std::env::var_os("PATH").unwrap_or_default(),
                ));
                std::env::join_paths(paths).unwrap()
            })
            .env("HOME", &home)
            .env("USER", "loop-test")
            .env("AF_PROVIDERS_FILE", &provider_registry)
            .output()
            .unwrap()
    };
    let doctor = run_doctor("diff");
    assert!(
        doctor.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    contracts::valid("provider-doctor-v2.json", &doctor);
    assert_eq!(doctor["schema"], "af/provider-doctor@2");
    assert_eq!(doctor["task"]["committed_tokens"], "2");
    assert_eq!(doctor["task"]["begun_attempts"], "1");
    let doctor_task_id = doctor["task"]["task_id"].as_str().unwrap().to_string();
    {
        let cas = review_store::Cas::open(Path::new(&state).join("cas")).unwrap();
        let store =
            review_store::EventStore::open(Path::new(&state).join("events.sqlite")).unwrap();
        let task = store
            .task_projection(&cas, &doctor_task_id)
            .unwrap()
            .unwrap();
        assert!(
            task.run_reports.is_empty(),
            "Provider doctor must not fabricate a business RunReport"
        );
        assert!(!matches!(
            task.phase,
            review_core::task::TaskPhaseV1::Finished { .. }
        ));
        assert!(
            store
                .replay("campaign-diff")
                .unwrap()
                .iter()
                .all(|event| !matches!(
                    event.event_type,
                    review_core::EventType::TaskReviewResultSelectedV1
                        | review_core::EventType::RunReportV6
                ))
        );
    }
    assert_eq!(doctor["ready"], true);
    assert_eq!(doctor["gates_run"], false);
    assert_eq!(doctor["workers_dispatched"], false);

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "diff",
            "--state",
            &state,
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD^",
            "--candidate",
            "HEAD",
            "--provider",
            "reviewer=test-codex",
        ],
    );

    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("done      generation"), "{stdout}");
    {
        let cas = review_store::Cas::open(Path::new(&state).join("cas")).unwrap();
        let store =
            review_store::EventStore::open(Path::new(&state).join("events.sqlite")).unwrap();
        let task = store
            .task_projection(&cas, &doctor_task_id)
            .unwrap()
            .unwrap();
        let execution = task.execution.unwrap();
        assert_eq!(
            execution.budget.begun_attempts(),
            2,
            "one Provider probe followed by one Reviewer; doctor admission is reused"
        );
        assert_eq!(execution.budget.committed_tokens(), 4);
        assert!(matches!(
            task.phase,
            review_core::task::TaskPhaseV1::Finished { .. }
        ));
    }

    std::fs::write(repo.join("dirty.txt"), b"not committed\n").unwrap();
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "diff-dirty",
            "--state",
            &state,
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD",
            "--uncommitted",
            "--provider",
            "reviewer=test-codex",
        ],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("done      reviewer"), "{stdout}");

    // A failed capability probe still returns exactly one typed document and keeps
    // the original charged Attempt when doctor is repeated for the same Campaign.
    let native_script = std::fs::read_to_string(&codex).unwrap();
    let failing_script = native_script.replace(
        "printf '%s' 'OK' >\"$out\"",
        "printf '%s' 'not-ready' >\"$out\"",
    );
    assert_ne!(native_script, failing_script);
    std::fs::write(&codex, failing_script).unwrap();
    let mut failed_task_id = None;
    for _ in 0..2 {
        let output = run_doctor("diff-probe-failed");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let failed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        contracts::valid("provider-doctor-v2.json", &failed);
        assert_eq!(failed["schema"], "af/provider-doctor@2");
        assert_eq!(failed["ready"], false);
        assert_eq!(failed["gates_run"], false);
        assert_eq!(failed["workers_dispatched"], false);
        assert_eq!(failed["task"]["begun_attempts"], "1");
        assert_eq!(failed["task"]["committed_tokens"], "2");
        let id = failed["task"]["task_id"].as_str().unwrap().to_string();
        if let Some(previous) = &failed_task_id {
            assert_eq!(&id, previous);
        }
        failed_task_id = Some(id.clone());
        let cas = review_store::Cas::open_existing(Path::new(&state).join("cas")).unwrap();
        let store =
            review_store::EventStore::open_read_only(Path::new(&state).join("events.sqlite"))
                .unwrap();
        let task = store.task_projection(&cas, &id).unwrap().unwrap();
        assert!(task.run_reports.is_empty());
        assert!(!matches!(
            task.phase,
            review_core::task::TaskPhaseV1::Finished { .. }
        ));
    }
}

/// `gc` previews by default and removes only whole Campaign directories on `--apply`; what it
/// kept is still listed, what it removed is gone from the enumeration.
#[test]
fn gc_previews_then_removes_whole_campaign_directories() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, _) = fixture(dir.path());
    let root = dir.path().join("campaigns");
    let state = root.join("loop").to_string_lossy().into_owned();
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    let root = root.to_string_lossy().into_owned();

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["gc", "--state-root", &root, "--older-than", "0", "--json"],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let preview: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(preview["schema"], "af/review-gc@1");
    assert_eq!(preview["applied"], false);
    assert_eq!(preview["campaigns"][0]["label"], "loop");
    assert_eq!(preview["campaigns"][0]["action"], "would remove");
    assert!(preview["reclaimable_bytes"].as_u64().unwrap() > 0);
    assert_eq!(preview["reclaimed_bytes"], 0);
    assert!(
        std::path::Path::new(&state).exists(),
        "a preview removes nothing"
    );

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "gc",
            "--state-root",
            &root,
            "--older-than",
            "0",
            "--keep",
            "1",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let kept: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        kept["campaigns"][0]["action"], "keep",
        "--keep protects the newest"
    );

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["gc", "--state-root", &root, "--older-than", "0", "--apply"],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("removed"), "{stdout}");
    assert!(
        !std::path::Path::new(&state).exists(),
        "--apply removes the directory"
    );

    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &["campaigns", "--state-root", &root, "--format", "json"],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let campaigns: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(campaigns["campaigns"], serde_json::json!([]));
}
