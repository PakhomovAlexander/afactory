//! Changed paths select the pipeline — deterministically, token-free, visible in `plan`, pinned
//! at Campaign open — and an oversized input switches to the declared bounded pipeline. The
//! selected pipeline's `plan` reserves every static Worker's first Attempt against its cap.

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
    Command::new(env!("CARGO_BIN_EXE_af"))
        .args(args)
        .env("XDG_STATE_HOME", state_home)
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn commit(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
}

/// A repository onboarded by this binary with two pipelines (`review`, `docs`) and routes.
struct Routed {
    root: tempfile::TempDir,
    repo: PathBuf,
}

impl Routed {
    fn new(routing: &str) -> Routed {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        let state_home = root.path().join("state");
        let created = af(
            &state_home,
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
        std::fs::copy(
            repo.join(".af/pipelines/review.toml"),
            repo.join(".af/pipelines/docs.toml"),
        )
        .unwrap();
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("docs/guide.md"), "# Guide\n").unwrap();
        std::fs::write(repo.join("README.md"), "# Demo\n").unwrap();
        std::fs::write(repo.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
        let routed = Routed { root, repo };
        routed.set_routing(routing);
        commit(&routed.repo, "base");
        routed
    }

    fn state_home(&self) -> PathBuf {
        self.root.path().join("state")
    }

    /// Rewrites the routing policy and re-pins every pipeline the project may select.
    fn set_routing(&self, routing: &str) {
        let project_path = self.repo.join(".af/af.toml");
        let text = std::fs::read_to_string(&project_path).unwrap();
        let base = text
            .split("\n[routing]")
            .next()
            .unwrap()
            .split("\n[[routes]]")
            .next()
            .unwrap();
        std::fs::write(&project_path, format!("{}\n{routing}", base.trim_end())).unwrap();
        let refreshed = af(
            &self.state_home(),
            &[
                "onboard",
                "--repo",
                self.repo.to_str().unwrap(),
                "--refresh-lock",
            ],
        );
        assert!(refreshed.status.success(), "{}", stderr(&refreshed));
    }

    fn plan(&self, extra: &[&str]) -> Output {
        let mut args = vec![
            "review",
            "plan",
            "--repo",
            self.repo.to_str().unwrap(),
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD~1",
            "--candidate",
            "HEAD",
            "--json",
        ];
        args.extend_from_slice(extra);
        af(&self.state_home(), &args)
    }

    fn planned(&self, extra: &[&str]) -> serde_json::Value {
        let output = self.plan(extra);
        assert!(output.status.success(), "{}", stderr(&output));
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

const DOCS_ROUTE: &str =
    "[[routes]]\nname = \"docs\"\npaths = [\"docs/**\", \"*.md\"]\npipeline = \"docs\"\n";

#[test]
fn changed_paths_select_the_pipeline_and_plan_shows_the_decision() {
    let routed = Routed::new(DOCS_ROUTE);
    let validated = af(
        &routed.state_home(),
        &["onboard", "--repo", routed.repo.to_str().unwrap(), "--json"],
    );
    assert!(validated.status.success(), "{}", stderr(&validated));
    let lock = std::fs::read_to_string(routed.repo.join(".af/af.lock")).unwrap();
    assert!(
        lock.contains("[pipelines.docs]"),
        "route targets are pinned:\n{lock}"
    );

    std::fs::write(routed.repo.join("docs/guide.md"), "# Guide\n\nmore\n").unwrap();
    std::fs::write(routed.repo.join("README.md"), "# Demo\n\nmore\n").unwrap();
    commit(&routed.repo, "docs only");
    let plan = routed.planned(&[]);
    assert_eq!(plan["route"]["policy"], "route");
    assert_eq!(plan["route"]["name"], "docs");
    assert_eq!(plan["route"]["changed_paths"], 2);
    assert_eq!(plan["pipeline"]["path"], ".af/pipelines/docs.toml");

    // An explicit --pipeline always wins.
    let explicit = routed.planned(&["--pipeline", ".af/pipelines/review.toml"]);
    assert_eq!(explicit["route"]["policy"], "explicit");
    assert_eq!(explicit["pipeline"]["path"], ".af/pipelines/review.toml");

    std::fs::write(routed.repo.join("src/lib.rs"), "pub fn one() -> u8 { 2 }\n").unwrap();
    commit(&routed.repo, "code and docs");
    let mixed = routed.planned(&[]);
    assert_eq!(mixed["route"]["policy"], "default");
    assert_eq!(mixed["pipeline"]["path"], ".af/pipelines/review.toml");
    assert!(mixed["route"]["matched"].as_array().unwrap().is_empty());
}

/// Planning is reservation-aware and token-free: every static Worker's first Attempt reserves
/// its node cap or the pipeline's, the run must admit all of them at once, and each Worker's
/// first-Attempt input is measured against its cap.
#[test]
fn plan_reserves_every_static_worker_first_attempt_and_measures_its_input() {
    let routed = Routed::new(DOCS_ROUTE);
    // correctness reserves its own cap; architecture falls back to the pipeline's 300k.
    let review = routed.repo.join(".af/pipelines/review.toml");
    let text = std::fs::read_to_string(&review).unwrap();
    let capped = text.replace(
        "id = \"correctness\"\nkind = \"reviewer\"\n",
        "id = \"correctness\"\nkind = \"reviewer\"\nbudget = { attempt = 200000 }\n",
    );
    assert_ne!(capped, text);
    std::fs::write(&review, capped).unwrap();
    routed.set_routing(DOCS_ROUTE);
    commit(&routed.repo, "policy: a node cap");
    std::fs::write(routed.repo.join("src/lib.rs"), "pub fn one() -> u8 { 2 }\n").unwrap();
    commit(&routed.repo, "code");

    let plan = routed.planned(&[]);
    assert_eq!(plan["schema"], "af/review-plan@1");
    assert_eq!(plan["token_free"], true);
    assert!(
        plan["external_effects"].is_object(),
        "a plan must report its external effects"
    );
    assert_eq!(plan["pipeline"]["path"], ".af/pipelines/review.toml");
    let reservations = plan["pipeline"]["reservations"].as_array().unwrap();
    let reserved = |node: &str| {
        reservations
            .iter()
            .find(|reservation| reservation["node"] == node)
            .unwrap_or_else(|| panic!("{node} holds no reservation: {reservations:?}"))
    };
    assert_eq!(reservations.len(), 2, "{reservations:?}");
    assert_eq!(reserved("correctness")["source"], "node");
    assert_eq!(reserved("correctness")["tokens"], 200_000);
    assert_eq!(reserved("architecture")["source"], "pipeline");
    assert_eq!(reserved("architecture")["tokens"], 300_000);
    let sum: u64 = reservations
        .iter()
        .map(|reservation| reservation["tokens"].as_u64().unwrap())
        .sum();
    assert_eq!(
        plan["pipeline"]["max_simultaneous_reservation"].as_u64(),
        Some(sum)
    );
    for reservation in reservations {
        assert!(
            reservation["input_bytes"].as_u64().unwrap() > 0,
            "{reservation}"
        );
        assert_eq!(reservation["fits"], true, "{reservation}");
    }
    assert_eq!(plan["pipeline"]["inputs_fit"], true);
}

#[test]
fn unmatched_and_ambiguous_routes_fail_by_policy() {
    let routed = Routed::new(&format!("[routing]\nunmatched = \"refuse\"\n{DOCS_ROUTE}"));
    std::fs::write(routed.repo.join("src/lib.rs"), "pub fn one() -> u8 { 2 }\n").unwrap();
    commit(&routed.repo, "code");
    let refused = routed.plan(&[]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("no route covers"),
        "{}",
        stderr(&refused)
    );

    routed.set_routing(&format!(
        "{DOCS_ROUTE}[[routes]]\nname = \"markdown\"\npaths = [\"**/*.md\"]\npipeline = \"docs\"\n"
    ));
    commit(&routed.repo, "policy: overlapping routes");
    std::fs::write(routed.repo.join("docs/guide.md"), "# Guide\n\nmore\n").unwrap();
    commit(&routed.repo, "docs");
    let ambiguous = routed.plan(&[]);
    assert!(!ambiguous.status.success());
    assert!(
        stderr(&ambiguous).contains("routes docs, markdown"),
        "{}",
        stderr(&ambiguous)
    );

    routed.set_routing(&format!(
        "[routing]\nambiguous = \"first\"\n{DOCS_ROUTE}[[routes]]\nname = \"markdown\"\npaths = [\"**/*.md\"]\npipeline = \"docs\"\n"
    ));
    commit(&routed.repo, "policy: first wins");
    std::fs::write(routed.repo.join("docs/guide.md"), "# Guide\n\neven more\n").unwrap();
    commit(&routed.repo, "docs again");
    let first = routed.planned(&[]);
    assert_eq!(first["route"]["name"], "docs");
    assert_eq!(
        first["route"]["matched"],
        serde_json::json!(["docs", "markdown"])
    );
}

#[test]
fn an_oversized_input_switches_to_the_declared_pipeline_and_explicit_is_never_overridden() {
    let routed = Routed::new(&format!("[routing]\noversized = \"docs\"\n{DOCS_ROUTE}"));
    // The default pipeline caps correctness at 100 tokens; the docs pipeline keeps 300k.
    let review = routed.repo.join(".af/pipelines/review.toml");
    let text = std::fs::read_to_string(&review).unwrap();
    let capped = text.replace(
        "id = \"correctness\"\nkind = \"reviewer\"\n",
        "id = \"correctness\"\nkind = \"reviewer\"\nbudget = { attempt = 100 }\n",
    );
    assert_ne!(capped, text);
    std::fs::write(&review, capped).unwrap();
    routed.set_routing(&format!("[routing]\noversized = \"docs\"\n{DOCS_ROUTE}"));
    commit(&routed.repo, "policy: tiny cap");
    std::fs::write(routed.repo.join("src/lib.rs"), "pub fn one() -> u8 { 2 }\n").unwrap();
    commit(&routed.repo, "code");

    let plan = routed.planned(&[]);
    assert_eq!(plan["route"]["policy"], "oversized");
    assert_eq!(plan["route"]["replaced"], ".af/pipelines/review.toml");
    assert_eq!(plan["pipeline"]["path"], ".af/pipelines/docs.toml");
    assert_eq!(plan["pipeline"]["inputs_fit"], true);
    // Open uses the same selection; a continuation never re-routes, and only an explicit,
    // different `--pipeline` conflicts with a pinned one (covered by the resume path). An
    // explicit selection is never overridden by the oversized policy either.
    let explicit = routed.planned(&["--pipeline", ".af/pipelines/review.toml"]);
    assert_eq!(explicit["route"]["policy"], "explicit");
    assert_eq!(explicit["pipeline"]["inputs_fit"], false);
}
