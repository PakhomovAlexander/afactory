//! Binary-owned, token-free onboarding: preview first, atomic creation, then exact validation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Output;

use review_config::lock::{Lockfile, Pin};

fn repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    // A `.git` the kernel accepts as a repository holds a HEAD.
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    repo
}

fn af(repo: &Path, args: &[&str]) -> Output {
    crate::common::af()
        .args(["onboard", "--repo", repo.to_str().unwrap()])
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repository directory whose name is not UTF-8, or `None` where the filesystem refuses to
/// hold one. The constraint is the filesystem's own encoding rule, not the platform: APFS
/// rejects with `EILSEQ` the byte that ext4 stores without complaint. Skipping there keeps a
/// Mac from failing these tests against a limitation of its disk rather than anything in `af`;
/// Linux CI still exercises them.
fn non_utf8_repository(root: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    let path = root.join(std::ffi::OsString::from_vec(b"repo-\xff".to_vec()));
    std::fs::create_dir(&path).ok().map(|()| path)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn preview_detects_gate_and_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    std::fs::write(repo.join("Makefile"), "check:\n\t@true\n").unwrap();

    let output = af(&repo, &["--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "preview");
    assert!(
        report["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step.as_str().unwrap().contains("af setup agents --write")),
        "{report}"
    );
    assert_eq!(report["gates"][0]["program"], "make");
    assert_eq!(report["reviewers"].as_array().unwrap().len(), 2);
    assert!(
        report["topology"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line == "correctness.result -> gather.correctness")
    );
    // A source build (this test binary) pins no af release, and the preview says so.
    let warnings = report["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].as_str().unwrap().contains("no install receipt"),
        "{warnings:?}"
    );
    assert!(!repo.join(".af").exists());

    let apply = report["next_steps"][0].as_str().unwrap();
    let command = apply.split('`').nth(1).unwrap();
    let words = shell_words::split(command).unwrap();
    assert!(
        words
            .windows(2)
            .any(|pair| pair == ["--gate", "check=make check"])
    );
    assert!(
        words
            .windows(2)
            .any(|pair| pair == ["--af", env!("CARGO_PKG_VERSION")]),
        "the printed command must preserve the executing release: {apply}"
    );
    assert_eq!(words.last().map(String::as_str), Some("--apply"));
}

#[test]
fn preview_preserves_explicit_gate_quoting_in_the_printed_apply_command() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());

    let output = af(
        &repo,
        &[
            "--runner",
            "codex",
            "--gate",
            "lint=sh -c 'echo ready'",
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let apply = report["next_steps"][0].as_str().unwrap();
    assert!(apply.contains("--runner codex"), "{apply}");
    let command = apply.split('`').nth(1).unwrap();
    let words = shell_words::split(command).unwrap();
    let gate = words
        .windows(2)
        .find(|pair| pair[0] == "--gate")
        .map(|pair| pair[1].as_str());
    assert_eq!(
        gate,
        Some("lint=sh -c 'echo ready'"),
        "the printed command must be shell-copyable and semantically exact: {apply}"
    );
    assert_eq!(words.last().map(String::as_str), Some("--apply"));
}

#[test]
fn preview_preserves_the_explicit_af_release_in_the_printed_apply_command() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());

    let output = af(
        &repo,
        &[
            "--gate",
            "check=true",
            "--af",
            env!("CARGO_PKG_VERSION"),
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let apply = report["next_steps"][0].as_str().unwrap();
    let command = apply.split('`').nth(1).unwrap();
    let words = shell_words::split(command).unwrap();
    assert!(
        words
            .windows(2)
            .any(|pair| pair == ["--af", env!("CARGO_PKG_VERSION")]),
        "the copied apply must run under the release that produced the preview: {apply}"
    );
}

#[test]
fn preview_rejects_a_repository_path_that_cannot_be_copied_as_utf8() {
    let root = tempfile::tempdir().unwrap();
    let Some(repo) = non_utf8_repository(root.path()) else {
        return;
    };
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    // A `.git` the kernel accepts as a repository holds a HEAD.
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let output = crate::common::af()
        .arg("onboard")
        .arg("--repo")
        .arg(&repo)
        .args(["--gate", "check=true", "--json"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("not valid UTF-8"),
        "{}",
        stderr(&output)
    );
    assert!(!repo.join(".af").exists());
}

#[test]
fn existing_authority_can_be_validated_under_a_non_utf8_repository_path() {
    let root = tempfile::tempdir().unwrap();
    let source = repo(root.path());
    std::fs::write(source.join("Makefile"), "check:\n\t@true\n").unwrap();
    let created = af(&source, &["--apply", "--json"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let Some(target) = non_utf8_repository(root.path()) else {
        return;
    };
    std::fs::remove_dir(&target).unwrap();
    std::fs::rename(&source, &target).unwrap();

    let output = crate::common::af()
        .arg("onboard")
        .args(["--repo", "."])
        .arg("--json")
        .current_dir(&target)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "onboarded");
}

#[test]
fn preview_rejects_a_non_utf8_canonical_repository_reached_as_dot() {
    let root = tempfile::tempdir().unwrap();
    let Some(repo) = non_utf8_repository(root.path()) else {
        return;
    };
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    // A `.git` the kernel accepts as a repository holds a HEAD.
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let output = crate::common::af()
        .args(["onboard", "--repo", ".", "--gate", "check=true", "--json"])
        .current_dir(&repo)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("not valid UTF-8"),
        "{}",
        stderr(&output)
    );
    assert!(!repo.join(".af").exists());
}

#[test]
fn apply_creates_valid_authority_and_never_overwrites_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());

    let output = af(
        &repo,
        &[
            "--runner",
            "codex",
            "--gate",
            "check=make check",
            "--apply",
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "created");
    assert!(repo.join(".af/README.md").is_file());
    let readme = std::fs::read_to_string(repo.join(".af/README.md")).unwrap();
    assert!(readme.contains("## Worker data authorization"));
    assert!(readme.contains("should not ask for additional per-Worker"));
    assert!(readme.contains("Plain `af review run` is light"));
    assert!(readme.contains("Do not open a follow-up review Campaign"));
    assert!(readme.contains("human-requested `--heavy`"));
    // The guide names the whole `--refresh-lock` scope: route and oversized targets too, and
    // the stale pins it drops.
    let guide = readme.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        guide.contains(
            "re-pins the selected pipeline, every pipeline a route or the oversized policy names, \
             and the Worker packages they reference; it drops a pin whose"
        ),
        "{readme}"
    );
    assert!(
        !guide.contains("recomputes only the selected pipeline"),
        "{readme}"
    );
    assert!(repo.join(".af/pipelines/review.toml").is_file());
    let pipeline = std::fs::read_to_string(repo.join(".af/pipelines/review.toml")).unwrap();
    assert_eq!(pipeline.matches("demands = \"required\"").count(), 2);
    let pipeline_value: toml::Value = toml::from_str(&pipeline).unwrap();
    assert_eq!(pipeline_value["version"].as_integer(), Some(3));
    assert_eq!(
        pipeline_value["gate"]["provider"].as_str(),
        Some("trusted_local")
    );
    assert_eq!(
        pipeline_value["gate"]["required_isolation"].as_str(),
        Some("none")
    );
    assert_eq!(
        pipeline_value["gate"]["mode"].as_str(),
        Some("ephemeral-write")
    );
    assert!(repo.join(".af/workers/correctness/reviewer.md").is_file());
    let architecture_manifest =
        std::fs::read_to_string(repo.join(".af/workers/architecture/reviewer.toml")).unwrap();
    assert!(architecture_manifest.contains("program = \"codex\""));
    let architecture_manifest: toml::Value = toml::from_str(&architecture_manifest).unwrap();
    let architecture_args = architecture_manifest["runner"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg["value"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        architecture_args,
        [
            "--model",
            "gpt-6-sol",
            "-c",
            "model_reasoning_effort=\"high\""
        ],
        "generated Codex authority must select an admissible model explicitly"
    );

    let validate = af(&repo, &["--json"]);
    assert!(validate.status.success(), "{}", stderr(&validate));
    let validated: serde_json::Value = serde_json::from_slice(&validate.stdout).unwrap();
    assert_eq!(validated["status"], "onboarded");

    let second_apply = af(&repo, &["--apply"]);
    assert!(!second_apply.status.success());
    assert!(stderr(&second_apply).contains("never overwrites"));
}

#[test]
fn tampering_fails_closed_and_explicit_refresh_prunes_absent_pins() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    let create = af(&repo, &["--gate", "check=make check", "--apply"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let lock_path = repo.join(".af/af.lock");
    let mut lock = Lockfile::from_toml(&std::fs::read_to_string(&lock_path).unwrap()).unwrap();
    let unrelated = Pin {
        version: "9.9.9".into(),
        digest: format!("sha256:{}", "0".repeat(64)),
    };
    lock.workers.insert("unused".into(), unrelated.clone());
    std::fs::write(&lock_path, lock.to_toml()).unwrap();
    let pipeline_path = repo.join(".af/pipelines/review.toml");
    let mut pipeline: toml::Value =
        toml::from_str(&std::fs::read_to_string(&pipeline_path).unwrap()).unwrap();
    pipeline.as_table_mut().unwrap().remove("budgets");
    std::fs::write(&pipeline_path, toml::to_string_pretty(&pipeline).unwrap()).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(repo.join(".af/workers/correctness/reviewer.md"))
        .unwrap()
        .write_all(b"\nproject addition\n")
        .unwrap();

    let validate = af(&repo, &[]);
    assert!(!validate.status.success());
    assert!(stderr(&validate).contains("stale Worker pin `unused`"));

    let refresh = af(&repo, &["--refresh-lock", "--json"]);
    assert!(refresh.status.success(), "{}", stderr(&refresh));
    let report: serde_json::Value = serde_json::from_slice(&refresh.stdout).unwrap();
    assert_eq!(report["status"], "lock_refreshed");
    assert!(report["attempt_tokens"].is_null());
    assert!(report["run_tokens"].is_null());
    let refreshed = Lockfile::from_toml(&std::fs::read_to_string(&lock_path).unwrap()).unwrap();
    assert!(!refreshed.workers.contains_key("unused"));

    let validate = af(&repo, &[]);
    assert!(validate.status.success(), "{}", stderr(&validate));
}

#[test]
fn absent_gate_is_a_refusal_and_help_is_self_contained() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    let refused = af(&repo, &["--apply"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("no unambiguous acceptance Gate"));
    assert!(!repo.join(".af").exists());

    let help = crate::common::af()
        .args(["onboard", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success(), "{}", stderr(&help));
    assert!(stdout(&help).contains("never calls a model"));
    assert!(stdout(&help).contains("does not ask for per-call confirmation"));
    assert!(stdout(&help).contains("Review Campaigns are light by default"));
    assert!(stdout(&help).contains("Use `--heavy` only when a human"));
    assert!(stdout(&help).contains("--refresh-lock"));
    // `--refresh-lock` re-pins every configured pipeline, not only the selected one, and drops
    // pins whose package or pipeline file is gone; the help says so.
    let text = stdout(&help)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        text.contains(
            "--refresh-lock: explicitly re-pin the selected pipeline, every pipeline a route or \
             the oversized policy names, and the Worker packages they reference; it drops a pin \
             whose `.af/workers/NAME` package or `.af/pipelines/NAME.toml` file no longer exists."
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "Repin the selected, routed and oversized pipelines and their Workers; drop stale pins"
        ),
        "{text}"
    );
    assert!(
        !text.contains("recompute only the selected pipeline"),
        "{text}"
    );
}

/// A Provider registry naming `providers` (ID, kind), each with an auth directory of its own,
/// at `root/providers.toml`; `AF_PROVIDERS_FILE` points `af` at it.
fn registry(root: &Path, providers: &[(&str, &str)]) -> PathBuf {
    std::fs::create_dir_all(root).unwrap();
    let mut text = "version = 1\n".to_string();
    for (id, kind) in providers {
        let auth = root.join(format!("auth-{id}"));
        std::fs::create_dir_all(&auth).unwrap();
        text.push_str(&format!(
            "[[providers]]\nid = \"{id}\"\nkind = \"{kind}\"\nauth_dir = {:?}\n",
            auth.canonicalize().unwrap().to_str().unwrap()
        ));
    }
    let path = root.join("providers.toml");
    std::fs::write(&path, text).unwrap();
    path
}

/// `af onboard` with the registry at `providers` and a PATH holding no Provider CLI, so a probe
/// finds none and no ambient login appears.
fn onboard_with(repo: &Path, providers: &Path, args: &[&str]) -> Output {
    let empty = repo.parent().unwrap().join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    crate::common::af()
        .args(["onboard", "--repo", repo.to_str().unwrap()])
        .args(args)
        .env("AF_PROVIDERS_FILE", providers)
        .env("PATH", &empty)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .output()
        .unwrap()
}

/// The lines of one block of the text preview: those indented under `name` up to the next.
fn block(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != name)
        .skip(1)
        .take_while(|line| line.starts_with("  "))
        .map(|line| line.trim().to_owned())
        .collect()
}

#[test]
fn the_providers_block_and_the_runner_follow_the_registered_kinds() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    std::fs::write(repo.join("Makefile"), "check:\n\t@true\n").unwrap();

    // None registered: mixed, and every model Worker is told how to register its kind.
    let none = registry(&root.path().join("none"), &[]);
    let output = onboard_with(&repo, &none, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("profile     multi-review@1 · runner mixed (no Provider is registered on this machine yet; see providers below; pass --runner to change)"),
        "{text}"
    );
    // The block stands between the reviewers and the gates.
    let order: Vec<&str> = text
        .lines()
        .filter(|line| ["reviewers", "providers", "gates"].contains(line))
        .collect();
    assert_eq!(order, ["reviewers", "providers", "gates"], "{text}");
    assert_eq!(
        block(&text, "providers"),
        [
            "correctness needs a claude Provider: none registered   ← af provider detect, or af provider setup claude-main --kind claude --login",
            "architecture needs a codex Provider: none registered   ← af provider detect, or af provider setup codex-main --kind codex --login",
        ]
    );
    assert!(
        text.contains("Register a Provider if the providers block says none: af provider detect"),
        "{text}"
    );

    // One kind: the runner follows it, and each Worker is bound to its Provider.
    let codex = registry(&root.path().join("codex"), &[("codex-main", "codex")]);
    let output = onboard_with(&repo, &codex, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("runner codex (the only Provider kind registered on this machine; pass --runner to change)"),
        "{text}"
    );
    assert_eq!(
        block(&text, "providers"),
        [
            "correctness needs a codex Provider: codex-main (registered; --probe checks its login)   ← bind with --provider correctness=codex-main",
            "architecture needs a codex Provider: codex-main (registered; --probe checks its login)   ← bind with --provider architecture=codex-main",
        ]
    );
    assert!(text.contains(" --runner codex "), "{text}");
    assert!(!text.contains("Register a Provider"), "{text}");
    let report: serde_json::Value =
        serde_json::from_slice(&onboard_with(&repo, &codex, &["--json"]).stdout).unwrap();
    assert_eq!(report["reviewers"][0]["runner"], "codex");
    // The text-only block leaves the JSON document's shape as it was.
    assert!(report.get("providers").is_none(), "{report}");

    // Both kinds: mixed, each Worker bound to the Provider of its own kind.
    let both = registry(
        &root.path().join("both"),
        &[("claude-work", "claude"), ("codex-main", "codex")],
    );
    let output = onboard_with(&repo, &both, &[]);
    let text = stdout(&output);
    assert!(
        text.contains("runner mixed (claude and codex Providers are both registered on this machine; pass --runner to change)"),
        "{text}"
    );
    assert_eq!(
        block(&text, "providers"),
        [
            "correctness needs a claude Provider: claude-work (registered; --probe checks its login)   ← bind with --provider correctness=claude-work",
            "architecture needs a codex Provider: codex-main (registered; --probe checks its login)   ← bind with --provider architecture=codex-main",
        ]
    );

    // --probe asks the status probe; with no CLI on PATH it says so instead of guessing.
    let probed = onboard_with(&repo, &both, &["--probe"]);
    assert!(probed.status.success(), "{}", stderr(&probed));
    let lines = block(&stdout(&probed), "providers");
    assert!(
        lines[0]
            .starts_with("correctness needs a claude Provider: claude-work (installation failed)"),
        "{lines:#?}"
    );

    // An explicit --runner wins, and the block names what that choice needs.
    let output = onboard_with(&repo, &codex, &["--runner", "claude"]);
    let text = stdout(&output);
    assert!(text.contains("runner claude (--runner)"), "{text}");
    assert!(
        block(&text, "providers")[0]
            .starts_with("correctness needs a claude Provider: none registered"),
        "{text}"
    );
    assert!(!repo.join(".af").exists());
}

#[test]
fn apply_and_validate_say_what_to_register_and_fill_the_plan_line() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    let none = registry(&root.path().join("none"), &[]);
    let applied = onboard_with(&repo, &none, &["--gate", "check=true", "--apply"]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    let text = stdout(&applied);
    assert!(
        text.contains("Register a Provider if the providers block says none: af provider detect"),
        "{text}"
    );
    let guide = std::fs::read_to_string(repo.join(".af/README.md")).unwrap();
    let collapsed = guide.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        collapsed.contains("2. Run `af provider status`; if a Worker's kind has no Provider, `af provider detect` or `af provider setup <id> --kind <kind> --login` at your terminal."),
        "{guide}"
    );
    // The private-host protocol is the closing note for agents, not step 2.
    let (steps, note) = guide.split_once("## Agents without a terminal").unwrap();
    assert!(!steps.contains("af provider auth"), "{guide}");
    assert!(note.contains("af provider auth"), "{guide}");

    // Validation with both kinds registered fills the token-free plan line.
    let both = registry(
        &root.path().join("both"),
        &[("claude-work", "claude"), ("codex-main", "codex")],
    );
    let validated = onboard_with(&repo, &both, &[]);
    assert!(validated.status.success(), "{}", stderr(&validated));
    let text = stdout(&validated);
    assert!(
        text.contains("`af review plan --policy-rev HEAD --base HEAD --uncommitted --provider architecture=codex-main --provider correctness=claude-work`"),
        "{text}"
    );
    assert!(!text.contains("Register a Provider"), "{text}");
    // Without them it keeps placeholders and says how to register.
    let text = stdout(&onboard_with(&repo, &none, &[]));
    assert!(
        text.contains("--provider architecture=<codex-provider-id> --provider correctness=<claude-provider-id>"),
        "{text}"
    );
    assert!(
        text.contains("Register a Provider if the providers block says none"),
        "{text}"
    );
}

#[test]
fn a_failing_checksum_fetch_says_what_happened_and_what_the_lock_records() {
    use crate::common::{FAKE_CURL, Sandbox, executable, tool_dir};
    let sandbox = Sandbox::new();
    let real = sandbox.adopt_real_binary();
    let repo = repo(&sandbox.path("work"));
    // GitHub refuses the anonymous route, and gh is installed but not logged in.
    let tools = tool_dir(&sandbox.path("fakes"), &["sh", "git", "cp", "basename"]);
    executable(&tools.join("curl"), FAKE_CURL);
    executable(
        &tools.join("gh"),
        "#!/bin/sh\necho 'To get started with GitHub CLI, please run:  gh auth login' >&2\nexit 4\n",
    );
    let output = sandbox
        .command(&real)
        .args([
            "onboard",
            "--repo",
            repo.to_str().unwrap(),
            "--gate",
            "check=true",
            "--json",
        ])
        .env_remove("AF_RELEASE_SOURCE")
        .env("AF_SELF_OFFLINE", "")
        // No detached update check: the subject is the lock's own fetch.
        .env("CI", "1")
        .env("PATH", &tools)
        .env("FAKE_CURL_STATUS", "404")
        .env("FAKE_CURL_LOG", sandbox.path("curl.log"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let warnings: Vec<&str> = report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning.as_str().unwrap())
        .collect();
    let version = env!("CARGO_PKG_VERSION");
    let target = env!("AF_TARGET");
    let warning = warnings
        .iter()
        .find(|warning| warning.starts_with("could not fetch SHA256SUMS"))
        .unwrap_or_else(|| panic!("{warnings:#?}"));
    assert!(
        warning.starts_with(&format!("could not fetch SHA256SUMS of v{version} (")),
        "{warning}"
    );
    assert!(warning.contains("gh auth login"), "{warning}");
    assert!(
        warning.contains("the anonymous route failed: HTTP 404"),
        "{warning}"
    );
    assert!(
        warning.contains(&format!("); the lock records this target only ({target})")),
        "{warning}"
    );
    assert!(!warning.contains("has no SHA256SUMS"), "{warning}");
    assert_eq!(report["lock_af_targets"], serde_json::json!([target]));
}

#[test]
fn a_build_without_a_receipt_names_the_receipt_path_and_the_install() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    let output = af(&repo, &["--gate", "check=true", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let warning = report["warnings"][0].as_str().unwrap();
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        warning.contains(&format!(
            "no install receipt at {}",
            crate::common::private_home_env()
                .into_iter()
                .find(|(name, _)| *name == "XDG_DATA_HOME")
                .unwrap()
                .1
                .join(format!("af/versions/{version}/receipt.toml"))
                .display()
        )),
        "{warning}"
    );
    assert!(
        warning.contains(&format!("af self install {version}")),
        "{warning}"
    );
}

/// `af help start` without a terminal, with the registry at `providers` and no Provider CLI on
/// PATH.
fn help_start_with(root: &Path, providers: &Path) -> String {
    let empty = root.join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let output = crate::common::af()
        .args(["help", "start"])
        .env("AF_PROVIDERS_FILE", providers)
        .env("PATH", &empty)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    for line in text.lines() {
        assert!(line.chars().count() <= 100, "`{line}`\n{text}");
    }
    text
}

/// GA final fix item 4: `af help start` binds the first review to the Providers `af onboard`'s
/// preview selects on this machine, and with none registered prints the PROVIDER placeholder
/// and says why.
#[test]
fn help_start_binds_the_review_as_the_preview_does() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    std::fs::write(repo.join("Makefile"), "check:\n\t@true\n").unwrap();
    for (name, providers, bound) in [
        (
            "codex",
            &[("codex-main", "codex")][..],
            "--provider correctness=codex-main --provider architecture=codex-main",
        ),
        (
            "claude",
            &[("claude-main", "claude")][..],
            "--provider correctness=claude-main --provider architecture=claude-main",
        ),
        (
            "both",
            &[("claude-work", "claude"), ("codex-main", "codex")][..],
            "--provider correctness=claude-work --provider architecture=codex-main",
        ),
    ] {
        let registry = registry(&root.path().join(name), providers);
        let text = help_start_with(root.path(), &registry);
        assert!(
            text.contains(&format!(
                "     af review --policy-rev HEAD --base HEAD --uncommitted \\\n       {bound}\n"
            )),
            "{name}:\n{text}"
        );
        assert!(
            text.contains("These are the Providers registered on this machine that `af onboard`"),
            "{name}:\n{text}"
        );
        assert!(!text.contains("PROVIDER"), "{name}:\n{text}");
        // The same bindings the preview prints, Worker by Worker.
        let preview = stdout(&onboard_with(&repo, &registry, &[]));
        for binding in bound
            .split(" --provider ")
            .map(|b| b.trim_start_matches("--provider "))
        {
            assert!(
                preview.contains(&format!("← bind with --provider {binding}")),
                "{name}: {binding}\n{preview}"
            );
        }
    }

    let none = registry(&root.path().join("none"), &[]);
    let text = help_start_with(root.path(), &none);
    assert!(
        text.contains(
            "       --provider correctness=PROVIDER --provider architecture=PROVIDER\n   No \
             Provider is registered on this machine yet, so PROVIDER stands for the ID step 1\n   \
             registers; `af onboard` prints the exact command once one is.\n"
        ),
        "{text}"
    );
    assert!(!text.contains("claude-main --provider"), "{text}");
}

fn committed_repo(root: &Path) -> PathBuf {
    let repo = root.join("task-repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Task fixture"],
        vec!["config", "user.email", "task@example.invalid"],
    ] {
        let output = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }
    std::fs::write(repo.join("Makefile"), "check:\n\t@true\n").unwrap();
    commit(&repo);
    repo
}

fn commit(repo: &Path) {
    for args in [vec!["add", "-A"], vec!["commit", "-qm", "authority"]] {
        let output = std::process::Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }
}

fn task_onboard(repo: &Path, registry: &Path, args: &[&str]) -> Output {
    crate::common::af()
        .args(["onboard", "--repo", repo.to_str().unwrap(), "--tasks"])
        .args(args)
        .env("AF_PROVIDERS_FILE", registry)
        .output()
        .unwrap()
}

#[test]
fn task_preview_and_new_bundle_have_exact_pins_and_plan_after_commit() {
    let root = tempfile::tempdir().unwrap();
    let repo = committed_repo(root.path());
    let provider = registry(&root.path().join("provider"), &[("codex-main", "codex")]);
    let preview = task_onboard(&repo, &provider, &["--json"]);
    assert!(preview.status.success(), "{}", stderr(&preview));
    let document: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("onboard-tasks-v1.json"),
        &document,
    );
    assert_eq!(document["status"], "preview");
    assert!(
        document["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == ".af/task-catalog.toml")
    );
    assert!(!repo.join(".af").exists());

    let none = registry(&root.path().join("none"), &[]);
    let without_provider = task_onboard(&repo, &none, &["--json"]);
    assert!(
        without_provider.status.success(),
        "{}",
        stderr(&without_provider)
    );
    let without_provider: serde_json::Value =
        serde_json::from_slice(&without_provider.stdout).unwrap();
    assert!(
        without_provider["next_steps"][0]
            .as_str()
            .unwrap()
            .contains("af provider setup codex-main --kind codex --login")
    );
    assert!(!repo.join(".af").exists());

    let applied = task_onboard(&repo, &provider, &["--apply", "--json"]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    let created: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("onboard-tasks-v1.json"),
        &created,
    );
    assert_eq!(created["status"], "created");
    for path in document["files"].as_array().unwrap() {
        assert!(repo.join(path.as_str().unwrap()).is_file(), "{path}");
    }
    let project = std::fs::read_to_string(repo.join(".af/af.toml")).unwrap();
    assert!(project.ends_with(document["append_text"].as_str().unwrap()));
    assert!(
        std::fs::read_to_string(repo.join(".af/README.md"))
            .unwrap()
            .contains("## Model-driven Tasks")
    );
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("task-catalog-v2.json"),
        &serde_json::to_value(&catalog).unwrap(),
    );
    let policy: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/code-policy.toml")).unwrap())
            .unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("code-task-policy-v1.json"),
        &serde_json::to_value(&policy).unwrap(),
    );
    for (name, pin) in catalog["packages"].as_table().unwrap() {
        assert_eq!(
            pin["digest"].as_str().unwrap(),
            review_config::lock::package_digest(name, &repo.join(pin["path"].as_str().unwrap()))
                .unwrap()
        );
    }
    commit(&repo);
    let ticket = root.path().join("ticket.json");
    std::fs::write(
        &ticket,
        r#"{"task_id":"onboard-task","kind":"implement","goal":"Document the check command"}"#,
    )
    .unwrap();
    let bin = root.path().join("bin");
    let auth = root.path().join("provider/auth-codex-main");
    crate::fake_models::install("codex", &bin, &auth);
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let plan = crate::common::af()
        .current_dir(&repo)
        .args([
            "task",
            "plan",
            "--file",
            ticket.to_str().unwrap(),
            "--json",
            "--state",
        ])
        .arg(root.path().join("state"))
        .env("AF_PROVIDERS_FILE", &provider)
        .env("USER", "fixture")
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        plan.status.success(),
        "{}\n{}",
        stdout(&plan),
        stderr(&plan)
    );
    assert!(
        !auth.join("turns").exists(),
        "planning must not dispatch a model"
    );
}

#[test]
fn additive_task_apply_preserves_review_bytes_and_refuses_conflicts() {
    let root = tempfile::tempdir().unwrap();
    let repo = committed_repo(root.path());
    let provider = registry(&root.path().join("provider"), &[("codex-main", "codex")]);
    let review = af(&repo, &["--apply"]);
    assert!(review.status.success(), "{}", stderr(&review));
    let project = std::fs::read(repo.join(".af/af.toml")).unwrap();
    let lock = std::fs::read(repo.join(".af/af.lock")).unwrap();
    let review_pipeline = std::fs::read(repo.join(".af/pipelines/review.toml")).unwrap();

    std::fs::write(repo.join(".af/code-policy.toml"), "occupied\n").unwrap();
    let refused = task_onboard(&repo, &provider, &["--apply"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("code-policy.toml"));
    assert!(!repo.join(".af/task-catalog.toml").exists());
    assert_eq!(std::fs::read(repo.join(".af/af.toml")).unwrap(), project);
    std::fs::remove_file(repo.join(".af/code-policy.toml")).unwrap();

    std::fs::write(root.path().join("outside"), "outside").unwrap();
    std::os::unix::fs::symlink(root.path().join("outside"), repo.join(".af/unrelated")).unwrap();
    let refused = task_onboard(&repo, &provider, &["--apply"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains(".af/unrelated"),
        "{}",
        stderr(&refused)
    );
    assert!(!repo.join(".af/task-catalog.toml").exists());
    assert_eq!(std::fs::read(repo.join(".af/af.toml")).unwrap(), project);
    std::fs::remove_file(repo.join(".af/unrelated")).unwrap();

    let created = task_onboard(&repo, &provider, &["--apply"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let appended = std::fs::read(repo.join(".af/af.toml")).unwrap();
    assert!(appended.starts_with(&project));
    assert_eq!(std::fs::read(repo.join(".af/af.lock")).unwrap(), lock);
    assert_eq!(
        std::fs::read(repo.join(".af/pipelines/review.toml")).unwrap(),
        review_pipeline
    );
    let refused = task_onboard(&repo, &provider, &["--apply"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("[task]"));
    assert_eq!(std::fs::read(repo.join(".af/af.toml")).unwrap(), appended);

    commit(&repo);
    let ticket = root.path().join("existing-ticket.json");
    std::fs::write(
        &ticket,
        r#"{"task_id":"existing-task","kind":"implement","goal":"Document the check command"}"#,
    )
    .unwrap();
    let bin = root.path().join("bin");
    crate::fake_models::install("codex", &bin, &root.path().join("provider/auth-codex-main"));
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let plan = crate::common::af()
        .current_dir(&repo)
        .args([
            "task",
            "plan",
            "--file",
            ticket.to_str().unwrap(),
            "--json",
            "--state",
        ])
        .arg(root.path().join("existing-state"))
        .env("AF_PROVIDERS_FILE", &provider)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        plan.status.success(),
        "{}\n{}",
        stdout(&plan),
        stderr(&plan)
    );

    let other = root.path().join("table-repo");
    std::fs::create_dir_all(other.join(".git")).unwrap();
    std::fs::create_dir_all(other.join(".af")).unwrap();
    std::fs::write(other.join(".af/af.toml"), "version = 1\n[task]\n").unwrap();
    std::fs::write(other.join("Makefile"), "check:\n\t@true\n").unwrap();
    let refused = task_onboard(&other, &provider, &["--apply"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("[task]"));
    assert!(!other.join(".af/task-catalog.toml").exists());
}

#[test]
fn task_onboarding_uses_a_registered_claude_kind_with_distinct_models() {
    let root = tempfile::tempdir().unwrap();
    let repo = committed_repo(root.path());
    let provider = registry(&root.path().join("provider"), &[("claude-main", "claude")]);
    let applied = task_onboard(&repo, &provider, &["--apply", "--json"]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    let report: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(report["provider_kind"], "claude");
    let implementer =
        std::fs::read_to_string(repo.join(".af/task-packages/onboard/implementer/worker.toml"))
            .unwrap();
    let reviewer =
        std::fs::read_to_string(repo.join(".af/task-packages/onboard/bugs/worker.toml")).unwrap();
    assert!(implementer.contains("model = \"claude-opus-5-5\""));
    assert!(reviewer.contains("model = \"claude-sonnet-5-5\""));
    commit(&repo);

    let ticket = root.path().join("claude-ticket.json");
    std::fs::write(
        &ticket,
        r#"{"task_id":"claude-task","kind":"implement","goal":"Document the check command"}"#,
    )
    .unwrap();
    let bin = root.path().join("bin");
    let auth = root.path().join("provider/auth-claude-main");
    crate::fake_models::install("claude", &bin, &auth);
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let plan = crate::common::af()
        .current_dir(&repo)
        .args([
            "task",
            "plan",
            "--file",
            ticket.to_str().unwrap(),
            "--json",
            "--state",
        ])
        .arg(root.path().join("state"))
        .env("AF_PROVIDERS_FILE", &provider)
        .env("USER", "fixture")
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        plan.status.success(),
        "{}\n{}",
        stdout(&plan),
        stderr(&plan)
    );
    assert!(!auth.join("turns").exists());
}

#[test]
fn task_onboarding_uses_both_registered_provider_aliases_in_mixed_profile() {
    let root = tempfile::tempdir().unwrap();
    let repo = committed_repo(root.path());
    let provider = registry(
        &root.path().join("provider"),
        &[("claude-main", "claude"), ("codex-main", "codex")],
    );
    let applied = task_onboard(&repo, &provider, &["--apply", "--json"]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    let report: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    crate::schemas::valid(&crate::schemas::validator("onboard-tasks-v1.json"), &report);
    assert_eq!(report["provider_kind"], "mixed");
    let catalog = std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap();
    assert!(catalog.contains("\"onboard/implementer\" = \"claude-main\""));
    assert!(catalog.contains("\"onboard/bugs\" = \"codex-main\""));
    let implementer =
        std::fs::read_to_string(repo.join(".af/task-packages/onboard/implementer/worker.toml"))
            .unwrap();
    let reviewer =
        std::fs::read_to_string(repo.join(".af/task-packages/onboard/bugs/worker.toml")).unwrap();
    assert!(implementer.contains("provider_kind = \"claude\""));
    assert!(reviewer.contains("provider_kind = \"codex\""));
}

#[test]
fn committed_onboard_task_runs_with_fake_model_workers() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let repo = committed_repo(root.path());
    let provider = registry(&root.path().join("provider"), &[("codex-main", "codex")]);
    let applied = task_onboard(&repo, &provider, &["--apply"]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    commit(&repo);
    let ticket = root.path().join("run-ticket.json");
    std::fs::write(
        &ticket,
        r#"{"task_id":"onboard-run","kind":"implement","goal":"Add fixture.txt with the word done"}"#,
    )
    .unwrap();
    let bin = root.path().join("bin");
    let auth = root.path().join("provider/auth-codex-main");
    crate::fake_models::install("codex", &bin, &auth);
    let script = r#"#!/usr/bin/python3
import json,os,shutil,sys
home=os.environ['CODEX_HOME']
if sys.argv[1:]==['--version']:
 print('codex-cli 0.162.1'); sys.exit(0)
if sys.argv[1:3]==['login','status']:
 print('Logged in using ChatGPT'); sys.exit(0)
if sys.argv[1:]==['app-server']:
 for line in sys.stdin:
  message=json.loads(line)
  method=message.get('method')
  if method=='initialize': answer={'id':message['id'],'result':{}}
  elif method=='model/list':
   shutil.copyfile(home+'/codex-models-cache.json',home+'/models_cache.json')
   answer={'id':message['id'],'result':json.load(open(home+'/codex-model-list.json'))}
  elif method=='account/read':
   answer={'id':message['id'],'result':{'account':{'type':'chatgpt','email':'fixture@example.invalid'}}}
  else: continue
  print(json.dumps(answer),flush=True)
 sys.exit(0)
request=sys.stdin.read()
if request==PROBE_INPUT:
 message='OK'
else:
 r=json.loads(request)
 if 'report' in r.get('output_schemas',{}):
  open('fixture.txt','w').write('done\n')
  outputs={'report':[{'summary':'Added fixture.txt'}]}
 elif 'Goal evaluator' in str(r):
  outputs={'result':[{'outcome':'passed','reason':'The fixture exists and the check passed.'}]}
 else:
  outputs={'result':[{'reports':[],'benchmark_demands':[],'dispositions':[]}]}
 message=json.dumps({'schema':'af.worker-reply/1','outputs':outputs})
 with open(home+'/turns','a') as f: f.write('worker\n')
if '-o' in sys.argv:
 open(sys.argv[sys.argv.index('-o')+1],'w').write(message)
print(json.dumps({'type':'thread.started','thread_id':'synthetic-onboard'}))
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':message}}))
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':10,'cached_input_tokens':0,'output_tokens':2,'reasoning_output_tokens':0,'cache_write_input_tokens':0}}))
"#;
    let code = crate::provider_cli::with_help("codex", &crate::provider_cli::with_probe(script));
    std::fs::write(bin.join("codex"), code).unwrap();
    std::fs::set_permissions(bin.join("codex"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let run = crate::common::af()
        .current_dir(&repo)
        .args([
            "task",
            "start",
            "--execute",
            "--file",
            ticket.to_str().unwrap(),
            "--json",
            "--state",
        ])
        .arg(root.path().join("state"))
        .env("AF_PROVIDERS_FILE", &provider)
        .env("USER", "fixture")
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}\n{}", stdout(&run), stderr(&run));
    let result: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(result["result"]["acceptance"], "satisfied", "{result:#}");
    assert!(
        auth.join("turns").exists(),
        "the Task did not call a Worker"
    );
}
