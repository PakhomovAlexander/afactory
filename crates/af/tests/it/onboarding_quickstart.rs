//! Execute the README Quickstart's token-free commands, including the local policy commit, the
//! doctor and a first Task. No remote, Provider login, Gate or model is needed to reach a real
//! review plan.
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
        .env("AF_BIN", crate::common::af_binary())
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
        .env("AF_PROVIDERS_FILE", root.join("providers.toml"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Quickstart fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Quickstart fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        // Plenty of free disk for `af doctor`, whatever the machine running the test has.
        .env("AF_TEST_FREE_BYTES", (1_u64 << 40).to_string())
        .env_remove("SHELL")
        .output()
        .unwrap()
}

fn readme() -> String {
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    std::fs::read_to_string(workspace.join("README.md")).unwrap()
}

/// The README Quickstart's code blocks, in order.
fn quickstart_blocks() -> Vec<String> {
    let readme = readme();
    let quickstart = readme
        .split("## Quickstart\n")
        .nth(1)
        .unwrap()
        .split("## What it does")
        .next()
        .unwrap();
    quickstart
        .split("```sh\n")
        .skip(1)
        .map(|part| {
            // A block inside a list item is indented; its commands are not.
            part.split("```")
                .next()
                .unwrap()
                .lines()
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect()
}

/// The README's commands, run against a fixture repository: every one that is token-free and
/// local. The rest are named exactly: the install (network), `af provider detect --apply` (it
/// registers this machine's logins; package P1 adds it), the review run (it spends tokens) and
/// `af self update`/`uninstall` (they manage this machine's installation).
#[test]
fn readme_quickstart_reaches_a_plan_with_local_committed_authority() {
    let blocks = quickstart_blocks();
    assert_eq!(blocks.len(), 7, "{blocks:#?}");
    let [install, provider, onboard, review, doctor, updating, task] = &blocks[..] else {
        unreachable!()
    };
    // The four steps, one command each, then the doctor and a first Task.
    assert!(
        install.starts_with("curl --proto '=https' --tlsv1.2 -fsSL "),
        "{install}"
    );
    assert!(
        install.trim_end().ends_with("/install.sh | sh"),
        "{install}"
    );
    assert_eq!(provider.trim(), "af provider detect --apply");
    assert_eq!(
        onboard.trim(),
        "af onboard && af onboard --apply && git add .af && git commit -m 'Configure Afactory review authority'"
    );
    let (plan, run) = review.trim().split_once('\n').unwrap();
    assert_eq!(
        plan,
        "af review plan --policy-rev HEAD --base HEAD --uncommitted --provider correctness=PROVIDER \
         --provider architecture=PROVIDER"
    );
    // No machine's Provider IDs: the placeholder, and where the exact command comes from.
    assert_eq!(
        run,
        "af review --policy-rev HEAD --base HEAD --uncommitted --provider correctness=PROVIDER \
         --provider architecture=PROVIDER"
    );
    let flat = readme().split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("PROVIDER is the ID of a Provider step 2 registered, such as `codex-main`: `af onboard` prints the exact command"),
        "README says where the exact command comes from"
    );
    assert_eq!(doctor.trim(), "af doctor");
    // Updating and removing manage this machine's installation; they are not run here.
    for line in updating.lines() {
        assert!(line.starts_with("af self "), "{updating}");
    }
    for command in [
        "af task plan",
        "af task run",
        "af task show",
        "af task deliver",
    ] {
        assert!(task.contains(command), "{command} missing: {task}");
    }
    let readme = readme();
    let before = readme
        .split("curl --proto '=https' --tlsv1.2 -fsSL")
        .next()
        .unwrap();
    assert!(
        before.contains("brew install minisign"),
        "the minisign line comes first"
    );
    assert!(readme.contains("AF_INSTALL_CHANNEL=rc"));

    let root = tempfile::tempdir().unwrap();
    let executable_plan = plan
        .replace("correctness=PROVIDER", "correctness=claude-main")
        .replace("architecture=PROVIDER", "architecture=codex-main");
    let script = format!(
        r#"
af() {{
  case "$1 ${{2:-}}" in
    "onboard "*|"review plan"|"doctor "*) "$AF_BIN" "$@" ;;
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
{onboard}
# The guide asks the operator to make a real source change after committing the policy.
printf '\nA candidate change.\n' >> README.md
mkdir -p ../claude-auth ../codex-auth
AF_AUTH_ROOT=$(cd .. && pwd)
"$AF_BIN" provider add claude-main --kind claude --auth-dir "$AF_AUTH_ROOT/claude-auth" --unverified
"$AF_BIN" provider add codex-main --kind codex --auth-dir "$AF_AUTH_ROOT/codex-auth" --unverified
{executable_plan} > ../plan.txt
{executable_plan} --json > ../plan.json
{doctor} > ../doctor.txt
test ! -e ../gate-ran
test -z "$(git remote)"
"#
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
    // Planning and the doctor record nothing; `af onboard --apply` registers the Project and its
    // checkout (ADR-0158), which is the only state the walkthrough leaves.
    let kept: std::collections::BTreeSet<String> = std::fs::read_dir(root.path().join("state/af"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        kept.iter()
            .all(|name| ["project", "stores.toml", "stores.lock"].contains(&name.as_str())),
        "{kept:?}"
    );
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
    let doctor = std::fs::read_to_string(root.path().join("doctor.txt")).unwrap();
    // The rows wrap at 100 columns, where depends on the temporary path's length: read the
    // words, not the lines.
    let words = doctor.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(words.contains("validates"), "{doctor}");
    assert!(words.contains("Gate check runs make"), "{doctor}");
}

/// The README's first Task, run as written on the software starter: token-free command Workers
/// plan, run, show and deliver it to a new worktree.
#[test]
fn readme_first_task_plans_runs_shows_and_delivers_the_starter() {
    let blocks = quickstart_blocks();
    let task = blocks.last().unwrap();
    let root = tempfile::tempdir().unwrap();
    let script = format!(
        r#"
af() {{
  case "$1 ${{2:-}}" in
    "catalog init"|"task plan"|"task run"|"task show"|"task deliver") "$AF_BIN" "$@" ;;
    *) echo "Unexpected command in the first Task: $*" >&2; exit 1 ;;
  esac
}}
{task}
git status --porcelain > ../status.txt
"#
    );
    let output = shell(root.path(), &script);
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(
        shown.contains("Task implementation-small: finished/verified"),
        "{shown}"
    );
    // Delivered to a new worktree; the checkout itself is untouched.
    assert!(root.path().join("pagination").is_dir());
    assert!(
        std::fs::read_to_string(root.path().join("status.txt"))
            .unwrap()
            .is_empty()
    );
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
