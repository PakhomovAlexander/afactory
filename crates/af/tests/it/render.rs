//! `af review render` shows the exact input a Worker would receive, token-free: no Campaign
//! state, no Provider, no Gate, no spend — and byte-for-byte what the adapter would send.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn af(state_home: &Path, args: &[&str]) -> Output {
    crate::common::af()
        .args(args)
        .env("HOME", state_home)
        .env("XDG_CONFIG_HOME", state_home.join("config"))
        .env("XDG_STATE_HOME", state_home)
        .env("AF_SELF_OFFLINE", "1")
        .env("PATH", path_without_provider_clis())
        .output()
        .unwrap()
}

/// `PATH` without the directories that hold a `codex` or `claude`. A plan reads each bound
/// Provider's model catalog token-free through its CLI, so with the developer's own CLI on
/// `PATH` a model line would read `ok` where CI, which has none, reads `unverified against the
/// catalog`; here every machine reads the latter.
fn path_without_provider_clis() -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(
        std::env::split_paths(&path)
            .filter(|dir| !["codex", "claude"].iter().any(|cli| dir.join(cli).exists())),
    )
    .unwrap()
}

fn register_codex(state_home: &Path) {
    let auth = state_home.join("codex-auth");
    std::fs::create_dir_all(&auth).unwrap();
    let added = af(
        state_home,
        &[
            "provider",
            "add",
            "codex-main",
            "--kind",
            "codex",
            "--auth-dir",
            auth.to_str().unwrap(),
            "--unverified",
        ],
    );
    assert!(added.status.success(), "{}", stderr(&added));
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Re-pin one `.af/` pipeline after editing it: the lock binds pipelines by digest.
fn repin(repo: &Path, name: &str) {
    let lock_path = repo.join(".af/af.lock");
    let mut lock = std::fs::read_to_string(&lock_path)
        .ok()
        .map(|text| review_config::lock::Lockfile::from_toml(&text).unwrap())
        .unwrap_or_else(review_config::lock::Lockfile::empty);
    let bytes = std::fs::read(repo.join(format!(".af/pipelines/{name}.toml"))).unwrap();
    lock.pipelines.insert(
        name.into(),
        review_config::lock::Pin {
            version: "1.0.0".into(),
            digest: review_store::canonical::blob_content_id(&bytes),
        },
    );
    std::fs::write(lock_path, lock.to_toml()).unwrap();
}

/// A repository onboarded by this binary with Codex reviewers, plus one committed change, so
/// the Diff Subject has a real patch.
fn onboarded_repo_with_a_change(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let created = af(
        &root.join("onboard"),
        &[
            "onboard",
            "--repo",
            repo.to_str().unwrap(),
            "--runner",
            "codex",
            "--gate",
            "check=true",
            "--apply",
        ],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "policy"]);
    std::fs::write(repo.join("notes.md"), "# Notes\n\nrender me exactly\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "a change to review"]);
    repo
}

#[test]
fn a_model_worker_input_is_the_package_contract_and_change_set_with_no_effects() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded_repo_with_a_change(root.path());
    let state_home = root.path().join("state");
    let repo_arg = repo.to_str().unwrap();
    let selectors = [
        "review",
        "render",
        "--repo",
        repo_arg,
        "--pipeline",
        ".af/pipelines/review.toml",
        "--policy-rev",
        "HEAD",
        "--base",
        "HEAD~1",
        "--candidate",
        "HEAD",
        "--node",
        "correctness",
    ];

    let mut json_args = selectors.to_vec();
    json_args.push("--json");
    let output = af(&state_home, &json_args);
    assert!(output.status.success(), "{}", stderr(&output));
    let view: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["schema"], "af/review-render@1");
    assert_eq!(view["token_free"], true);
    assert_eq!(view["node"], "correctness");
    assert_eq!(view["runner"], "codex");
    assert_eq!(view["transport"], "prompt");
    assert_eq!(view["package"]["name"], "correctness");
    let input = view["input"].as_str().unwrap();
    let instructions =
        std::fs::read_to_string(repo.join(".af/workers/correctness/reviewer.md")).unwrap();
    assert!(
        input.starts_with(&instructions),
        "the verified package bytes lead"
    );
    assert!(input.contains("## Diff Subject Change Set"), "{input}");
    assert!(
        input.contains("render me exactly"),
        "the exact patch is in the input"
    );
    assert!(input.contains("notes.md"), "{input}");
    assert_eq!(view["bytes"].as_u64().unwrap(), input.len() as u64);
    assert_eq!(view["manifest"]["rendered_bytes"], view["bytes"]);
    assert_eq!(
        view["cap_tokens"], 300_000,
        "onboarding caps every Attempt at 300k"
    );
    assert_eq!(view["fits"], true);
    for effect in [
        "campaign_state",
        "gates",
        "provider_admission",
        "worker_dispatch",
        "token_spend",
    ] {
        assert_eq!(view["external_effects"][effect], false, "{effect}");
    }
    let omitted = view["not_rendered"].as_array().unwrap();
    assert!(
        omitted
            .iter()
            .any(|item| item.as_str().unwrap().contains("attempt authority")),
        "{omitted:?}"
    );
    assert!(
        !state_home.join("af").exists(),
        "render must create no Campaign state"
    );

    // Without --json the raw input is stdout, byte for byte; the header is stderr.
    let raw = af(&state_home, &selectors);
    assert!(raw.status.success(), "{}", stderr(&raw));
    assert_eq!(raw.stdout, input.as_bytes());
    assert!(stderr(&raw).contains("review render (token-free; no Campaign state)"));
    assert!(stderr(&raw).contains("omitted"));
}

#[test]
fn a_command_worker_receives_the_typed_document_and_non_workers_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("consumer");
    std::fs::create_dir_all(repo.join(".af/pipelines")).unwrap();
    std::fs::write(
        repo.join(".af/af.toml"),
        "version = 1\n[project]\nname = \"consumer\"\nmin_af = \"0.6\"\n[defaults]\npipeline = \"check\"\n",
    )
    .unwrap();
    std::fs::write(
        repo.join(".af/pipelines/check.toml"),
        r#"version = 2

[subject]
kind = "whole-tree"

[[nodes]]
id = "gate"
kind = "gate"
outputs = [{ name = "decision", type = "review.kernel/GateDecision@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]

[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]

[[nodes]]
id = "lint"
kind = "reviewer"
inputs = [{ name = "gate", type = "review.kernel/GateDecision@1", cardinality = "one", optional = false, snapshot_affinity = "any" }, { name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
gated_by = "gate"
[nodes.runner]
program = "/bin/sh"
args = [{ value = "-c" }, { value = "cat >/dev/null; printf '%s' '{}'" }]

[[nodes]]
id = "gather"
kind = "gather"
inputs = [{ name = "lint", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [{ name = "reports", type = "review.kernel/ReportSet@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]

[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [{ name = "reports", type = "review.kernel/ReportSet@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]
outputs = [
  { name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]

[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "lint", port = "gate" }

[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "lint", port = "prior_findings" }

[[edges]]
from = { node = "lint", port = "result" }
to = { node = "gather", port = "lint" }

[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#,
    )
    .unwrap();
    repin(&repo, "check");
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "policy"]);
    let state_home = root.path().join("state");
    let repo_arg = repo.to_str().unwrap();
    let base = [
        "review",
        "render",
        "--repo",
        repo_arg,
        "--pipeline",
        ".af/pipelines/check.toml",
        "--policy-rev",
        "HEAD",
        "--json",
    ];

    let mut args = base.to_vec();
    args.extend(["--node", "lint"]);
    let output = af(&state_home, &args);
    assert!(output.status.success(), "{}", stderr(&output));
    let view: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["transport"], "json");
    assert_eq!(view["runner"], "sh");
    assert!(
        view.get("package").is_none(),
        "a command Worker has no package"
    );
    let document: serde_json::Value =
        serde_json::from_str(view["input"].as_str().unwrap()).unwrap();
    assert!(document.is_object(), "{document}");
    assert!(
        view["not_rendered"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str().unwrap().starts_with("gate:")),
        "{}",
        view["not_rendered"]
    );

    let mut args = base.to_vec();
    args.extend(["--node", "gather"]);
    let refused = af(&state_home, &args);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("not a reviewer Worker"),
        "{}",
        stderr(&refused)
    );

    let missing = af(&state_home, &base);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("--node"), "{}", stderr(&missing));
}

/// An input that alone exhausts its Worker's Attempt cap is refused before any Gate, Provider
/// admission, or Worker — nothing charged — and `plan` says so first.
#[test]
fn an_input_that_exhausts_its_attempt_cap_is_refused_before_admission() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded_repo_with_a_change(root.path());
    // A 100-token cap on the correctness Worker: its rendered input cannot fit.
    let pipeline = repo.join(".af/pipelines/review.toml");
    let text = std::fs::read_to_string(&pipeline).unwrap();
    let capped = text.replace(
        "id = \"correctness\"\nkind = \"reviewer\"\n",
        "id = \"correctness\"\nkind = \"reviewer\"\nbudget = { attempt = 100 }\n",
    );
    assert_ne!(capped, text);
    std::fs::write(&pipeline, capped).unwrap();
    repin(&repo, "review");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "tiny cap"]);
    let state_home = root.path().join("state");
    let repo_arg = repo.to_str().unwrap();
    let selectors = [
        "--repo",
        repo_arg,
        "--pipeline",
        ".af/pipelines/review.toml",
        "--policy-rev",
        "HEAD",
        "--base",
        "HEAD~2",
        "--candidate",
        "HEAD",
    ];

    let mut plan_args = vec!["review", "plan"];
    plan_args.extend(selectors);
    register_codex(&state_home);
    plan_args.extend([
        "--provider",
        "architecture=codex-main",
        "--provider",
        "correctness=codex-main",
    ]);
    plan_args.push("--json");
    let planned = af(&state_home, &plan_args);
    assert!(planned.status.success(), "{}", stderr(&planned));
    let plan: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(plan["pipeline"]["inputs_fit"], false);
    let reservation = plan["pipeline"]["reservations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|reservation| reservation["node"] == "correctness")
        .expect("the correctness Worker holds a reservation");
    assert_eq!(reservation["source"], "node");
    assert_eq!(reservation["tokens"], 100);
    assert_eq!(reservation["fits"], false);
    assert!(reservation["input_tokens"].as_u64().unwrap() > 100);

    let campaign_state = root.path().join("campaign");
    let mut run_args = vec!["review", "run", "--campaign", "fit", "--state"];
    run_args.push(campaign_state.to_str().unwrap());
    run_args.extend(selectors);
    run_args.extend([
        "--provider",
        "correctness=codex-main",
        "--provider",
        "architecture=codex-main",
    ]);
    let refused = af(&state_home, &run_args);
    assert!(!refused.status.success(), "the run must refuse");
    let message = stderr(&refused);
    assert!(message.contains("exhausts its Attempt cap"), "{message}");
    assert!(message.contains("`correctness` receives"), "{message}");
    assert!(
        message.contains("Nothing was dispatched or charged"),
        "{message}"
    );
    assert!(message.contains("Scatter"), "{message}");
}

/// `af review plan` names each captured Model Worker's verdict from the one validation authority,
/// and refuses a binding the static rules refuse before any Provider process starts, naming the
/// levels Codex accepts and `af provider models` (ADR-0148).
#[test]
fn review_plan_prints_each_model_line_and_refuses_an_effort_level_codex_does_not_accept() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded_repo_with_a_change(root.path());
    let state_home = root.path().join("state");
    let repo_arg = repo.to_str().unwrap();
    register_codex(&state_home);
    let plan_args = [
        "review",
        "plan",
        "--repo",
        repo_arg,
        "--pipeline",
        ".af/pipelines/review.toml",
        "--policy-rev",
        "HEAD",
        "--base",
        "HEAD~1",
        "--candidate",
        "HEAD",
        "--provider",
        "architecture=codex-main",
        "--provider",
        "correctness=codex-main",
    ];
    let planned = af(&state_home, &plan_args);
    assert!(planned.status.success(), "{}", stderr(&planned));
    let text = String::from_utf8_lossy(&planned.stdout);
    for node in ["correctness", "architecture"] {
        let line = format!("model {node} gpt-6-sol high  unverified against the catalog");
        assert!(text.lines().any(|row| row == line), "{line}:\n{text}");
    }

    let manifest = repo.join(".af/workers/correctness/reviewer.toml");
    let declared = std::fs::read_to_string(&manifest).unwrap();
    let edited = declared.replace(
        "model_reasoning_effort=\"high\"",
        "model_reasoning_effort=\"minimal\"",
    );
    assert_ne!(edited, declared);
    std::fs::write(&manifest, edited).unwrap();
    let lock_path = repo.join(".af/af.lock");
    let mut lock =
        review_config::lock::Lockfile::from_toml(&std::fs::read_to_string(&lock_path).unwrap())
            .unwrap();
    let registry = review_config::lock::Registry::new(repo.join(".af/workers"));
    lock.workers.insert(
        "correctness".into(),
        review_config::lock::Lockfile::pin("correctness", &registry).unwrap(),
    );
    std::fs::write(&lock_path, lock.to_toml()).unwrap();
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "commit",
            "-q",
            "-m",
            "an effort level codex does not accept",
        ],
    );
    let refused = af(&state_home, &plan_args);
    assert!(!refused.status.success(), "the plan must refuse");
    let message = stderr(&refused);
    for part in [
        "correctness",
        "the effort level `minimal` is not one Codex accepts: low, medium, high, xhigh, max, ultra",
        "(model_not_accepted)",
        "`af provider models` (token-free)",
    ] {
        assert!(message.contains(part), "{part}:\n{message}");
    }
}

/// File names are repository-authored: `af review plan` shows one holding ESC, BEL or a newline
/// with every control character escaped visibly in text mode, while `--json` carries the names
/// exactly as before, escaped by JSON itself.
#[test]
fn review_plan_text_output_shows_control_characters_in_changed_paths_escaped() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded_repo_with_a_change(root.path());
    let hostile = ["esc\u{1b}]0;PWNED\u{7}name", "new\nline"];
    for name in hostile {
        std::fs::write(repo.join(name), "hostile name\n").unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "hostile names"]);
    let state_home = root.path().join("state");
    let repo_arg = repo.to_str().unwrap();
    register_codex(&state_home);
    let mut plan_args = vec![
        "review",
        "plan",
        "--repo",
        repo_arg,
        "--pipeline",
        ".af/pipelines/review.toml",
        "--policy-rev",
        "HEAD~2",
        "--base",
        "HEAD~1",
        "--candidate",
        "HEAD",
        "--provider",
        "architecture=codex-main",
        "--provider",
        "correctness=codex-main",
    ];
    let planned = af(&state_home, &plan_args);
    assert!(planned.status.success(), "{}", stderr(&planned));
    let raw_controls: Vec<u8> = planned
        .stdout
        .iter()
        .copied()
        .filter(|byte| (*byte < 0x20 && *byte != b'\n') || *byte == 0x7f)
        .collect();
    assert!(
        raw_controls.is_empty(),
        "raw control bytes {raw_controls:?}"
    );
    let text = String::from_utf8(planned.stdout).unwrap();
    assert!(!text.chars().any(|c| c.is_control() && c != '\n'), "{text}");
    for line in [
        "  change  esc\\u{1b}]0;PWNED\\u{7}name",
        "  change  new\\u{a}line",
    ] {
        assert!(text.lines().any(|row| row == line), "{line}:\n{text}");
    }
    assert!(
        !text.lines().any(|row| row == "line"),
        "a forged line:\n{text}"
    );

    plan_args.push("--json");
    let planned = af(&state_home, &plan_args);
    assert!(planned.status.success(), "{}", stderr(&planned));
    let plan: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
    let changed: Vec<&str> = plan["subject"]["changed_paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path.as_str().unwrap())
        .collect();
    assert_eq!(changed, ["esc\u{1b}]0;PWNED\u{7}name", "new\nline"]);
    let json = String::from_utf8(planned.stdout).unwrap();
    assert!(json.contains(r#""esc\u001b]0;PWNED\u0007name""#), "{json}");
}

/// The `--repo` of `af review plan` comes from the command line: one holding ESC, BEL or a
/// newline that cannot be opened is named in the error with every control character escaped
/// visibly, so it neither drives the terminal nor forges a line (audit C3).
#[test]
fn review_plan_names_a_repo_it_cannot_open_with_control_characters_escaped() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("esc\u{1b}]0;PWNED\u{7}repo\nforged");
    let planned = af(
        &root.path().join("state"),
        &["review", "plan", "--repo", repo.to_str().unwrap()],
    );
    assert!(!planned.status.success(), "{}", stderr(&planned));
    let text = stderr(&planned);
    assert!(
        !text.chars().any(|c| c.is_control() && c != '\n'),
        "raw control characters:\n{text}"
    );
    assert_eq!(text.lines().count(), 1, "a forged line:\n{text}");
    assert!(
        text.starts_with("af review: ")
            && text.contains("esc\\u{1b}]0;PWNED\\u{7}repo\\u{a}forged is not a Git repository"),
        "{text}"
    );
}

/// Pipeline node IDs are repository-authored too: a reviewer and a gather node whose IDs hold
/// ESC and BEL (written as TOML unicode escapes) reach no terminal raw through any line of
/// `af review plan`'s text output — topology, edges, reservations, input sizes or providers.
#[test]
fn review_plan_text_output_shows_control_characters_in_node_ids_escaped() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded_repo_with_a_change(root.path());
    let pipeline = repo.join(".af/pipelines/review.toml");
    let declared = std::fs::read_to_string(&pipeline).unwrap();
    let hostile = declared
        .replace(
            "id = \"architecture\"",
            "id = \"arch\\u001b]0;PWNED\\u0007itecture\"",
        )
        .replace(
            "node = \"architecture\"",
            "node = \"arch\\u001b]0;PWNED\\u0007itecture\"",
        )
        .replace("id = \"gather\"", "id = \"gath\\u001b[2J\\u0007er\"")
        .replace("node = \"gather\"", "node = \"gath\\u001b[2J\\u0007er\"");
    assert_ne!(hostile, declared);
    std::fs::write(&pipeline, hostile).unwrap();
    repin(&repo, "review");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "hostile node ids"]);
    let state_home = root.path().join("state");
    register_codex(&state_home);
    let planned = af(
        &state_home,
        &[
            "review",
            "plan",
            "--repo",
            repo.to_str().unwrap(),
            "--pipeline",
            ".af/pipelines/review.toml",
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD~1",
            "--candidate",
            "HEAD",
            "--provider",
            "arch\u{1b}]0;PWNED\u{7}itecture=codex-main",
            "--provider",
            "correctness=codex-main",
        ],
    );
    assert!(planned.status.success(), "{}", stderr(&planned));
    let raw_controls: Vec<u8> = planned
        .stdout
        .iter()
        .copied()
        .filter(|byte| (*byte < 0x20 && *byte != b'\n') || *byte == 0x7f)
        .collect();
    assert!(
        raw_controls.is_empty(),
        "raw control bytes {raw_controls:?}"
    );
    let text = String::from_utf8(planned.stdout).unwrap();
    assert!(!text.chars().any(|c| c.is_control() && c != '\n'), "{text}");
    let architecture = "arch\\u{1b}]0;PWNED\\u{7}itecture";
    let gather = "gath\\u{1b}[2J\\u{7}er";
    for line in [
        format!("  node {architecture} [reviewer]"),
        format!("  node {gather} [gather]"),
        format!("  {architecture}.result -> {gather}.architecture"),
    ] {
        assert!(text.lines().any(|row| row == line), "{line}:\n{text}");
    }
    // A resolved binding names the node as safely shown as everywhere else.
    let binding = format!("provider {architecture} -> codex-main");
    assert!(text.lines().any(|row| row == binding), "{binding}:\n{text}");
    for prefix in ["reserve  ", "input    "] {
        assert!(
            text.lines()
                .any(|row| row.starts_with(prefix) && row.contains(architecture)),
            "{prefix}:\n{text}"
        );
    }
}
