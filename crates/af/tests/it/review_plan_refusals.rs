//! `af review plan` refuses what `af review run` would, and its first-run refusals name the fix.
//!
//! Its help promises exactly what run would admit, so a `--provider NODE=ID` whose Provider
//! this machine does not register, or whose node is no model Worker of the pipeline, is refused
//! at plan time, exit 1, in text and under `--json`, in the words run uses. A repository with
//! no `.af/`, a policy revision git does not know, a model Worker without a binding and an
//! empty diff each say what to do next, and the plan ends with a `next` line: the exact
//! `af review run` command once the plan is ready.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// `af` in `repo`, with no Provider CLI on PATH: nothing here starts a Provider.
fn af(repo: &Path, args: &[&str]) -> Output {
    crate::common::af()
        .args(args)
        .current_dir(repo)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("NO_COLOR", "1")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env(
            "AF_PROVIDERS_FILE",
            repo.parent().unwrap().join("providers.toml"),
        )
        .output()
        .unwrap()
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The plan's text as one line per statement: a command continued with ` \` between its
/// arguments joined back, and prose wrapped under its label joined at its spaces.
fn unwrapped(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let continued = lines.last().is_some_and(|last| last.ends_with(" \\"));
        match lines.last_mut() {
            Some(last) if continued => {
                last.truncate(last.len() - 2);
                last.push(' ');
                last.push_str(line.trim_start());
            }
            Some(last)
                if line.starts_with("  ")
                    && !line.trim_start().starts_with("node ")
                    && !line.trim_start().starts_with("change ")
                    && !line.trim_start().starts_with("empty ")
                    && !line.contains(" -> ") =>
            {
                last.push(' ');
                last.push_str(line.trim_start());
            }
            _ => lines.push(line.to_owned()),
        }
    }
    lines.join("\n")
}

fn git(repo: &Path, args: &[&str]) {
    let done = Command::new("git")
        .current_dir(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(done.status.success(), "git {args:?}: {done:?}");
}

/// A repository with one commit and no `.af/`.
fn bare(root: &Path) -> PathBuf {
    let repo = root.join("bare");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# Bare\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// The bare repository onboarded with Codex reviewers and committed, plus an uncommitted change.
fn onboarded(root: &Path) -> PathBuf {
    let repo = bare(root);
    let onboard = af(
        &repo,
        &[
            "onboard",
            "--runner",
            "codex",
            "--gate",
            "check=true",
            "--apply",
        ],
    );
    assert!(onboard.status.success(), "{}", err(&onboard));
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "af review policy"]);
    std::fs::write(repo.join("notes.md"), "# Notes\n").unwrap();
    repo
}

/// Register `id` of `kind` with an auth directory of its own in the private home. The directory
/// holds no login and no Provider CLI is installed here, so it is registered `--unverified`
/// (ADR-0206); a token-free plan reads only the registry.
fn register(repo: &Path, id: &str, kind: &str) {
    let home = crate::common::private_home_env()
        .into_iter()
        .find(|(name, _)| *name == "HOME")
        .unwrap()
        .1;
    let auth = home.join(format!("auth-{id}"));
    std::fs::create_dir_all(&auth).unwrap();
    let added = af(
        repo,
        &[
            "provider",
            "add",
            id,
            "--kind",
            kind,
            "--auth-dir",
            auth.to_str().unwrap(),
            "--unverified",
        ],
    );
    assert!(added.status.success(), "{}", err(&added));
}

/// The suggestion path runs before a sandbox exists. A same-size tracked edit forces ordinary
/// `git status` to hash it and run the filter selected by the committed attributes file.
#[test]
fn selector_suggestions_do_not_run_checkout_git_programs() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let repo = onboarded(&root);
    std::fs::remove_file(repo.join("notes.md")).unwrap();
    std::fs::write(
        repo.join(".gitattributes"),
        "victim.txt filter=evil diff=evil\n",
    )
    .unwrap();
    std::fs::write(repo.join("victim.txt"), "before\n").unwrap();
    git(&repo, &["add", ".gitattributes", "victim.txt"]);
    git(&repo, &["commit", "-q", "-m", "hostile attributes"]);
    register(&repo, "codex-main", "codex");
    let clean = af(&repo, &["review", "plan", "--json"]);
    assert_eq!(clean.status.code(), Some(1), "{}", err(&clean));
    let clean_document: Value = serde_json::from_slice(&clean.stdout).unwrap();
    assert_eq!(clean_document["cause"], "review_selectors_missing");
    assert_eq!(clean_document["next_action"], Value::Null);

    let marker = root.join("marker");
    let script = root.join("evil.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf 'called:%s\\n' \"$*\" >> '{}'\ncat\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let script = script.to_str().unwrap();
    for key in [
        "filter.evil.clean",
        "filter.evil.smudge",
        "diff.evil.textconv",
        "core.fsmonitor",
    ] {
        git(&repo, &["config", key, script]);
    }
    std::fs::write(repo.join("victim.txt"), "after!\n").unwrap();
    assert!(
        !marker.exists(),
        "the fixture itself ran a configured program"
    );

    for args in [
        &["review", "plan", "--json"][..],
        &["review", "run", "--json"][..],
    ] {
        let refused = af(&repo, args);
        assert_eq!(refused.status.code(), Some(1), "{}", err(&refused));
        let document: Value = serde_json::from_slice(&refused.stdout).unwrap();
        assert_eq!(document["cause"], "review_selectors_missing", "{document}");
        assert!(
            document["next_action"]
                .as_str()
                .unwrap()
                .contains("--policy-rev HEAD --base HEAD --uncommitted"),
            "{document}"
        );
        assert!(!marker.exists(), "{args:?} ran a checkout Git program");
    }
    let bare = af(&repo, &["review"]);
    assert_eq!(bare.status.code(), Some(2), "{}", err(&bare));
    assert!(
        err(&bare).contains("--policy-rev HEAD --base HEAD --uncommitted"),
        "{}",
        err(&bare)
    );
    assert!(!marker.exists(), "bare review ran a checkout Git program");

    let status = Command::new("git")
        .arg("status")
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
    assert!(
        marker.exists(),
        "ordinary git status did not arm the fixture"
    );
    assert!(
        std::fs::read_to_string(marker)
            .unwrap()
            .contains("called:\n"),
        "ordinary git status did not run the clean filter"
    );
}

#[test]
fn review_plan_names_timeout_scope_beside_value() {
    let temp = tempfile::tempdir().unwrap();
    let repo = onboarded(temp.path());
    register(&repo, "codex-main", "codex");
    let output = plan(
        &repo,
        &[
            "--timeout-secs",
            "37",
            "--provider",
            "architecture=codex-main",
            "--provider",
            "correctness=codex-main",
        ],
        false,
    );
    assert!(output.status.success(), "{}", err(&output));
    assert!(
        out(&output).contains("timeout  --timeout-secs 37: per reviewer Attempt"),
        "{}",
        out(&output)
    );
}

const PLAN: [&str; 8] = [
    "review",
    "plan",
    "--policy-rev",
    "HEAD",
    "--base",
    "HEAD",
    "--uncommitted",
    "--json",
];

fn plan(repo: &Path, extra: &[&str], json: bool) -> Output {
    let mut args: Vec<&str> = PLAN[..if json { 8 } else { 7 }].to_vec();
    args.extend(extra);
    af(repo, &args)
}

/// The refusal, as text on stderr and as the one `af/error@2` document `--json` prints.
fn refused(repo: &Path, extra: &[&str], expected: &str) {
    let text = plan(repo, extra, false);
    assert_eq!(text.status.code(), Some(1), "{}", err(&text));
    assert!(err(&text).contains(expected), "{}", err(&text));
    let json = plan(repo, extra, true);
    assert_eq!(json.status.code(), Some(1), "{}", err(&json));
    let document: Value = serde_json::from_slice(&json.stdout)
        .unwrap_or_else(|_| panic!("not one document: {}", out(&json)));
    assert_eq!(document["schema"], "af/error@2");
    assert!(
        document["error"].as_str().unwrap().contains(expected),
        "{document}"
    );
}

#[test]
fn plan_refuses_an_unregistered_provider_and_a_node_the_pipeline_does_not_have() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded(&root.path().canonicalize().unwrap());
    refused(
        &repo,
        &["--provider", "architecture=nope"],
        "Provider nope not found in this machine's Provider registry",
    );
    refused(
        &repo,
        &["--provider", "architecture=nope"],
        "af provider status lists the registry, and af provider setup nope --kind codex \
         registers it",
    );
    refused(
        &repo,
        &["--provider", "nosuchnode=nope"],
        "Node nosuchnode not found in .af/pipelines/review.toml; model Workers: architecture, \
         correctness",
    );
    // A registered Provider of another kind than the Worker runs is refused too.
    register(&repo, "claude-main", "claude");
    refused(
        &repo,
        &["--provider", "architecture=claude-main"],
        "Provider claude-main is a claude Provider, and node architecture runs codex",
    );
    // `af review run` refuses the same binding in the same words.
    let run = af(
        &repo,
        &[
            "review",
            "run",
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD",
            "--uncommitted",
            "--provider",
            "architecture=nope",
            "--provider",
            "correctness=nope",
        ],
    );
    assert_eq!(run.status.code(), Some(1), "{}", err(&run));
    assert!(
        err(&run).contains("Provider nope not found in this machine's Provider registry"),
        "{}",
        err(&run)
    );
}

#[test]
fn ready_comes_from_the_registry_and_a_ready_plan_names_the_run_command() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded(&root.path().canonicalize().unwrap());
    let unregistered = plan(&repo, &[], true);
    assert_eq!(unregistered.status.code(), Some(1));
    let document: Value = serde_json::from_slice(&unregistered.stdout).unwrap();
    assert_eq!(document["cause"], "review_binding_missing", "{document}");
    assert_eq!(document["next_action"], Value::Null, "{document}");
    register(&repo, "codex-main", "codex");
    let suggested = plan(&repo, &[], true);
    assert_eq!(suggested.status.code(), Some(1));
    let document: Value = serde_json::from_slice(&suggested.stdout).unwrap();
    assert_eq!(document["cause"], "review_binding_missing", "{document}");
    let next = document["next_action"].as_str().unwrap();
    assert!(
        next.contains("--provider 'architecture=codex-main'"),
        "{next}"
    );
    assert!(
        next.contains("--provider 'correctness=codex-main'"),
        "{next}"
    );
    let text = err(&plan(&repo, &[], false));
    assert_eq!(text.lines().last(), Some(next), "{text}");
    let bound = [
        "--provider",
        "architecture=codex-main",
        "--provider",
        "correctness=codex-main",
    ];
    let document: Value = serde_json::from_slice(&plan(&repo, &bound, true).stdout).unwrap();
    assert_eq!(document["schema"], "af/review-plan@1");
    for provider in document["providers"].as_array().unwrap() {
        assert_eq!(provider["ready"], true, "{provider}");
    }
    let run = "af review run --policy-rev HEAD --base HEAD --uncommitted --provider \
               'architecture=codex-main' --provider 'correctness=codex-main'";
    assert_eq!(document["next_action"], run, "{document}");
    let text = unwrapped(&out(&plan(&repo, &bound, false)));
    assert!(
        text.contains("binding  architecture => codex-main"),
        "{text}"
    );
    assert!(
        text.contains("binding  correctness => codex-main"),
        "{text}"
    );
    assert_eq!(
        text.lines().last().unwrap(),
        format!("next:    {run}"),
        "{text}"
    );
}

/// The first-run plan at a terminal 100 and 80 columns wide: every line fits, prose continues
/// under its label, and the next command continues between its arguments, so a copied command
/// still runs whole.
#[test]
fn the_first_run_plan_fits_the_terminal_and_keeps_commands_whole() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded(&root.path().canonicalize().unwrap());
    let bound = [
        "--provider",
        "architecture=codex-main",
        "--provider",
        "correctness=codex-main",
    ];
    register(&repo, "codex-main", "codex");
    for (extra, columns) in [(&bound[..], 100), (&bound[..], 80)] {
        let mut args: Vec<&str> = PLAN[..7].to_vec();
        args.extend(extra);
        let shown = crate::common::af()
            .args(&args)
            .current_dir(&repo)
            .env(
                "AF_PROVIDERS_FILE",
                repo.parent().unwrap().join("providers.toml"),
            )
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("NO_COLOR", "1")
            .env("COLUMNS", columns.to_string())
            .output()
            .unwrap();
        assert!(shown.status.success(), "{}", err(&shown));
        let text = out(&shown);
        for line in text.lines() {
            assert!(
                line.chars().count() <= columns,
                "{columns}: {line:?}\n{text}"
            );
        }
        let next: Vec<&str> = text
            .lines()
            .skip_while(|line| !line.starts_with("next:    "))
            .collect();
        let command = next.join("\n").replace(" \\\n", " ");
        let words = shell_words::split(command.trim_start_matches("next:")).unwrap();
        assert_eq!(words[..3], ["af", "review", "run"]);
        assert!(words.contains(&"--provider".to_owned()), "{command}");
    }
    // The same plan without bindings is still a classified refusal.
    let unregistered = plan(&repo, &[], true);
    let error: Value = serde_json::from_slice(&unregistered.stdout).unwrap();
    assert_eq!(error["cause"], "review_binding_missing");
}

#[test]
fn first_run_refusals_name_the_fix() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    // No `.af/` at the policy revision.
    let bare = bare(&root);
    let refused = plan(&bare, &[], false);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        err(&refused).contains(
            "this repository has no .af/ authority at HEAD; fix: af onboard (preview), then af \
             onboard --apply, commit, and retry"
        ),
        "{}",
        err(&refused)
    );
    // A policy revision git does not know: its own one line, not the command af ran.
    let repo = onboarded(&root.join("onboarded"));
    register(&repo, "codex-main", "codex");
    let refused = af(
        &repo,
        &[
            "review",
            "plan",
            "--policy-rev",
            "nope",
            "--base",
            "HEAD",
            "--uncommitted",
        ],
    );
    assert_eq!(refused.status.code(), Some(1));
    let message = err(&refused);
    assert!(
        message.contains(&format!(
            "policy revision nope is not a git revision of {}: fatal: ",
            repo.display()
        )),
        "{message}"
    );
    assert!(!message.contains("rev-parse"), "{message}");
    // No policy revision at all: what it is and the command to try.
    let refused = af(&repo, &["review", "plan"]);
    assert_eq!(refused.status.code(), Some(1));
    let document: Value =
        serde_json::from_slice(&af(&repo, &["review", "plan", "--json"]).stdout).unwrap();
    assert_eq!(document["cause"], "review_selectors_missing");
    assert!(
        document["next_action"]
            .as_str()
            .unwrap()
            .contains("--policy-rev HEAD --base HEAD --uncommitted"),
        "{document}"
    );
    // An empty diff says what is empty and the selectors that would fill it.
    std::fs::remove_file(repo.join("notes.md")).unwrap();
    let empty = out(&plan(
        &repo,
        &[
            "--provider",
            "architecture=codex-main",
            "--provider",
            "correctness=codex-main",
        ],
        false,
    ));
    assert!(empty.contains("EMPTY — run will refuse"), "{empty}");
    let collapsed = empty.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        collapsed.contains(
            "empty the diff from base HEAD to candidate worktree changes no file; change files \
             and review them with --uncommitted, or name other revisions with --base REV and \
             --candidate REV"
        ),
        "{empty}"
    );
    assert!(
        collapsed.contains("next: change files first; the diff is empty"),
        "{empty}"
    );
    assert!(
        empty.contains("af review plan --policy-rev HEAD --base HEAD --uncommitted --provider"),
        "{empty}"
    );
}

#[test]
fn review_refusal_command_plans_and_never_trusts_a_feature_branch() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded(&root.path().canonicalize().unwrap());
    register(&repo, "codex-main", "codex");
    let refusal = af(&repo, &["review", "plan", "--json"]);
    assert_eq!(refusal.status.code(), Some(1), "{}", err(&refusal));
    let document: Value = serde_json::from_slice(&refusal.stdout).unwrap();
    assert_eq!(document["cause"], "review_selectors_missing");
    crate::schemas::valid(&crate::schemas::validator("error-v2.json"), &document);
    let command = document["next_action"].as_str().unwrap();
    assert!(
        command.contains("--policy-rev HEAD --base HEAD --uncommitted"),
        "{command}"
    );
    let words = shell_words::split(command).unwrap();
    let args: Vec<&str> = words.iter().skip(1).map(String::as_str).collect();
    let planned = af(&repo, &args);
    assert!(
        planned.status.success(),
        "{}\n{}",
        err(&planned),
        out(&planned)
    );
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    git(&repo, &["add", "notes.md"]);
    git(&repo, &["commit", "-q", "-m", "feature"]);
    let refusal = af(&repo, &["review", "plan", "--json"]);
    let document: Value = serde_json::from_slice(&refusal.stdout).unwrap();
    let command = document["next_action"].as_str().unwrap();
    assert!(!command.contains("--policy-rev HEAD"), "{command}");
    assert!(command.contains("--candidate HEAD"), "{command}");
}

#[test]
fn two_registered_providers_leave_the_binding_to_the_caller() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded(&root.path().canonicalize().unwrap());
    register(&repo, "codex-one", "codex");
    register(&repo, "codex-two", "codex");
    let refusal = plan(&repo, &[], true);
    assert_eq!(refusal.status.code(), Some(1));
    let document: Value = serde_json::from_slice(&refusal.stdout).unwrap();
    assert_eq!(document["cause"], "review_binding_missing");
    assert_eq!(document["next_action"], Value::Null);
    let candidates = &document["details"]["candidates"]["architecture"];
    assert!(
        candidates
            .as_array()
            .unwrap()
            .contains(&Value::from("codex-one"))
    );
    assert!(
        candidates
            .as_array()
            .unwrap()
            .contains(&Value::from("codex-two"))
    );
    crate::schemas::valid(&crate::schemas::validator("error-v2.json"), &document);
}

#[test]
fn first_run_review_causes_are_structured() {
    let root = tempfile::tempdir().unwrap();
    let bare = bare(&root.path().canonicalize().unwrap());
    let no_authority = plan(&bare, &[], true);
    let document: Value = serde_json::from_slice(&no_authority.stdout).unwrap();
    assert_eq!(document["cause"], "no_authority");
    assert_eq!(document["details"]["revision"], "HEAD");
    let not_repo = root.path().join("not-a-repo");
    std::fs::create_dir_all(&not_repo).unwrap();
    let output = af(
        &not_repo,
        &["review", "plan", "--policy-rev", "HEAD", "--json"],
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["cause"], "not_repository", "{document}");
    let repo = onboarded(&root.path().join("policy"));
    for (args, cause) in [
        (
            &["--provider", "architecture=nope"][..],
            "provider_not_registered",
        ),
        (
            &["--provider", "nosuchnode=nope"][..],
            "unknown_review_node",
        ),
    ] {
        let output = plan(&repo, args, true);
        let document: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(document["cause"], cause, "{document}");
        crate::schemas::valid(&crate::schemas::validator("error-v2.json"), &document);
    }
}

#[test]
fn bare_review_shows_the_copyable_command_before_help() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded(&root.path().canonicalize().unwrap());
    register(&repo, "codex-main", "codex");
    let shown = af(&repo, &["review"]);
    assert_eq!(shown.status.code(), Some(2), "{}", err(&shown));
    assert!(
        err(&shown).contains("af review plan --policy-rev HEAD --base HEAD --uncommitted"),
        "{}",
        err(&shown)
    );
    let text = err(&shown);
    assert!(text.contains("Usage: af review"), "{text}");
    assert!(
        text.find("af review plan --policy-rev").unwrap() < text.find("Usage: af review").unwrap(),
        "{text}"
    );
}

#[test]
fn review_run_missing_binding_returns_its_explicit_command() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded(&root.path().canonicalize().unwrap());
    register(&repo, "codex-main", "codex");
    let refused = af(
        &repo,
        &[
            "review",
            "run",
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD",
            "--uncommitted",
            "--json",
        ],
    );
    assert_eq!(refused.status.code(), Some(1), "{}", err(&refused));
    let document: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(document["cause"], "review_binding_missing", "{document}");
    let next = document["next_action"].as_str().unwrap();
    assert!(
        next.starts_with("af review run --policy-rev HEAD --base HEAD --uncommitted"),
        "{next}"
    );
    assert!(
        next.contains("--provider 'architecture=codex-main'"),
        "{next}"
    );
    crate::schemas::valid(&crate::schemas::validator("error-v2.json"), &document);
}

#[test]
fn unknown_default_branch_requires_a_trusted_policy_choice() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded(&root.path().canonicalize().unwrap());
    register(&repo, "codex-main", "codex");
    git(&repo, &["branch", "-m", "feature"]);
    git(&repo, &["config", "init.defaultBranch", "absent"]);
    let refused = af(&repo, &["review", "plan", "--json"]);
    let document: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(document["cause"], "review_selectors_missing");
    assert_eq!(document["next_action"], Value::Null, "{document}");
    assert!(
        document["error"]
            .as_str()
            .unwrap()
            .contains("trusted policy revision must be chosen"),
        "{document}"
    );
}
