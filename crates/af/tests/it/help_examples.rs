//! Every example in every `--help` page and every `af help <topic>` runs as written.
//!
//! A command's help is its contract, and an example a reader copies is the first thing they
//! run. This walks the clap tree as the binary prints it (subcommands from the binary's own
//! completion, pages from `--help`) and the topic registry (`af help <topic>`), takes every
//! example out of each `Examples:` block, and runs it with `bash` in an onboarded repository
//! with a private home, expecting the exit code the example documents. An example that needs
//! something this repository cannot have (a Task, a Campaign, a registered Provider, the
//! network) says so with `# needs: …`, and is held to that form instead of run.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The widest line a help page prints: clap wraps at this width, and a wrapped example no
/// longer runs as written.
const WIDTH: usize = 100;

fn af(args: &[&str]) -> Output {
    crate::common::af()
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The visible subcommands of `af <path>`, from the binary's own completion engine: the same
/// clap tree `--help` prints, without parsing a help layout.
fn subcommands(path: &[String]) -> Vec<String> {
    let listed = crate::common::af()
        .env("COMPLETE", "fish")
        .arg("--")
        .arg("af")
        .args(path)
        .arg("")
        .output()
        .unwrap();
    assert!(listed.status.success(), "completion of {path:?}");
    out(&listed)
        .lines()
        .filter_map(|line| line.split('\t').next())
        .filter(|word| !word.is_empty() && !word.starts_with('-'))
        .map(str::to_owned)
        .collect()
}

/// Every help page: `af <path> --help` for the root and every visible subcommand, then
/// `af help <topic>` for every topic. Each is named as a reader would type it.
fn pages() -> Vec<(String, String)> {
    let mut pages = Vec::new();
    let mut pending: Vec<Vec<String>> = vec![Vec::new()];
    while let Some(path) = pending.pop() {
        let mut args: Vec<&str> = path.iter().map(String::as_str).collect();
        args.push("--help");
        let help = af(&args);
        assert!(help.status.success(), "af {} --help", path.join(" "));
        let text = out(&help);
        let has_subcommands = text
            .lines()
            .find(|line| line.starts_with("Usage:"))
            .is_some_and(|usage| usage.contains("COMMAND"));
        if has_subcommands {
            for name in subcommands(&path).into_iter().rev() {
                let mut child = path.clone();
                child.push(name);
                pending.push(child);
            }
        }
        pages.push((format!("af {} --help", path.join(" ")), text));
    }
    let listed = af(&["help", "no-such-topic"]);
    let stderr = String::from_utf8_lossy(&listed.stderr).into_owned();
    let topics = stderr
        .lines()
        .find_map(|line| line.split_once("topics: ").map(|(_, topics)| topics))
        .unwrap_or_else(|| panic!("af help lists its topics: {stderr}"))
        .split(", ")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(topics.len() >= 8, "{topics:?}");
    for topic in topics {
        let page = af(&["help", &topic]);
        assert!(page.status.success(), "af help {topic}");
        pages.push((format!("af help {topic}"), out(&page)));
    }
    pages
}

/// One example as its page prints it.
#[derive(Debug, Clone)]
struct Example {
    page: String,
    /// The lines as printed, a continued example's lines joined by newlines.
    text: String,
    /// The precondition `# needs: …` names, when the example has one.
    needs: Option<String>,
    /// The exit code the example documents: `# exit N`, else 0.
    exit: i32,
}

/// Where the shell comment of `line` starts: a `#` after whitespace, outside quotes.
fn comment_start(line: &str) -> Option<usize> {
    let mut quote = None;
    let mut previous = ' ';
    for (at, c) in line.char_indices() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '#' && previous.is_whitespace() => return Some(at),
            None => {}
        }
        previous = c;
    }
    None
}

/// The examples of one page: every line of each `Examples:` block, comment lines attached to
/// the example below them, continued lines joined.
fn examples(page: &str, text: &str) -> Vec<Example> {
    let mut found = Vec::new();
    let mut in_block = false;
    let mut comments: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let finish = |lines: String, comments: &mut Vec<String>, found: &mut Vec<Example>| {
        let last = lines.lines().last().unwrap_or_default();
        let mut notes = std::mem::take(comments);
        if let Some(at) = comment_start(last) {
            notes.push(last[at..].to_owned());
        }
        let mut needs = None;
        let mut exit = 0;
        for note in notes {
            let note = note.trim_start_matches('#').trim();
            if let Some(what) = note.strip_prefix("needs:") {
                needs = Some(what.trim().to_owned());
            } else if let Some(code) = note
                .strip_prefix("exit ")
                .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|digits| digits.parse().ok())
            {
                exit = code;
            }
        }
        found.push(Example {
            page: page.to_owned(),
            text: lines,
            needs,
            exit,
        });
    };
    for line in text.lines() {
        if line.trim_end() == "Examples:" {
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        if line.trim().is_empty() || !line.starts_with(' ') {
            assert!(
                current.is_none(),
                "{page}: a continued example ends without its last line"
            );
            in_block = false;
            comments.clear();
            continue;
        }
        let body = line.trim();
        let continues = body.ends_with('\\');
        match current.as_mut() {
            Some(lines) => {
                lines.push('\n');
                lines.push_str(line);
            }
            None if body.starts_with('#') => {
                comments.push(body.to_owned());
                continue;
            }
            None => current = Some(body.to_owned()),
        }
        if !continues {
            let lines = current.take().unwrap();
            finish(lines, &mut comments, &mut found);
        }
    }
    found
}

/// The repository every example runs in: two commits on `main`, the second the `.af/` this
/// binary's onboarding writes (Codex reviewers, a trivial Gate), and an uncommitted change.
fn onboarded(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let done = Command::new("git")
            .current_dir(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(done.status.success(), "git {args:?}: {done:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# Example\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);
    let onboard = crate::common::af()
        .args([
            "onboard",
            "--runner",
            "codex",
            "--gate",
            "check=true",
            "--apply",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(onboard.status.success(), "{onboard:?}");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "af review policy"]);
    std::fs::write(repo.join("notes.md"), "# Notes\n\nan uncommitted change\n").unwrap();
    repo
}

/// The PATH examples run with: this binary, then the system's own tools.
fn examples_path(bin: &Path) -> String {
    format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display())
}

fn has_jq(bin: &Path) -> bool {
    Command::new("bash")
        .args(["-c", "jq --version"])
        .env("PATH", examples_path(bin))
        // A login shell, as every reader's terminal has one.
        .env("SHELL", "/bin/bash")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// `text` as it runs here: without jq, the af command before a `| jq` filter, so the example's
/// own exit code is still what is checked.
fn runnable(text: &str, jq: bool) -> String {
    if jq {
        return text.to_owned();
    }
    match text.find("| jq") {
        Some(at) => text[..at].trim_end().to_owned(),
        None => text.to_owned(),
    }
}

/// Run one example with `bash` in `repo`, with `af` on PATH as this test's binary and the
/// private home every `af` a test starts uses; killed when it runs past `deadline`.
fn run(example: &Example, repo: &Path, bin: &Path, jq: bool) -> (Option<i32>, String) {
    let script = format!("set -o pipefail\n{}", runnable(&example.text, jq));
    let mut command = Command::new("bash");
    // Nothing of the developer's own environment: no Provider CLI on PATH, no Provider auth
    // directory, no af override; only this binary, the system's tools and a private home.
    command.env_clear();
    for name in ["TMPDIR", "USER", "LOGNAME", "LANG"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .arg("-c")
        .arg(&script)
        .current_dir(repo)
        .env("PATH", examples_path(bin))
        // A login shell, as every reader's terminal has one.
        .env("SHELL", "/bin/bash")
        .env("AF_SELF_OFFLINE", "1")
        .env("NO_COLOR", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            crate::common::PROBE_TIMEOUT_SETTING,
            crate::common::LOAD_SAFE_PROBE_TIMEOUT_MS,
        )
        .stdin(Stdio::null());
    for (name, value) in crate::common::private_home_env() {
        command.env(name, value);
    }
    // Files, not pipes: an example that prints more than a pipe holds never blocks on a reader
    // that waits for it to exit.
    let streams = tempfile::tempdir().unwrap();
    let (stdout, stderr) = (streams.path().join("out"), streams.path().join("err"));
    command
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap());
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(300);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let read = |path: &Path| String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
    (
        status.and_then(|status| status.code()),
        format!("stdout:\n{}\nstderr:\n{}", read(&stdout), read(&stderr)),
    )
}

/// Every example is one that runs as written or names what it needs: no line wider than the
/// page (clap would wrap it), every one an `af` command, every `needs` a precondition.
#[test]
fn every_help_example_is_a_command_or_names_what_it_needs() {
    let mut count = 0;
    let mut wrong = Vec::new();
    for (page, text) in pages() {
        for example in examples(&page, &text) {
            count += 1;
            if !example.text.contains("af ") {
                wrong.push(format!("{page}: runs no af command: {}", example.text));
            }
            for (index, line) in example.text.lines().enumerate() {
                // The first line lost its two-column indent when it was read.
                let width = line.chars().count() + if index == 0 { 2 } else { 0 };
                if width > WIDTH {
                    wrong.push(format!("{page}: wider than the help page: {line}"));
                }
                if index > 0 && !line.starts_with("    ") {
                    wrong.push(format!("{page}: a continued line not indented: {line}"));
                }
            }
            if example.needs.as_ref().is_some_and(|needs| needs.len() < 6) {
                wrong.push(format!(
                    "{page}: `# needs:` names nothing: {}",
                    example.text
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(count > 150, "only {count} examples found");
}

#[test]
fn every_help_example_runs_as_written_in_an_onboarded_repository() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let repo = onboarded(&root);
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(crate::common::af_binary(), bin.join("af")).unwrap();
    let jq = has_jq(&bin);
    let mut seen = std::collections::BTreeSet::new();
    let mut ran = 0;
    let mut failures = Vec::new();
    for (page, text) in pages() {
        for example in examples(&page, &text) {
            if example.needs.is_some() || !seen.insert(example.text.clone()) {
                continue;
            }
            ran += 1;
            let (code, output) = run(&example, &repo, &bin, jq);
            if code != Some(example.exit) {
                failures.push(format!(
                    "{}: `{}` exited {code:?}, documented {}\n{output}",
                    example.page,
                    example.text.trim(),
                    example.exit
                ));
            }
        }
    }
    assert!(ran > 60, "only {ran} examples ran");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn the_example_grammar_reads_comments_continuations_and_exit_codes() {
    let text = "Usage: af x\n\nExamples:\n  af a\n  # needs: a Task\n  af b TASK_ID \\\n    \
                --json\n  af help nope   # exit 2\n  af c 'quoted # not a comment'   # what it \
                does\n\nAfter\n  af not-an-example\n";
    let found = examples("page", text);
    let summary: Vec<(String, Option<String>, i32)> = found
        .into_iter()
        .map(|example| (example.text, example.needs, example.exit))
        .collect();
    assert_eq!(
        summary,
        [
            ("af a".to_owned(), None, 0),
            (
                "af b TASK_ID \\\n    --json".to_owned(),
                Some("a Task".to_owned()),
                0
            ),
            ("af help nope   # exit 2".to_owned(), None, 2),
            (
                "af c 'quoted # not a comment'   # what it does".to_owned(),
                None,
                0
            ),
        ]
    );
}
