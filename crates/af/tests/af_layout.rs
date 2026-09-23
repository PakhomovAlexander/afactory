//! The declared `.af/` layout: one table in the code, rendered into `af help config` and
//! and an advisory warning when a Task file sits in the checkout.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use review_config::layout;

#[path = "support/task_cli.rs"]
mod task_cli;

const AF: &str = env!("CARGO_BIN_EXE_af");

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Whether the scan reads this file at all: a separate `tests.rs` beside a module is that
/// module's test file, never the code that resolves authority paths.
fn is_kernel_source(path: &Path) -> bool {
    let is_rust = path.extension().is_some_and(|suffix| suffix == "rs");
    is_rust && path.file_name().is_some_and(|name| name != "tests.rs")
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if is_kernel_source(&path) {
            out.push(path);
        }
    }
}

/// One module's production source: everything above its inline `#[cfg(test)] mod … {` block.
fn production(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let opens_tests = |index: &usize| {
        let gated = lines[*index].trim() == "#[cfg(test)]";
        gated && lines.get(index + 1).is_some_and(is_inline_module)
    };
    let end = (0..lines.len()).find(opens_tests).unwrap_or(lines.len());
    lines[..end].join("\n")
}

fn is_inline_module(line: &&str) -> bool {
    let line = line.trim();
    line.starts_with("mod ") && line.ends_with('{')
}

/// Every first path segment under `.af/` that a source text names.
fn authority_segments(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find(".af/") {
        rest = &rest[at + ".af/".len()..];
        let named = |byte: &char| byte.is_ascii_alphanumeric() || "-_.".contains(*byte);
        let segment: String = rest.chars().take_while(named).collect();
        if !segment.is_empty() {
            found.insert(segment);
        }
    }
    found
}

#[test]
fn every_authority_path_the_kernel_resolves_is_declared_in_the_layout_table() {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(workspace().join("crates")).unwrap() {
        let source = entry.unwrap().path().join("src");
        if source.is_dir() {
            rust_sources(&source, &mut files);
        }
    }
    assert!(files.len() > 50, "the source scan found almost nothing");
    let mut seen = BTreeSet::new();
    for path in files {
        let text = production(&std::fs::read_to_string(&path).unwrap());
        for segment in authority_segments(&text) {
            let candidate = format!(".af/{segment}");
            // Membership in the table, not repository classification: a synthetic entry is in
            // the table and classifies as undeclared on a repository path by design.
            let declared = layout::LAYOUT.iter().any(|entry| entry.segment == segment);
            let owner = path.display();
            assert!(declared, "{owner} resolves `{candidate}`, undeclared");
            seen.insert(segment);
        }
    }
    // A scan that found nothing would pass vacuously. These are the paths onboarding, the
    // configuration layers, the lock and the catalog cannot run without.
    let required = [
        "af.toml",
        "af.local.toml",
        "af.lock",
        "pipelines",
        "workers",
        "task-catalog.toml",
        "code-policy.toml",
    ];
    for expected in required {
        assert!(seen.contains(expected), "the scan never saw `{expected}`");
    }
}

#[test]
fn af_help_config_carries_the_codes_table() {
    let help = Command::new(AF)
        .args(["help", "config"])
        .env("AF_SELF_OFFLINE", "1")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(help.status.success(), "{}", err(&help));
    let rendered = String::from_utf8_lossy(&help.stdout).into_owned();
    let table = layout::render_table();
    assert!(rendered.contains(&table), "af help config:\n{rendered}");
    assert!(rendered.contains(layout::ELSEWHERE), "{rendered}");

    for entry in layout::LAYOUT {
        let name = entry.name();
        assert!(rendered.contains(&name), "af help lost `{name}`");
    }
}

fn plan(repo: &Path, state: &Path, file: &str) -> Output {
    Command::new(AF)
        .current_dir(repo)
        .args(["task", "plan", "--file", file, "--state"])
        .arg(state)
        .arg("--json")
        .output()
        .unwrap()
}

#[test]
fn a_task_file_the_repository_would_carry_is_warned_about_and_nothing_else_is() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(root.path(), "embedded-review");
    let ticket = std::fs::read(repo.join("ticket.json")).unwrap();

    std::fs::create_dir_all(repo.join(".af/tasks")).unwrap();
    let request = repo.join(".af/tasks/x.json");
    std::fs::write(&request, &ticket).unwrap();
    let inside = plan(&repo, &state.join("inside"), ".af/tasks/x.json");
    assert_eq!(inside.status.code(), Some(0), "{}", err(&inside));
    let warning = err(&inside);
    let named = "warning: Task file `.af/tasks/x.json`";
    assert!(warning.contains(named), "{warning}");
    assert!(warning.contains("$XDG_STATE_HOME/af/tasks/"), "{warning}");
    assert!(warning.contains("captured it"), "{warning}");
    // Advisory only: stdout is the ordinary plan document, and the exit code is unchanged.
    let document: serde_json::Value = serde_json::from_slice(&inside.stdout).unwrap();
    assert_eq!(document["attempts"], 0);

    let excludes = repo.join(".gitignore");
    std::fs::write(excludes, "local-tasks/\n").unwrap();
    std::fs::create_dir_all(repo.join("local-tasks")).unwrap();
    let ignored = repo.join("local-tasks/x.json");
    std::fs::write(&ignored, &ticket).unwrap();
    let held = plan(&repo, &state.join("held"), "local-tasks/x.json");
    assert_eq!(held.status.code(), Some(0), "{}", err(&held));
    let quiet = err(&held);
    assert!(!quiet.contains("warning: Task file"), "{quiet}");

    let away = root.path().join("outside.json");
    std::fs::write(&away, &ticket).unwrap();
    let out = plan(&repo, &state.join("away"), away.to_str().unwrap());
    assert_eq!(out.status.code(), Some(0), "{}", err(&out));
    let silent = err(&out);
    assert!(!silent.contains("warning: Task file"), "{silent}");
}

#[test]
fn a_task_file_under_the_authority_directory_is_undeclared_wherever_it_sits() {
    let requests = [".af/tasks", ".af/tasks/x.json", ".af/tasks/warm/p1.json"];
    for path in requests {
        assert!(layout::is_undeclared(path), "{path}");
    }
    // Authority itself stays declared, so the rule separates requests from declarations
    // rather than condemning everything the directory carries.
    assert!(layout::is_declared(".af/pipelines/review.toml"));
    assert!(layout::is_declared(".af/task-packages/kernel/bugs"));
}
