//! Binary-owned, token-free onboarding: preview first, atomic creation, then exact validation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Output;

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
        report["topology"].as_array().unwrap().iter().any(
            |line| line == "checks -> bugs, correctness -> reduce -> review, findings, history"
        )
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

/// The words of the command a next step of the text preview continues over its lines, the one
/// whose first line starts `prefix` under its step: as a shell reads it pasted.
fn step_command(text: &str, prefix: &str) -> Vec<String> {
    let mut lines = text
        .lines()
        .skip_while(|line| !line.trim_start().starts_with(prefix));
    let mut command = lines.next().unwrap_or_default().to_owned();
    for line in lines {
        if !command.ends_with('\\') {
            break;
        }
        command.push('\n');
        command.push_str(line);
    }
    shell_words::split(&command).unwrap()
}

/// The rows `text` takes on a terminal `columns` wide: a line wider than the terminal wraps onto
/// as many rows as it needs.
fn display_rows(text: &str, columns: usize) -> usize {
    text.lines()
        .map(|line| line.chars().count().div_ceil(columns).max(1))
        .sum()
}

/// The rows the preview `text` takes on an 80-column terminal where the repository sits at
/// `/tmp/repo` instead of `path`. The path is as long as the host's temp root, not the
/// preview's: its `repository` line takes the placeholder, and the apply command, which the
/// preview continues between its arguments and cuts inside a path wider than a line, is laid
/// out again with it as `fit_command` lays a command: its words after a five-column label while
/// each leaves room for ` \`, each continuation nine columns in.
fn rows_with_short_repo(text: &str, path: &str) -> usize {
    let mut rest = Vec::new();
    let mut lines = text.lines();
    let mut command = None;
    while let Some(line) = lines.next() {
        if command.is_some() || !line.trim_start().starts_with("af onboard ") {
            rest.push(line.replace(path, "/tmp/repo"));
            continue;
        }
        let mut block = line.to_owned();
        while block.ends_with('\\') {
            let Some(next) = lines.next() else { break };
            block.push('\n');
            block.push_str(next);
        }
        command = Some(shell_words::split(&block).unwrap());
    }
    let command_rows = command.map_or(0, |words| {
        let mut rows = 1;
        let mut current = 5;
        let mut fresh = true;
        for word in words {
            let word = if word == path {
                "/tmp/repo".to_owned()
            } else {
                word
            };
            let width = shell_words::quote(&word).chars().count();
            let space = usize::from(!fresh);
            if current + space + width + 2 <= 80 {
                current += space + width;
            } else {
                rows += 1;
                current = 9 + width;
            }
            fresh = false;
        }
        rows
    });
    display_rows(&rest.join("\n"), 80) + command_rows
}

/// The preview of a source build (no install receipt) on a machine with no Provider registered,
/// its longest case, reads on one 80x24 terminal: no line wraps and it takes at most 24 rows.
#[test]
fn default_preview_fits_one_screen_and_long_shows_topology() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    std::fs::write(repo.join("Makefile"), "check:\n\t@true\n").unwrap();
    let none = registry(&root.path().join("none"), &[]);
    let short = onboard_with(&repo, &none, &[]);
    assert!(short.status.success(), "{}", stderr(&short));
    let text = stdout(&short);
    assert!(text.contains("none registered"), "{text}");
    assert!(text.contains("warning     no install receipt"), "{text}");
    let path = repo.canonicalize().unwrap().display().to_string();
    // Only the `repository` line, which prints the path whole, may be wider.
    for line in text.lines() {
        assert!(
            line.chars().count() <= 80 || line == format!("repository  {path}"),
            "{line:?}\n{text}"
        );
    }
    // The rows are counted with the repository at a short path: its length is the host's.
    let rows = rows_with_short_repo(&text, &path);
    assert!(rows <= 24, "{rows} rows at /tmp/repo:\n{text}");
    assert!(!text.contains("topology\n"), "{text}");
    assert!(text.contains("--long lists each"), "{text}");
    // The apply command, continued between its arguments, still runs as copied.
    let words = step_command(&text, "af onboard ");
    assert_eq!(
        words[..4],
        ["af", "onboard", "--repo", path.as_str()],
        "{text}"
    );
    assert_eq!(words.last().map(String::as_str), Some("--apply"), "{text}");
    let long = onboard_with(&repo, &none, &["--long"]);
    assert!(long.status.success(), "{}", stderr(&long));
    let long = stdout(&long);
    assert!(long.contains("topology\n"));
    assert!(long.contains("  .af/README.md"));
    assert!(
        long.contains("convergence 1 clean Round; 2 Round maximum"),
        "{long}"
    );
    assert!(long.contains("no install receipt at"), "{long}");
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
    assert!(readme.contains("## First Review"));
    for command in [
        "af review plan --file ../review.json",
        "af review ledger review --json",
        "af review report review --json",
        "af review resume review --confirm-plan PLAN_ID",
    ] {
        assert!(readme.contains(command), "{command}: {readme}");
    }
    assert!(readme.contains("one complete Round"));
    assert!(readme.contains("bounded successor Rounds"));
    assert!(!repo.join(".af/pipelines").exists());
    assert!(!repo.join(".af/workers").exists());
    let package = repo.join(".af/task-packages/onboard/review-code");
    let pipeline: af_contracts::task::pipeline::PipelineDefinition =
        af_pipeline_config::task::parse_task_pipeline(
            &std::fs::read_to_string(package.join("pipeline.toml")).unwrap(),
        )
        .unwrap();
    assert!(pipeline.contract.inputs.contains_key("base"));
    assert!(pipeline.contract.inputs.contains_key("continuation"));
    assert!(package.join("task.json").is_file());
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    assert_eq!(
        catalog["review"]["reviewers"]["bugs"].as_str(),
        Some("required")
    );
    assert_eq!(
        catalog["review"]["reviewers"]["correctness"].as_str(),
        Some("required")
    );
    for name in ["bugs", "correctness"] {
        let manifest: toml::Value = toml::from_str(
            &std::fs::read_to_string(
                repo.join(format!(".af/task-packages/onboard/{name}/worker.toml")),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["runner"]["provider_kind"].as_str(), Some("codex"));
        assert_eq!(manifest["runner"]["model"].as_str(), Some("gpt-6.1-terra"));
        assert_eq!(manifest["runner"]["effort"].as_str(), Some("high"));
    }

    let validate = af(&repo, &["--json"]);
    assert!(validate.status.success(), "{}", stderr(&validate));
    let validated: serde_json::Value = serde_json::from_slice(&validate.stdout).unwrap();
    assert_eq!(validated["status"], "onboarded");

    let second_apply = af(&repo, &["--apply"]);
    assert!(!second_apply.status.success());
    assert!(stderr(&second_apply).contains("never overwrites"));
}

#[test]
fn tampering_fails_closed_and_explicit_refresh_repins_current_packages() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    assert!(
        af(&repo, &["--gate", "check=make check", "--apply"])
            .status
            .success()
    );
    let lock_path = repo.join(".af/af.lock");
    let lock = std::fs::read(&lock_path).unwrap();
    let worker = repo.join(".af/task-packages/onboard/correctness/instructions.md");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&worker)
        .unwrap()
        .write_all(b"\nProject rubric.\n")
        .unwrap();
    let validate = af(&repo, &[]);
    assert!(!validate.status.success());
    assert!(
        stderr(&validate).contains("changed since it was locked"),
        "{}",
        stderr(&validate)
    );
    let refresh = af(&repo, &["--refresh-lock", "--json"]);
    assert!(refresh.status.success(), "{}", stderr(&refresh));
    assert_eq!(
        std::fs::read(&lock_path).unwrap(),
        lock,
        "release pin is unchanged"
    );
    assert!(af(&repo, &[]).status.success());
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    for (name, pin) in catalog["packages"].as_table().unwrap() {
        assert_eq!(
            pin["digest"].as_str().unwrap(),
            af_pipeline_config::lock::package_digest(
                name,
                &repo.join(pin["path"].as_str().unwrap())
            )
            .unwrap()
        );
    }
    // A valid native catalog need not pin an installed af release. Package refresh preserves
    // that absence instead of opportunistically adding a release pin or failing after re-pin.
    std::fs::remove_file(&lock_path).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&worker)
        .unwrap()
        .write_all(b"\nAnother project rubric.\n")
        .unwrap();
    let refreshed = af(&repo, &["--refresh-lock", "--json"]);
    assert!(refreshed.status.success(), "{}", stderr(&refreshed));
    assert!(!lock_path.exists());
    assert!(af(&repo, &[]).status.success());
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
    assert!(stdout(&help).contains("native Review Pipeline is light"));
    assert!(stdout(&help).contains("retired --heavy Campaign flag is refused"));
    assert!(stdout(&help).contains("--refresh-lock"));
    let text = stdout(&help)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("--refresh-lock re-pins intentional current local package edits in .af/task-catalog.toml"), "{text}");
    assert!(
        text.contains("Repin current local Task packages and the receipted af release"),
        "{text}"
    );
    assert!(!text.contains("re-pin the selected pipeline"), "{text}");
}

/// A Provider registry naming `providers` (ID, kind), each with an auth directory of its own,
/// at `root/providers.toml`; `AF_PROVIDERS_FILE` points `af` at it.
fn registry(root: &Path, providers: &[(&str, &str)]) -> PathBuf {
    std::fs::create_dir_all(root).unwrap();
    let mut text = "version = 4\n".to_string();
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
        text.contains("profile     native-review@1 · runner mixed (no Provider registered yet)"),
        "{text}"
    );
    // The block stands between the reviewers and the gates.
    let order: Vec<&str> = text
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|label| ["reviewers", "providers", "gates"].contains(label))
        .collect();
    assert_eq!(order, ["reviewers", "providers", "gates"], "{text}");
    // One gate takes the label's line.
    assert!(
        text.contains("\ngates       check: make check [detected Makefile target]\n"),
        "{text}"
    );
    assert_eq!(
        block(&text, "providers"),
        [
            "bugs needs a codex Provider: none registered",
            "← af provider detect, or af provider setup codex-main --kind codex --login",
            "correctness needs a claude Provider: none registered",
            "← af provider detect, or af provider setup claude-main --kind claude --login",
        ]
    );
    // The block names the fix in text; the document's next steps carry it for an agent.
    assert!(!text.contains("Register a Provider"), "{text}");
    let report: serde_json::Value =
        serde_json::from_slice(&onboard_with(&repo, &none, &["--json"]).stdout).unwrap();
    assert!(
        report["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step
                == "Register a Provider if the providers block says none: af provider detect"),
        "{report}"
    );

    // One kind: the runner follows it, and each Worker is bound to its Provider.
    let codex = registry(&root.path().join("codex"), &[("codex-main", "codex")]);
    let output = onboard_with(&repo, &codex, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("runner codex (the only Provider kind registered)"),
        "{text}"
    );
    assert_eq!(
        block(&text, "providers"),
        [
            "bugs needs a codex Provider: codex-main (--probe checks its login)",
            "← bind onboard/bugs=codex-main in --bindings FILE",
            "correctness needs a codex Provider: codex-main (--probe checks its login)",
            "← bind onboard/correctness=codex-main in --bindings FILE",
        ]
    );
    let command = step_command(&text, "af onboard ");
    assert!(
        command.windows(2).any(|pair| pair == ["--runner", "codex"]),
        "{text}"
    );
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
        text.contains("runner mixed (both Provider kinds registered)"),
        "{text}"
    );
    assert_eq!(
        block(&text, "providers"),
        [
            "bugs needs a codex Provider: codex-main (--probe checks its login)",
            "← bind onboard/bugs=codex-main in --bindings FILE",
            "correctness needs a claude Provider: claude-work (--probe checks its login)",
            "← bind onboard/correctness=claude-work in --bindings FILE",
        ]
    );

    // --probe asks the status probe; with no CLI on PATH it says so instead of guessing.
    let probed = onboard_with(&repo, &both, &["--probe"]);
    assert!(probed.status.success(), "{}", stderr(&probed));
    let lines = block(&stdout(&probed), "providers");
    assert!(
        lines[2]
            .starts_with("correctness needs a claude Provider: claude-work (installation failed)"),
        "{lines:#?}"
    );

    // An explicit --runner wins, and the block names what that choice needs.
    let output = onboard_with(&repo, &codex, &["--runner", "claude"]);
    let text = stdout(&output);
    assert!(text.contains("runner claude (--runner)"), "{text}");
    assert!(
        block(&text, "providers")[0].starts_with("bugs needs a claude Provider: none registered"),
        "{text}"
    );
    assert!(!repo.join(".af").exists());
}

#[test]
fn long_repository_paths_preserve_the_copyable_command_and_registered_runner() {
    let root = tempfile::tempdir().unwrap();
    let parent = root
        .path()
        .join(format!("workspace with spaces {}", "x".repeat(95)))
        .join(format!("nested {}", "y".repeat(83)));
    std::fs::create_dir_all(&parent).unwrap();
    let repo = repo(&parent).canonicalize().unwrap();
    std::fs::write(repo.join("Makefile"), "check:\n\t@true\n").unwrap();
    let providers = registry(&root.path().join("providers"), &[("codex-main", "codex")]);
    let output = onboard_with(&repo, &providers, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("\\\n"),
        "a long command must actually wrap: {text}"
    );
    let words = step_command(&text, "af onboard ");
    assert_eq!(&words[..2], ["af", "onboard"]);
    for expected in [
        ["--runner", "codex"],
        ["--repo", repo.to_str().unwrap()],
        ["--gate", "check=make check"],
    ] {
        assert!(
            words.windows(2).any(|pair| pair == expected),
            "{words:?}\n{text}"
        );
    }
    assert_eq!(words.last().map(String::as_str), Some("--apply"));
    assert!(
        text.contains("runner codex (the only Provider kind registered)"),
        "{text}"
    );
    assert!(
        !repo.join(".af").exists(),
        "preview must write no project authority"
    );
}

#[test]
fn command_words_remove_shell_continuations_and_preserve_quoted_literal_content() {
    let words = step_command(
        "  af onboard --runner \\\n    codex --repo 'a path' --gate 'check=printf literal\\n' --apply",
        "af onboard ",
    );
    assert_eq!(
        words,
        [
            "af",
            "onboard",
            "--runner",
            "codex",
            "--repo",
            "a path",
            "--gate",
            "check=printf literal\\n",
            "--apply"
        ]
    );
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
    assert!(guide.contains("`af provider detect`"));
    assert!(guide.contains("login runs at the operator's terminal"));
    assert!(guide.contains("--bindings FILE"));

    // Validation with both kinds registered fills the token-free plan line.
    let both = registry(
        &root.path().join("both"),
        &[("claude-work", "claude"), ("codex-main", "codex")],
    );
    let validated = onboard_with(&repo, &both, &[]);
    assert!(validated.status.success(), "{}", stderr(&validated));
    let text = stdout(&validated);
    assert_eq!(
        step_command(&text, "af review plan "),
        shell_words::split(
            "af review plan --file ../review.json --authority HEAD --base HEAD --uncommitted"
        )
        .unwrap(),
        "{text}"
    );
    assert!(!text.contains("Register a Provider"), "{text}");
    // Without them it keeps placeholders and says how to register.
    let text = stdout(&onboard_with(&repo, &none, &[]));
    assert_eq!(
        step_command(&text, "af review plan "),
        shell_words::split(
            "af review plan --file ../review.json --authority HEAD --base HEAD --uncommitted"
        )
        .unwrap()
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
fn help_start_uses_declared_native_task_authority_without_campaign_binding_flags() {
    let root = tempfile::tempdir().unwrap();
    for (name, providers) in [
        ("codex", &[("codex-main", "codex")][..]),
        ("claude", &[("claude-main", "claude")][..]),
        (
            "both",
            &[("claude-work", "claude"), ("codex-main", "codex")][..],
        ),
        ("none", &[][..]),
    ] {
        let registry = registry(&root.path().join(name), providers);
        let text = help_start_with(root.path(), &registry);
        assert!(
            text.contains("af review plan --file ../review.json"),
            "{name}: {text}"
        );
        assert!(
            text.contains("--authority selects this trusted commit"),
            "{name}: {text}"
        );
        assert!(
            text.contains("Task catalog pins the Pipeline and its Workers"),
            "{name}: {text}"
        );
        assert!(!text.contains("--provider"), "{name}: {text}");
        assert!(!text.contains("--campaign"), "{name}: {text}");
    }
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
    let mut arguments = vec!["--tasks"];
    arguments.extend_from_slice(args);
    onboard_with(repo, registry, &arguments)
}

#[test]
fn afactory_authority_plans_a_three_field_task_with_its_defaults() {
    let root = tempfile::tempdir().unwrap();
    let repo = committed_repo(root.path());
    let source = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join(".af");
    let copied = std::process::Command::new("cp")
        .args(["-R", source.to_str().unwrap(), repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(copied.status.success(), "{}", stderr(&copied));
    // Plan the shipped catalog as published, without rewriting its contracts or pins.
    let project: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/af.toml")).unwrap()).unwrap();
    assert_eq!(
        project["task"]["defaults"]["pipeline"].as_str(),
        Some("default/implementation")
    );
    assert_eq!(
        project["task"]["defaults"]["limits"]["tokens"].as_integer(),
        Some(8_000_000)
    );
    assert_eq!(
        project["task"]["defaults"]["limits"]["max_attempts"].as_integer(),
        Some(16)
    );
    assert_eq!(
        project["task"]["defaults"]["limits"]["wall_ms"].as_integer(),
        Some(43_200_000)
    );
    assert_eq!(
        project["task"]["defaults"]["limits"]["verification"]["tokens"].as_integer(),
        Some(2_000_000)
    );
    assert_eq!(
        project["task"]["defaults"]["limits"]["verification"]["attempts"].as_integer(),
        Some(9)
    );
    assert_eq!(
        project["task"]["defaults"]["limits"]["verification"]["wall_ms"].as_integer(),
        Some(18_000_000)
    );
    commit(&repo);
    let ticket = root.path().join("short.json");
    std::fs::write(
        &ticket,
        r#"{"task_id":"short","kind":"implement","goal":"Improve the CLI"}"#,
    )
    .unwrap();
    let providers = registry(
        &root.path().join("providers"),
        &[("codex-main", "codex"), ("claude-main", "claude")],
    );
    let bin = root.path().join("bin");
    crate::fake_models::install(
        "codex",
        &bin,
        &root.path().join("providers/auth-codex-main"),
    );
    crate::fake_models::install(
        "claude",
        &bin,
        &root.path().join("providers/auth-claude-main"),
    );
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let planned = crate::common::af()
        .current_dir(&repo)
        .args([
            "task",
            "plan",
            "--file",
            ticket.to_str().unwrap(),
            "--state",
        ])
        .arg(root.path().join("state"))
        .env("AF_PROVIDERS_FILE", providers)
        .env("USER", "fixture")
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        planned.status.success(),
        "{}\n{}",
        stdout(&planned),
        stderr(&planned)
    );
    assert!(
        stderr(&planned).contains("limits (tokens 8000000, max_attempts 16, wall_ms 43200000)")
    );
    assert!(
        stdout(&planned).contains("default/implementation"),
        "{}",
        stdout(&planned)
    );
    // Every default-pipeline binding is checked against the recorded catalogs: the Codex
    // fixture lists `gpt-5.6-terra`, which `default/bugs` binds.
    for worker in [
        "default/implementer",
        "default/bugs",
        "default/correctness",
        "default/advisor",
        "default/evaluator",
    ] {
        assert!(
            stderr(&planned).contains(&format!("model {worker} ")),
            "{worker}: {}",
            stderr(&planned)
        );
    }
    assert!(
        !stderr(&planned).contains("unverified against the catalog"),
        "{}",
        stderr(&planned)
    );
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
            af_pipeline_config::lock::package_digest(
                name,
                &repo.join(pin["path"].as_str().unwrap())
            )
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
    let project_path = repo.join(".af/af.toml");
    let project = std::fs::read_to_string(&project_path)
        .unwrap()
        .split("[task.defaults]")
        .next()
        .unwrap()
        .trim_end()
        .to_owned()
        + "\n";
    std::fs::write(&project_path, &project).unwrap();
    let project = project.into_bytes();
    let lock = std::fs::read(repo.join(".af/af.lock")).unwrap();
    std::fs::remove_file(repo.join(".af/task-catalog.toml")).unwrap();
    std::fs::remove_file(repo.join(".af/code-policy.toml")).unwrap();
    std::fs::remove_dir_all(repo.join(".af/task-packages")).unwrap();
    let retained = std::fs::read(repo.join(".af/README.md")).unwrap();
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
    assert_eq!(std::fs::read(repo.join(".af/README.md")).unwrap(), retained);
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
fn combined_task_preview_reports_missing_review_and_task_providers_in_json_and_text() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    let schema = crate::schemas::validator("onboard-tasks-v1.json");
    for (name, providers, profile, missing_claude, missing_codex) in [
        (
            "codex-mixed",
            &[("codex-main", "codex")][..],
            "mixed",
            true,
            false,
        ),
        (
            "codex-claude",
            &[("codex-main", "codex")][..],
            "claude",
            true,
            false,
        ),
        (
            "codex-codex",
            &[("codex-main", "codex")][..],
            "codex",
            false,
            false,
        ),
        (
            "claude-mixed",
            &[("claude-main", "claude")][..],
            "mixed",
            false,
            true,
        ),
        (
            "claude-claude",
            &[("claude-main", "claude")][..],
            "claude",
            false,
            true,
        ),
        ("none-mixed", &[][..], "mixed", true, true),
        (
            "both-mixed",
            &[("codex-main", "codex"), ("claude-main", "claude")][..],
            "mixed",
            false,
            false,
        ),
    ] {
        let provider = registry(&root.path().join(name), providers);
        let args = ["--runner", profile, "--gate", "check=true"];
        let mut json_args = args.to_vec();
        json_args.push("--json");
        let preview = task_onboard(&repo, &provider, &json_args);
        assert!(preview.status.success(), "{name}: {}", stderr(&preview));
        let report: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
        crate::schemas::valid(&schema, &report);
        let steps: Vec<_> = report["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step.as_str().unwrap().to_owned())
            .collect();
        let text = task_onboard(&repo, &provider, &args);
        assert!(text.status.success(), "{name}: {}", stderr(&text));
        assert_eq!(steps, block(&stdout(&text), "next"), "{name}");
        for (kind, missing) in [("claude", missing_claude), ("codex", missing_codex)] {
            let setup = format!("af provider setup {kind}-main --kind {kind} --login");
            assert_eq!(
                steps.iter().any(|step| step.contains(&setup)),
                missing,
                "{name}: {steps:?}"
            );
        }
        assert!(!repo.join(".af").exists());
    }
}

#[test]
fn additive_task_preview_does_not_request_claude_for_a_review_runner_option() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    std::fs::create_dir(repo.join(".af")).unwrap();
    let authority = "version = 1\n";
    std::fs::write(repo.join(".af/af.toml"), authority).unwrap();
    for (name, providers, missing_codex) in [
        ("codex", &[("codex-main", "codex")][..], false),
        ("none", &[][..], true),
    ] {
        let provider = registry(&root.path().join(name), providers);
        let args = ["--runner", "mixed", "--gate", "check=true"];
        let mut json_args = args.to_vec();
        json_args.push("--json");
        let preview = task_onboard(&repo, &provider, &json_args);
        assert!(preview.status.success(), "{}", stderr(&preview));
        let report: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
        let steps: Vec<_> = report["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step.as_str().unwrap().to_owned())
            .collect();
        let text = task_onboard(&repo, &provider, &args);
        assert!(text.status.success(), "{}", stderr(&text));
        assert_eq!(steps, block(&stdout(&text), "next"));
        assert!(
            !steps.iter().any(|step| step.contains("--kind claude")),
            "{steps:?}"
        );
        assert_eq!(
            steps.iter().any(|step| step.contains("--kind codex")),
            missing_codex,
            "{steps:?}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join(".af/af.toml")).unwrap(),
            authority
        );
        assert!(!repo.join(".af/task-catalog.toml").exists());
    }
}

#[test]
fn task_onboarding_requires_codex_even_with_only_claude_registered() {
    let root = tempfile::tempdir().unwrap();
    let repo = committed_repo(root.path());
    let provider = registry(&root.path().join("provider"), &[("claude-main", "claude")]);
    for profile in ["claude", "mixed", "codex"] {
        let preview = task_onboard(&repo, &provider, &["--runner", profile, "--json"]);
        assert!(preview.status.success(), "{}", stderr(&preview));
        let report: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
        assert_eq!(report["provider_kind"], "codex");
        assert!(
            report["next_steps"][0]
                .as_str()
                .unwrap()
                .contains("No codex Provider")
        );
    }
    let applied = task_onboard(&repo, &provider, &["--apply", "--json"]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    let catalog = std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap();
    for (worker, model) in [
        ("implementer", "gpt-6.1-sol"),
        ("bugs", "claude-opus-5-5"),
        ("correctness", "claude-opus-5-5"),
        ("evaluator", "gpt-6.1-terra"),
    ] {
        let manifest: toml::Value = toml::from_str(
            &std::fs::read_to_string(
                repo.join(format!(".af/task-packages/onboard/{worker}/worker.toml")),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest["runner"]["provider_kind"].as_str(),
            Some(if matches!(worker, "bugs" | "correctness") {
                "claude"
            } else {
                "codex"
            })
        );
        assert_eq!(manifest["runner"]["model"].as_str(), Some(model));
        assert_eq!(manifest["runner"]["effort"].as_str(), Some("high"));
        let kind = if matches!(worker, "bugs" | "correctness") {
            "claude"
        } else {
            "codex"
        };
        assert!(catalog.contains(&format!("\"onboard/{worker}\" = \"{kind}-main\"")));
    }
}

#[test]
fn task_onboarding_uses_codex_task_workers_with_a_mixed_review_profile() {
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
    assert_eq!(report["provider_kind"], "codex");
    let catalog = std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap();
    assert!(catalog.contains("\"onboard/implementer\" = \"codex-main\""));
    assert!(catalog.contains("\"onboard/bugs\" = \"codex-main\""));
    let implementer =
        std::fs::read_to_string(repo.join(".af/task-packages/onboard/implementer/worker.toml"))
            .unwrap();
    let reviewer =
        std::fs::read_to_string(repo.join(".af/task-packages/onboard/bugs/worker.toml")).unwrap();
    assert!(implementer.contains("provider_kind = \"codex\""));
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
  assignment=r.get('inputs',{}).get('assignment',[{'payload':{'findings':[]}}])[0]['payload']
  claims=os.path.exists(home+'/native-review-claims')
  reports=[{'severity':'major','file':'subject.txt','line':1,'title':'Retained native claim','body':'The selected source still contains the claim','fix':'Repair the source','confidence':0.9}] if claims and not assignment['findings'] else []
  dispositions=[{'finding_id':f['finding_id'],'position':'corroborate','reason':'Checked the complete current Subject'} for f in assignment['findings']]
  outputs={'result':[{'reports':reports,'benchmark_demands':[],'dispositions':dispositions}]}
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
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}\n{}", stdout(&run), stderr(&run));
    let result: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(result["result"]["acceptance"], "satisfied", "{result:#}");
    assert!(
        auth.join("turns").exists(),
        "the Task did not call a Worker"
    );
    // The default scaffold's standalone Review is a real current Task, independent of the
    // implementation starter. Planning spends no Attempts; resume confirms precisely that plan.
    let review_file = root.path().join("review.json");
    std::fs::copy(
        repo.join(".af/task-packages/onboard/review-code/task.json"),
        &review_file,
    )
    .unwrap();
    let state = root.path().join("review-state");
    let invoke = |args: &[&str]| {
        crate::common::af()
            .current_dir(&repo)
            .args(args)
            .arg("--state")
            .arg(&state)
            .arg("--json")
            .env("AF_PROVIDERS_FILE", &provider)
            .env("USER", "fixture")
            .env("AF_TELEMETRY__LEVEL", "spans")
            .env(
                "AF_TELEMETRY__DIR",
                root.path().join("native-model-telemetry"),
            )
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    let planned = invoke(&["review", "plan", "--file", review_file.to_str().unwrap()]);
    assert!(planned.status.success(), "{}", stderr(&planned));
    let plan: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(plan["attempts"], 0);
    assert_eq!(plan["chargeable_tokens"], "0");
    let turns_before = std::fs::read_to_string(auth.join("turns"))
        .unwrap()
        .lines()
        .count();
    let reviewed = invoke(&[
        "review",
        "resume",
        "review",
        "--confirm-plan",
        plan["plan_id"].as_str().unwrap(),
    ]);
    assert!(
        reviewed.status.success(),
        "{}\n{}",
        stdout(&reviewed),
        stderr(&reviewed)
    );
    let review: serde_json::Value = serde_json::from_slice(&reviewed.stdout).unwrap();
    assert_eq!(review["result"]["acceptance"], "satisfied", "{review:#}");
    assert_eq!(review["result"]["domain_conclusion"], "pass", "{review:#}");
    assert_eq!(review["attempts"], 4);
    assert_eq!(review["chargeable_tokens"], "36");
    let context = invoke(&["review", "decision-context", "review"]);
    assert!(context.status.success(), "{}", stderr(&context));
    let context: serde_json::Value = serde_json::from_slice(&context.stdout).unwrap();
    let whole_tree = root.path().join("whole-tree-decision.json");
    std::fs::write(&whole_tree, serde_json::to_vec(&serde_json::json!({
        "schema":"af.review-decisions/1", "review_id":context["review_id"],
        "policy_id":context["review"]["policy_id"], "subject_id":context["review"]["subject_id"],
        "snapshot_id":context["review"]["snapshot_id"], "actor":"fixture-operator", "reason":"Whole-tree policy decision",
        "actions":[{"kind":"policy_time","time":{"tick":1,"actor":"fixture-operator","reason":"Explicit whole-tree clock"}}]
    })).unwrap()).unwrap();
    let whole_tree = invoke(&[
        "review",
        "decide",
        "whole-tree-human",
        "--from",
        "review",
        "--file",
        whole_tree.to_str().unwrap(),
        "--execute",
    ]);
    assert!(
        whole_tree.status.success(),
        "{}\n{}",
        stdout(&whole_tree),
        stderr(&whole_tree)
    );
    let whole_tree: serde_json::Value = serde_json::from_slice(&whole_tree.stdout).unwrap();
    assert_eq!(whole_tree["attempts"], 0);
    assert_eq!(whole_tree["chargeable_tokens"], "0");
    assert!(whole_tree["plan"]["inputs"].get("base").is_none());

    assert_eq!(
        std::fs::read_to_string(auth.join("turns"))
            .unwrap()
            .lines()
            .count(),
        turns_before + 2
    );
    // Both advertised Base forms must survive actual standalone `review continue` planning,
    // exact-plan resume and cold replay. The captured file of the CLI form has no Base label.
    let base = {
        let output = std::process::Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    std::fs::write(repo.join("subject.txt"), "claim remains\n").unwrap();
    commit(&repo);
    std::fs::write(auth.join("native-review-claims"), "enabled").unwrap();
    for cli_base in [false, true] {
        let id = if cli_base {
            "standalone-cli-base"
        } else {
            "standalone-file-base"
        };
        let next = format!("{id}-continued");
        let mut file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&review_file).unwrap()).unwrap();
        file["task_id"] = serde_json::json!(id);
        if !cli_base {
            file["base"] = serde_json::json!({"commit":base});
        }
        std::fs::write(&review_file, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        let mut args = vec![
            "review",
            "run",
            "--file",
            review_file.to_str().unwrap(),
            "--authority",
            base.as_str(),
            "--candidate",
            "HEAD",
        ];
        if cli_base {
            args.extend(["--base", base.as_str()]);
        }
        let first = invoke(&args);
        assert_eq!(
            first.status.code(),
            Some(3),
            "{}\n{}",
            stdout(&first),
            stderr(&first)
        );
        let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
        assert_eq!(first["chargeable_tokens"], "36");
        // A later zero-token human Task must retain each original model, even in a new
        // process with no Worker bindings of its own. It advances no discovery Round.
        let context_output = invoke(&["review", "decision-context", id]);
        assert!(
            context_output.status.success(),
            "{}",
            stderr(&context_output)
        );
        let context: serde_json::Value = serde_json::from_slice(&context_output.stdout).unwrap();
        let action_id = format!("{id}-human");
        let action_file = root.path().join(format!("{action_id}.json"));
        std::fs::write(&action_file, serde_json::to_vec(&serde_json::json!({
            "schema":"af.review-decisions/1", "review_id":context["review_id"],
            "policy_id":context["review"]["policy_id"], "subject_id":context["review"]["subject_id"],
            "snapshot_id":context["review"]["snapshot_id"], "base_snapshot_id":context["subject"]["base_snapshot_id"],
            "actor":"fixture-operator", "reason":"Advance explicit native policy time",
            "actions":[{"kind":"policy_time","time":{"tick":1,"actor":"fixture-operator","reason":"Explicit policy clock"}}]
        })).unwrap()).unwrap();
        let action = invoke(&[
            "review",
            "decide",
            &action_id,
            "--from",
            id,
            "--file",
            action_file.to_str().unwrap(),
            "--execute",
        ]);
        assert_eq!(
            action.status.code(),
            Some(3),
            "{}\n{}",
            stdout(&action),
            stderr(&action)
        );
        let action: serde_json::Value = serde_json::from_slice(&action.stdout).unwrap();
        assert_eq!(action["attempts"], 0);
        assert_eq!(action["chargeable_tokens"], "0");
        let ledger = crate::telemetry_ledger::read_ledger(
            &root.path().join("native-model-telemetry"),
            &af_store::store::task::task_run_id(&action_id).unwrap(),
        );
        assert_eq!(ledger.coverage, "complete", "{:?}", ledger.coverage_reasons);
        assert!(!ledger.findings.is_empty());
        let cas = af_store::Cas::open_existing(state.join("cas")).unwrap();
        let plan: af_contracts::task::plan::ExecutionPlan = serde_json::from_value(
            cas.get_artifact(first["plan_id"].as_str().unwrap())
                .unwrap()
                .payload,
        )
        .unwrap();
        let graph: af_graph::task::CompiledTask =
            serde_json::from_value(cas.get_artifact(&plan.compiled_graph_id).unwrap().payload)
                .unwrap();
        for finding in &ledger.findings {
            let af_graph::task::CompiledOperator::Primitive {
                operator: af_contracts::task::pipeline::TaskOperatorV1::Verify { slot },
                ..
            } = &graph.nodes[&finding.node_id].operator
            else {
                panic!("Finding lost its original reviewer");
            };
            let af_contracts::task::plan::WorkerExecutionV3::Model { model, .. } =
                &plan.bindings[slot].execution
            else {
                panic!("Expected the fixture's model binding");
            };
            assert_eq!(finding.model.as_ref(), Some(model));
        }

        let planned = invoke(&["review", "continue", &next, "--from", id]);
        assert!(
            planned.status.success(),
            "{}\n{}",
            stdout(&planned),
            stderr(&planned)
        );
        let planned: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
        assert_eq!(planned["attempts"], 0);
        assert_eq!(planned["chargeable_tokens"], "0");
        let source_state =
            af_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        let source_cas = af_store::Cas::open_existing(state.join("cas")).unwrap();
        let original = source_state
            .task_projection(&source_cas, id)
            .unwrap()
            .unwrap();
        let inputs = serde_json::to_value(&original.revision.inputs).unwrap();
        assert_eq!(planned["plan"]["inputs"]["base"], inputs["base"]);
        let base_snapshot = inputs["base"]["snapshot_id"].as_str().unwrap();
        let source_snapshot = inputs["source"]["snapshot_id"].as_str().unwrap();
        let (_, original_base) =
            af_source_git::task::read_snapshot(&source_cas, base_snapshot).unwrap();
        let (_, original_source) =
            af_source_git::task::read_snapshot(&source_cas, source_snapshot).unwrap();
        assert_ne!(
            base_snapshot, source_snapshot,
            "independent candidate and Base"
        );
        assert!(
            !original_base
                .entries
                .iter()
                .any(|e| e.path == "subject.txt")
        );
        assert!(
            original_source
                .entries
                .iter()
                .any(|e| e.path == "subject.txt")
        );

        assert_eq!(
            planned["plan"]["inputs"]["source"]["snapshot_id"],
            inputs["source"]["snapshot_id"]
        );
        assert_eq!(
            planned["plan"]["inputs"]["history"],
            first["result"]["outputs"]["history"]
        );
        assert!(planned["plan"]["inputs"].get("findings").is_none());
        let finished = invoke(&[
            "review",
            "resume",
            &next,
            "--confirm-plan",
            planned["plan_id"].as_str().unwrap(),
        ]);
        assert_eq!(
            finished.status.code(),
            Some(3),
            "{}\n{}",
            stdout(&finished),
            stderr(&finished)
        );
        let finished: serde_json::Value = serde_json::from_slice(&finished.stdout).unwrap();
        assert_eq!(finished["attempts"], 4);
        assert_eq!(finished["chargeable_tokens"], "36");
        let cas = af_store::Cas::open_existing(state.join("cas")).unwrap();
        let ledger = |value: &serde_json::Value| {
            let round = value["result"]["outputs"]["review"]["artifact_ids"][0]
                .as_str()
                .unwrap();
            af_runtime::task::review::ReviewTaskDomain::recorded_ledger(&cas, round).unwrap()
        };
        let before = ledger(&first);
        let after = ledger(&finished);
        assert_eq!(before.findings().len(), 2);
        assert_eq!(after.findings().len(), 2);
        for finding in before.findings() {
            let current = after.get(&finding.key).unwrap();
            assert_eq!(current.status, af_store::Status::Open);
            assert!(current.reports.len() > finding.reports.len());
            assert!(
                current
                    .reports
                    .iter()
                    .any(|r| r.report_id == finding.reports[0].report_id)
            );
        }
        let prior = invoke(&["review", "show", id]);
        assert!(prior.status.success());
        let prior: serde_json::Value = serde_json::from_slice(&prior.stdout).unwrap();
        assert_eq!(
            prior["chargeable_tokens"], "36",
            "a new Task never rewrites prior accounting"
        );
        let refused = invoke(&[
            "review",
            "continue",
            &format!("{next}-past-bound"),
            "--from",
            &next,
        ]);
        assert_eq!(refused.status.code(), Some(1));
        assert!(
            stderr(&refused).contains("Round bound") || stdout(&refused).contains("Round bound"),
            "{}{}",
            stdout(&refused),
            stderr(&refused)
        );
        // Each case starts from the original generated file, rather than inheriting the other form.
        std::fs::copy(
            repo.join(".af/task-packages/onboard/review-code/task.json"),
            &review_file,
        )
        .unwrap();
    }
}
