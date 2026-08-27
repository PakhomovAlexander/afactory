//! The fix-and-re-review loop, end to end through the real binary.
//!
//! Round 1 reviews a tree with a defect and fails to converge. The operator fixes the code,
//! commits, records the resolution, and runs again. Round 2's reviewer — a script that answers
//! from the sandbox's actual content — finds nothing, and the campaign converges. This is the
//! whole loop `/self-review-heavy` drives, with none of the model spend.

use std::path::{Path, PathBuf};
use std::process::Command;

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

fn reviewctl(repo: &Path, home: &Path, args: &[&str]) -> (i32, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_af"));
    command
        .arg("review")
        .current_dir(repo)
        .env("HOME", home)
        .env("USER", "loop-test");
    let mut actual = args.to_vec();
    if actual.first() == Some(&"run") {
        actual.splice(
            1..1,
            [
                "--authority",
                "HEAD",
                "--pipeline",
                ".review/pipelines/heavy.toml",
            ],
        );
    }
    let out = command.args(actual).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A pipeline whose one reviewer answers from the sandbox content: a finding while the
/// defect marker is present, a clean verdict once it is gone.
fn write_review_config(repo: &Path) {
    std::fs::create_dir_all(repo.join(".review/pipelines")).unwrap();
    std::fs::write(repo.join(".review/review.lock"), "version = 1\n").unwrap();
    let finding = r#"{\"verdict\":\"request-changes\",\"summary\":null,\"findings\":[{\"severity\":\"major\",\"file\":\"src/main.rs\",\"line\":1,\"title\":\"Unbounded loop\",\"body\":\"spins\",\"fix\":\"bound it\",\"confidence\":0.9}],\"benchmark_demands\":[],\"disputes\":[]}"#;
    let clean = r#"{\"verdict\":\"approve\",\"summary\":null,\"findings\":[],\"benchmark_demands\":[],\"disputes\":[]}"#;
    // A committed `FAIL` marker makes the reviewer exit non-zero, so a test can produce an
    // incomplete run on demand. Absent in every other test, so it changes nothing there.
    let script = format!(
        "if [ -f FAIL ]; then exit 7; fi; \
         if grep -q 'loop {{}}' src/main.rs; then printf '%s' \"{finding}\"; \
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
id = "architecture"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
[nodes.runner]
program = "/bin/sh"
args = [{{ value = "-c" }}, {{ value = '''{script}''' }}]

[[nodes]]
id = "gather"
kind = "gather"
inputs = ["architecture"]
outputs = ["reports"]

[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["findings"]

[[edges]]
from = {{ node = "gate", port = "decision" }}
to = {{ node = "architecture", port = "gate" }}

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
    std::fs::write(repo.join(".review/pipelines/heavy.toml"), pipeline).unwrap();

    std::fs::create_dir_all(repo.join(".af/pipelines")).unwrap();
    std::fs::write(
        repo.join(".af/af.toml"),
        "version = 1\n[defaults]\npipeline = \"review\"\n",
    )
    .unwrap();
    std::fs::copy(
        repo.join(".review/pipelines/heavy.toml"),
        repo.join(".af/pipelines/review.toml"),
    )
    .unwrap();
    let pipeline = std::fs::read(repo.join(".af/pipelines/review.toml")).unwrap();
    std::fs::write(
        repo.join(".af/af.lock"),
        format!(
            "version = 1\n[pipelines.review]\nversion = \"1.0.0\"\ndigest = \"{}\"\n",
            review_store::canonical::blob_content_id(&pipeline)
        ),
    )
    .unwrap();
}

fn write_disposition_config(repo: &Path) {
    let reviewer = repo.join("disposition-reviewer.sh");
    std::fs::write(
        &reviewer,
        r#"#!/bin/sh
input=$(cat)
if [ -f OMIT ]; then
  printf '%s' '{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"dispositions":[]}'
elif grep -q 'loop {}' src/main.rs; then
  printf '%s' '{"verdict":"request-changes","summary":null,"findings":[{"severity":"major","file":"src/main.rs","line":1,"title":"Unbounded loop","body":"spins","fix":"bound it","confidence":0.9}],"benchmark_demands":[],"dispositions":[]}'
else
  finding_id=$(printf '%s' "$input" | sed -n 's/.*"finding_id":"\([^"]*\)".*/\1/p')
  printf '{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"dispositions":[{"finding_id":"%s","position":"not_reproduced","reason":"the unbounded loop is absent from the current Subject"}]}' "$finding_id"
fi
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&reviewer, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        repo.join(".review/pipelines/heavy.toml"),
        format!(
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
outputs = [{{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }}]
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
    )
    .unwrap();
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
fn final_local_review_uses_af_authority_and_one_json_result() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());
    let (code, stdout, stderr) = reviewctl(
        &repo,
        &home,
        &["--authority", "HEAD", "--state", &state, "--json"],
    );

    assert_eq!(code, 3, "{stderr}");
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(outcome["schema"], "af/review-outcome@1");
    assert_eq!(outcome["outcome"]["kind"], "fail");
    assert_eq!(outcome["findings"].as_array().unwrap().len(), 1);
    let attempts = outcome["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["node"], "architecture");
    assert_eq!(attempts[0]["cost_tokens"], 0);
    assert!(
        attempts[0]["context_manifest"]["rendered_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(outcome["totals"]["usage"]["chargeable_tokens"], 0);
    assert_eq!(
        attempts[0]["context_manifest"]["entries"][0]["name"],
        "worker_input"
    );
    assert!(stderr.contains("authority sha256:"));
    assert!(Path::new(&state).join("events.sqlite").exists());
    assert!(!repo.join(".af/runs").exists());
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

    let (code, stdout, stderr) = reviewctl(
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
    let (code, stdout, stderr) = reviewctl(
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
    let invocation: review_core::NodeInvocationPayloadV1 =
        serde_json::from_value(invocation.payload.clone()).unwrap();
    let assigned = invocation
        .inputs
        .iter()
        .find(|port| port.artifact_type == review_core::contract::FINDING_SET_V1)
        .expect("exact prior FindingSet@1 input");
    assert_eq!(assigned.artifact_ids, vec![set.prior_finding_set_id]);
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

    let (code, ..) = reviewctl(
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

    let (code, stdout, stderr) = reviewctl(
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
        .find(|event| event.event_type == review_core::EventType::RunReportV3)
        .expect("durable incomplete RunReport@3");
    let report: review_core::RunReportPayloadV3 = serde_json::from_value(report.payload).unwrap();
    let review_core::RunVerdictV3::Incomplete { missing_nodes } = report.verdict else {
        panic!("missing dispositions did not produce an incomplete verdict");
    };
    let correctness = missing_nodes
        .iter()
        .find(|missing| missing.node == "correctness")
        .expect("structured missing correctness output");
    assert!(
        correctness.reason.contains("missing_disposition_coverage"),
        "{}",
        correctness.reason
    );
}

#[test]
fn a_campaign_converges_after_the_fix_survives_review() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());

    // Round 1: the defect is found; the campaign must not converge.
    let (code, stdout, stderr) = reviewctl(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 3, "round 1 must fail to converge\n{stdout}\n{stderr}");
    assert!(stdout.contains("round    1"), "{stdout}");
    assert!(stdout.contains("Unbounded loop"), "{stdout}");

    // The operator reads the ledger and takes the finding's key.
    let (code, ledger_out, _) = reviewctl(
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

    let (code, long_out, long_err) = reviewctl(
        &repo,
        &home,
        &["ledger", "--campaign", "loop", "--state", &state, "--long"],
    );
    assert_eq!(code, 0, "{long_out}\n{long_err}");
    assert!(long_out.contains("body: spins"), "{long_out}");
    assert!(long_out.contains("fix: bound it"), "{long_out}");

    let (code, show_out, show_err) = reviewctl(
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

    let (code, report_out, report_err) = reviewctl(
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

    // Fix, commit, record the disposition.
    std::fs::write(repo.join("src/main.rs"), "fn main() { /* bounded */ }\n").unwrap();
    git(&repo, &home, &["commit", "-qam", "bound the loop"]);
    let (code, resolve_out, resolve_err) = reviewctl(
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
            "--note",
            "bounded in src/main.rs",
        ],
    );
    assert_eq!(code, 0, "{resolve_out}\n{resolve_err}");
    assert!(resolve_out.contains("-> fixed"), "{resolve_out}");

    // Round 2: prior findings travel to the reviewer; the clean round converges.
    let (code, stdout, stderr) = reviewctl(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 0, "round 2 must converge\n{stdout}\n{stderr}");
    assert!(stdout.contains("round    2"), "{stdout}");
    assert!(stdout.contains("prior    1 findings carried"), "{stdout}");
    assert!(stdout.contains("verdict  Pass"), "{stdout}");

    // The ledger's final state: the finding stayed fixed, nothing reopened.
    let (_, ledger_out, ledger_err) = reviewctl(
        &repo,
        &home,
        &["ledger", "--campaign", "loop", "--state", &state],
    );
    assert!(ledger_out.contains("\tfixed\t"), "{ledger_out}");
    assert!(ledger_err.contains("0 open"), "{ledger_err}");

    let (_, report_out, report_err) = reviewctl(
        &repo,
        &home,
        &["report", "--campaign", "loop", "--state", &state],
    );
    assert!(report_out.contains("Final verdict: pass"), "{report_out}");
    assert!(
        report_out.contains("bounded in src/main.rs"),
        "{report_out}"
    );
    assert!(report_err.is_empty(), "{report_err}");
}

/// A "fix" that does not actually fix reopens the finding, and the campaign refuses to pass.
#[test]
fn a_resolution_the_next_round_refutes_reopens_and_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());

    let (code, ..) = reviewctl(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 3);
    let (_, ledger_out, _) = reviewctl(
        &repo,
        &home,
        &["ledger", "--campaign", "loop", "--state", &state],
    );
    let key = ledger_out.split('\t').next().unwrap().to_string();

    // Claim it is fixed without touching the code.
    let (code, ..) = reviewctl(
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
        ],
    );
    assert_eq!(code, 0);

    // Round 2 re-finds it: reopened, and the run must not pass.
    let (code, stdout, _) = reviewctl(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 3, "a hollow resolution must not converge\n{stdout}");
    let (_, ledger_out, _) = reviewctl(
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
    let (code, stdout, _) = reviewctl(
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
    let (code, stdout, _) = reviewctl(
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
    let (code, stdout, _) = reviewctl(
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
    assert_eq!(code, 3, "the defect is found; not converged\n{stdout}");
    assert!(
        stdout.contains("round    1"),
        "the incomplete run must not have burned round 1:\n{stdout}"
    );

    // Now that a round has closed, the next run advances.
    let (_, stdout, _) = reviewctl(
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
    let (code, ..) = reviewctl(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert_eq!(code, 3);
    let (_, ledger_out, _) = reviewctl(
        &repo,
        &home,
        &["ledger", "--campaign", "loop", "--state", &state],
    );
    let key = ledger_out.split('\t').next().unwrap().to_string();

    // The operator rejects it (disagrees with the finding).
    let (code, ..) = reviewctl(
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
        ],
    );
    assert_eq!(code, 0);

    // Round 2: the only finding is declined, so nothing is carried back to the reviewers.
    let (_, stdout, _) = reviewctl(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
    );
    assert!(
        !stdout.contains("findings carried"),
        "a rejected finding must not be sent back:\n{stdout}"
    );
}

#[test]
fn committed_and_dirty_diff_subjects_execute_the_wired_change_set() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(dir.path());
    let codex = home.join("codex");
    std::fs::write(
        &codex,
        r#"#!/bin/sh
out=
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then out=$2; shift 2; else shift; fi
done
cat >/dev/null
printf '%s' '{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]}' >"$out"
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1}}'
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let reviewers = repo.join(".review/reviewers");
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
args = []
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
    let registry = review_config::lock::Registry::new([&reviewers]);
    let mut lockfile = review_config::lock::Lockfile::empty();
    lockfile.reviewers.insert(
        "tester".into(),
        review_config::lock::Lockfile::pin("tester", &registry).unwrap(),
    );
    std::fs::write(repo.join(".review/review.lock"), lockfile.to_toml()).unwrap();
    std::fs::write(
        repo.join(".review/pipelines/heavy.toml"),
        r#"
version = 2
[subject]
kind = "diff"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [
  { name = "findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "change_set", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
[[nodes]]
id = "reviewer"
kind = "reviewer"
package = "tester"
inputs = [{ name = "change_set", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[nodes]]
id = "gather"
kind = "gather"
inputs = [{ name = "reviewer", type = "review.kernel/ReviewerResult@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [{ name = "reports", type = "review.kernel/ReportSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [{ name = "reports", type = "review.kernel/ReportSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
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
"#,
    )
    .unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-qm", "declare diff subject"]);

    let (code, stdout, stderr) = reviewctl(
        &repo,
        &home,
        &["run", "--campaign", "diff", "--state", &state],
    );

    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("done      generation"), "{stdout}");

    std::fs::write(repo.join("dirty.txt"), b"not committed\n").unwrap();
    let (code, stdout, stderr) = reviewctl(
        &repo,
        &home,
        &[
            "run",
            "--campaign",
            "diff-dirty",
            "--state",
            &state,
            "--uncommitted",
        ],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("done      reviewer"), "{stdout}");
}
