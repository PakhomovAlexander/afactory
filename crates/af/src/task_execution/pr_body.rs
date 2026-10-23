//! `af task report IDS… --pr-body` (ADR-0198): the pull request description this repository's
//! template requires, ready for `gh pr create --body-file`: the template with the `af task
//! report` block between its `<!-- af-task-report:v1 -->` markers, and the lines added block
//! `scripts/pr-lines.py` prints between its `<!-- af-pr-lines:v1 -->` markers when the script
//! exists (ADR-0142, ADR-0195).

use std::path::{Path, PathBuf};
use std::time::Duration;

use review_core::task::task_report::{TASK_REPORT_BEGIN, TASK_REPORT_END};

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

/// The lines added block of the checkout at `repo`, from its own `scripts/pr-lines.py`: `None`
/// when it has no such script. A script that fails is said on stderr, and the template's block
/// stays as it is.
fn lines_block(repo: &Path) -> Option<String> {
    let script = repo.join(PR_LINES);
    if !script.is_file() {
        return None;
    }
    let mut command = std::process::Command::new("python3");
    command
        .arg("-I")
        .arg(&script)
        .arg("--repo")
        .arg(repo)
        .current_dir(repo);
    let shown = PR_LINES;
    match review_process::run_supervised(&mut command, None, PR_LINES_TIMEOUT) {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            if text.lines().any(|line| line.trim() == LINES_BEGIN) {
                return Some(text);
            }
            eprintln!(
                "af task report --pr-body: {shown} printed no `{LINES_BEGIN}` block; the lines \
                 added block is left as the template has it: run `python3 {shown}` and paste \
                 its output"
            );
        }
        Ok(output) => eprintln!(
            "af task report --pr-body: {shown} failed: {}; the lines added block is left as \
             the template has it: run `python3 {shown}` (with `--base REF` for a base other than \
             origin/main) and paste its output",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => eprintln!(
            "af task report --pr-body: running {shown}: {error}; the lines added block is left \
             as the template has it"
        ),
    }
    None
}

/// The description: `template` (or the checkout's own) with `report` spliced in, and the lines
/// added block when the checkout's script prints one. Without a template, the two sections
/// alone.
pub(crate) fn render(repo: &Path, template: Option<&Path>, report: &str) -> Result<String, String> {
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
    if let Some(lines) = lines_block(repo) {
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
        let alone = render(directory.path(), None, &report).unwrap();
        assert!(alone.starts_with("## af task report\n\n<!-- af-task-report:v1 -->\n"));
        assert!(
            alone.ends_with(&format!("{LINES_BEGIN}\n{LINES_END}\n")),
            "{alone}"
        );
    }
}
