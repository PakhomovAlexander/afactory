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
    let out = Command::new(env!("CARGO_BIN_EXE_reviewctl"))
        .current_dir(repo)
        .env("HOME", home)
        .env("USER", "loop-test")
        .args(args)
        .output()
        .unwrap();
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
        r#"version = 1

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

    // Remove the marker and run again: the previous round never closed, so this is still round 1.
    std::fs::remove_file(repo.join("FAIL")).unwrap();
    git(&repo, &home, &["commit", "-qam", "let the reviewer run"]);
    let (code, stdout, _) = reviewctl(
        &repo,
        &home,
        &["run", "--campaign", "loop", "--state", &state],
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
