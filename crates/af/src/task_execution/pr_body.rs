//! `af task report IDS… --pr-body` (ADR-0202): the pull request description this repository's
//! template requires, ready for `gh pr create --body-file`: the template with the `af task
//! report` block between its `<!-- af-task-report:v1 -->` markers, and the lines added block
//! `scripts/pr-lines.py` prints between its `<!-- af-pr-lines:v1 -->` markers when the script
//! exists (ADR-0142, ADR-0195). A checkout that has the script and cannot run it to a block gets
//! no body at all: a body without its block is one the pull request check refuses.

use std::path::{Path, PathBuf};
use std::time::Duration;

use review_core::task::task_report::{TASK_REPORT_BEGIN, TASK_REPORT_END};

use super::preview;

pub(crate) const LINES_BEGIN: &str = "<!-- af-pr-lines:v1 -->";
pub(crate) const LINES_END: &str = "<!-- /af-pr-lines -->";
/// Where a repository keeps its pull request template, and its lines added script.
pub(crate) const TEMPLATE: &str = ".github/PULL_REQUEST_TEMPLATE.md";
pub(crate) const PR_LINES: &str = "scripts/pr-lines.py";

/// How long `scripts/pr-lines.py` may take.
const PR_LINES_TIMEOUT: Duration = Duration::from_secs(60);

/// Replace the first `begin` … `end` block of `body` (both marker lines included) with
/// `block`; `None` when `body` has no such block.
fn splice(body: &str, begin: &str, end: &str, block: &str) -> Option<String> {
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.iter().position(|line| line.trim() == begin)?;
    let stop = start + lines[start..].iter().position(|line| line.trim() == end)?;
    let mut out: Vec<String> = lines[..start].iter().map(|l| (*l).to_owned()).collect();
    out.extend(block.trim_end_matches('\n').lines().map(str::to_owned));
    out.extend(lines[stop + 1..].iter().map(|l| (*l).to_owned()));
    let mut text = out.join("\n");
    text.push('\n');
    Some(text)
}

/// The widest line of a refusal, `af task report: ` included.
const WIDTH: usize = 100;

/// The lines added block of the checkout at `repo`, from its own `scripts/pr-lines.py`
/// counting from `base` (its default, origin/main, when `None`): `None` when it has no such
/// script, whose template block then stays as it is. A script that fails, or prints no block,
/// is an error that says how to give it a base it can count from.
fn lines_block(repo: &Path, base: Option<&str>, rerun: &str) -> Result<Option<String>, String> {
    let script = repo.join(PR_LINES);
    if !script.is_file() {
        return Ok(None);
    }
    let mut command = std::process::Command::new("python3");
    command
        .arg("-I")
        .arg(&script)
        .arg("--repo")
        .arg(repo)
        .current_dir(repo);
    if let Some(base) = base {
        command.arg("--base").arg(base);
    }
    let what = match base {
        Some(base) => format!("--base {}", preview::text(base)),
        None => "origin/main, its default base,".to_owned(),
    };
    let cause = match review_process::run_supervised(&mut command, None, PR_LINES_TIMEOUT) {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            if text.lines().any(|line| line.trim() == LINES_BEGIN) {
                return Ok(Some(text));
            }
            format!("it printed no `{LINES_BEGIN}` block")
        }
        Ok(output) => {
            let said = String::from_utf8_lossy(&output.stderr);
            let last = said
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("no message");
            format!("it failed: {}", preview::short(last.trim(), 160))
        }
        Err(error) => format!("it could not run: {error}"),
    };
    let message = format!(
        "{PR_LINES} could not count the lines this branch adds from {what} so no pull request \
         body was printed: {cause}. Fetch the base (`git fetch origin main`), or name one that \
         exists with --base REF:"
    );
    let mut lines =
        crate::tui::panes::tasks::bounded(&message, WIDTH - "af task report: ".len(), "");
    lines.extend(super::runs::command_lines(
        "  ",
        rerun,
        WIDTH - "af task report: ".len(),
        "    ",
    ));
    Err(lines.join("\n"))
}

/// The command as typed, `af` and its arguments, with `--base REF` in place of any `--base` it
/// had.
fn rerun_with_base(arguments: impl Iterator<Item = String>) -> String {
    let mut words = vec!["af".to_owned()];
    let mut arguments = arguments.peekable();
    while let Some(word) = arguments.next() {
        if word == "--base" {
            arguments.next();
            continue;
        }
        if word.starts_with("--base=") {
            continue;
        }
        words.push(word);
    }
    words.extend(["--base".to_owned(), "REF".to_owned()]);
    shell_words::join(words)
}

/// The description: `template` (or the checkout's own) with `report` spliced in, and the lines
/// added block the checkout's script prints from `base`, when it has the script. Without a
/// template, the two sections alone.
pub(crate) fn render(
    repo: &Path,
    template: Option<&Path>,
    base: Option<&str>,
    report: &str,
) -> Result<String, String> {
    render_as(repo, template, base, report, std::env::args().skip(1))
}

/// [`render`] for the command line `arguments` (after `af`), which a refusal names again with
/// `--base REF`.
fn render_as(
    repo: &Path,
    template: Option<&Path>,
    base: Option<&str>,
    report: &str,
    arguments: impl Iterator<Item = String>,
) -> Result<String, String> {
    let rerun = rerun_with_base(arguments);
    let template_path: Option<PathBuf> = match template {
        Some(path) => Some(path.to_path_buf()),
        None => Some(repo.join(TEMPLATE)).filter(|path| path.is_file()),
    };
    let template_text = match &template_path {
        Some(path) => std::fs::read_to_string(path).map_err(|error| {
            format!(
                "--template {}: {error}; name a readable Markdown file, or leave --template out \
                 to use {TEMPLATE}",
                path.display()
            )
        })?,
        None => format!(
            "## af task report\n\n{TASK_REPORT_BEGIN}\n{TASK_REPORT_END}\n\n## Lines added\n\n\
             {LINES_BEGIN}\n{LINES_END}\n"
        ),
    };
    let mut body = match splice(&template_text, TASK_REPORT_BEGIN, TASK_REPORT_END, report) {
        Some(body) => body,
        None => {
            // A template without the block gets it as its own section, at the end.
            let mut body = template_text.trim_end().to_owned();
            body.push_str("\n\n## af task report\n\n");
            body.push_str(report.trim_end());
            body.push('\n');
            body
        }
    };
    if let Some(lines) = lines_block(repo, base, &rerun)? {
        body = match splice(&body, LINES_BEGIN, LINES_END, &lines) {
            Some(spliced) => spliced,
            None => format!("{}\n\n## Lines added\n\n{}", body.trim_end(), lines),
        };
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_replaces_the_template_block_and_nothing_else() {
        let report =
            format!("{TASK_REPORT_BEGIN}\n### af task report\n\n| x |\n{TASK_REPORT_END}\n");
        let template = format!(
            "## What\n\n<!-- say what -->\n\n## af task report\n\n{TASK_REPORT_BEGIN}\n\
             {TASK_REPORT_END}\n\n## Checklist\n"
        );
        let body = splice(&template, TASK_REPORT_BEGIN, TASK_REPORT_END, &report).unwrap();
        assert_eq!(
            body,
            format!(
                "## What\n\n<!-- say what -->\n\n## af task report\n\n{TASK_REPORT_BEGIN}\n\
                 ### af task report\n\n| x |\n{TASK_REPORT_END}\n\n## Checklist\n"
            )
        );
        assert_eq!(
            splice("no block\n", TASK_REPORT_BEGIN, TASK_REPORT_END, &report),
            None
        );
        let directory = tempfile::tempdir().unwrap();
        // No template and no script: the two sections alone, the lines block left empty.
        let alone = render(directory.path(), None, None, &report).unwrap();
        assert!(alone.starts_with("## af task report\n\n<!-- af-task-report:v1 -->\n"));
        assert!(
            alone.ends_with(&format!("{LINES_BEGIN}\n{LINES_END}\n")),
            "{alone}"
        );
    }

    /// A checkout whose script fails, or prints no block, gets no body: the refusal says why,
    /// names --base REF with the command to run again, and fits 100 columns.
    #[test]
    fn a_lines_script_that_cannot_print_its_block_is_refused() {
        let report = format!("{TASK_REPORT_BEGIN}\n{TASK_REPORT_END}\n");
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("scripts")).unwrap();
        let script = directory.path().join(PR_LINES);
        let long = "x".repeat(280);
        std::fs::write(
            &script,
            format!(
                "import sys\nsys.stderr.write(\"pr-lines: fatal: bad revision \
                 'origin/main...HEAD' {long}\\n\")\nsys.exit(2)\n"
            ),
        )
        .unwrap();
        let refused = |base: Option<&str>| {
            let arguments = ["task", "report", "a", "b", "--pr-body", "--state", "/s"];
            render_as(
                directory.path(),
                None,
                base,
                &report,
                arguments.into_iter().map(str::to_owned),
            )
        };
        let error = refused(None).unwrap_err();
        assert!(
            error.ends_with("af task report a b --pr-body --state /s --base REF"),
            "{error}"
        );
        let flat = |error: &str| error.replace('\n', " ");
        assert!(
            flat(&error).contains("bad revision 'origin/main...HEAD'"),
            "{error}"
        );
        assert!(flat(&error).contains("--base REF"), "{error}");
        assert!(
            flat(&error).contains("from origin/main, its default base,"),
            "{error}"
        );
        for line in error.lines() {
            assert!(
                line.chars().count() + "af task report: ".len() <= WIDTH,
                "{line}"
            );
        }
        assert_eq!(
            rerun_with_base(
                ["task", "report", "a", "b", "--pr-body", "--base", "v1"]
                    .into_iter()
                    .map(str::to_owned)
            ),
            "af task report a b --pr-body --base REF"
        );
        let error = refused(Some("v1")).unwrap_err();
        assert!(flat(&error).contains("from --base v1 so no"), "{error}");
        std::fs::write(&script, "print('no block here')\n").unwrap();
        let error = refused(None).unwrap_err();
        assert!(
            flat(&error).contains("printed no `<!-- af-pr-lines:v1 -->` block"),
            "{error}"
        );
    }
}
