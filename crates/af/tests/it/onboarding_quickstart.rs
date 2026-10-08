//! Execute the documented token-free onboarding path, including the reviewed local policy
//! commit. No remote, Provider login, Gate or model is needed to reach a real review plan.
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn shell(root: &Path, script: &str) -> std::process::Output {
    std::fs::create_dir_all(root.join("home with spaces")).unwrap();
    Command::new("sh")
        .arg("-eu")
        .arg("-c")
        .arg(script)
        .current_dir(root)
        .env("AF_BIN", env!("CARGO_BIN_EXE_af"))
        .env("AF_SELF_OFFLINE", "1")
        .env_remove("AF_VERSION")
        .env_remove("AF_RELEASE_KEY")
        .env("AF_RELEASE_SOURCE", root.join("releases"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_BIN_HOME", root.join("bin"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("HOME", root.join("home with spaces"))
        .env("TMPDIR", root)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Quickstart fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Quickstart fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap()
}

fn readme() -> String {
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    std::fs::read_to_string(workspace.join("README.md")).unwrap()
}

#[test]
fn readme_quickstart_reaches_a_plan_with_local_committed_authority() {
    let readme = readme();
    let quickstart = readme
        .split("## Quickstart\n")
        .nth(1)
        .unwrap()
        .split("## What it does")
        .next()
        .unwrap();
    let blocks: Vec<_> = quickstart
        .split("```sh\n")
        .skip(1)
        .map(|part| part.split("```").next().unwrap())
        .collect();
    // Keep the executable setup, review, commit and plan blocks distinct from paid execution.
    assert!(blocks[0].contains("af onboard"));
    assert!(blocks[1].contains("git diff"));
    assert!(blocks[2].contains("git commit"));
    assert!(blocks[3].contains("af review plan"));
    for block in &blocks[..4] {
        assert!(!block.contains("af provider"));
        assert!(!block.contains("af review run"));
    }
    let root = tempfile::tempdir().unwrap();
    let script = format!(
        r#"
af() {{
  case "$1 ${{2:-}}" in
    "onboard "*|"review plan"|"catalog test"|"task start"|"task explain") "$AF_BIN" "$@" ;;
    *) echo "Unexpected command in token-free walkthrough: $*" >&2; exit 1 ;;
  esac
}}
mkdir repo
cd repo
git init -q -b main
printf 'check:\n\t@touch "%s"; exit 1\n' "$PWD/../gate-ran" > Makefile
printf '# Example\n' > README.md
git add Makefile README.md
git commit -qm 'Initial source'
{}
{}
{}
# The guide asks the operator to make a real source change after capturing policy and Base.
printf '\nA candidate change.\n' >> README.md
{{
{}
}} > ../plan.txt
af review plan --policy-rev "$policy_rev" --base "$base_rev" --uncommitted --json > ../plan.json
test ! -e ../gate-ran
test -z "$(git remote)"
"#,
        blocks[0], blocks[1], blocks[2], blocks[3]
    );
    let output = shell(root.path(), &script);
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("plan.json")).unwrap()).unwrap();
    assert_eq!(plan["schema"], "af/review-plan@1");
    assert_eq!(plan["token_free"], true);
    assert!(!root.path().join("state").exists());
    assert_eq!(plan["subject"]["empty"], false);
    assert_eq!(plan["route"]["changed_paths"], 1);
    assert!(
        plan["external_effects"]
            .as_object()
            .unwrap()
            .values()
            .all(|value| value == false)
    );
    let text = std::fs::read_to_string(root.path().join("plan.txt")).unwrap();
    assert!(text.contains("correctness"), "{text}");
    assert!(text.contains("architecture"), "{text}");
}

#[test]
fn review_help_examples_name_policy_base_and_candidate() {
    for args in [vec!["--help"], vec!["review", "--help"]] {
        let output = crate::common::af().args(&args).output().unwrap();
        assert!(output.status.success());
        let help = String::from_utf8_lossy(&output.stdout);
        let examples = help.rsplit("Examples").next().unwrap();
        let mut found = false;
        for line in examples
            .lines()
            .filter(|line| line.trim_start().starts_with("af review"))
        {
            if line.contains(" ledger ") || line.contains(" report ") {
                continue;
            }
            found = true;
            assert!(line.contains("--policy-rev"), "{line}");
            assert!(line.contains("--base"), "{line}");
            assert!(
                line.contains("--uncommitted") || line.contains("--candidate"),
                "{line}"
            );
        }
        assert!(found, "{help}");
    }
}

#[test]
fn implementation_entrypoints_select_the_software_profile() {
    let readme = readme();
    assert!(readme.contains("af catalog init --profile software --destination DIR"));
    let help = crate::common::af()
        .args(["task", "--help"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&help.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        text.contains("af catalog init --profile software --destination DIR"),
        "{text}"
    );
}

fn generated_tutorial_plans_with_requests_outside_the_checkout(profile: &str) {
    let root = tempfile::tempdir().unwrap();
    let generated = shell(
        root.path(),
        &format!(r#""$AF_BIN" catalog init --profile {profile} --destination project"#),
    );
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let readme = std::fs::read_to_string(root.path().join("project/README.md")).unwrap();
    let block = readme
        .split("```sh\n")
        .nth(1)
        .unwrap()
        .split("# Replace PLAN_ID")
        .next()
        .unwrap();
    assert!(block.contains("task_files="), "{block}");
    assert!(!block.contains("--execute"), "{block}");
    let script = format!(
        r#"
af() {{
  case "$1 ${{2:-}}" in
    "onboard "*|"review plan"|"catalog test"|"task start"|"task explain") "$AF_BIN" "$@" ;;
    *) echo "Unexpected command in token-free walkthrough: $*" >&2; exit 1 ;;
  esac
}}
cd project
# A TMPDIR inside the checkout must not pull the requests back into the committed source.
export TMPDIR="$PWD"
{block}
case "$task_files/" in "$PWD"/*) echo "Task requests inside the checkout: $task_files" >&2; exit 1 ;; esac
af task explain implementation-reviewed --json > ../task-plan.json
git ls-files > ../tracked.txt
test -z "$(git status --porcelain)"
test -f "$task_files/implementation-reviewed.json"
test ! -e implementation-reviewed.json
"#
    );
    let output = shell(root.path(), &script);
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("is inside the repository"));
    let tracked = std::fs::read_to_string(root.path().join("tracked.txt")).unwrap();
    assert!(tracked.lines().any(|path| path == "contracts.json"));
    assert!(
        !tracked
            .lines()
            .any(|path| path.contains("af-task-requests"))
    );
    if profile == "all" {
        assert!(tracked.lines().any(|path| path == "sources.json"));
        assert!(!tracked.lines().any(|path| path == "document.json"));
    }
    assert!(
        !tracked
            .lines()
            .any(|path| path.starts_with("implementation-")
                || path.starts_with("review-")
                || path == "planning.json")
    );
    let plan: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("task-plan.json")).unwrap())
            .unwrap();
    assert_eq!(plan["attempts"], 0);
    assert_eq!(plan["chargeable_tokens"], "0");
    assert!(!plan["plan"].is_null());
}

#[test]
fn generated_software_tutorial_keeps_task_requests_external() {
    generated_tutorial_plans_with_requests_outside_the_checkout("software");
}

#[test]
fn generated_combined_tutorial_keeps_task_requests_external() {
    generated_tutorial_plans_with_requests_outside_the_checkout("all");
}
