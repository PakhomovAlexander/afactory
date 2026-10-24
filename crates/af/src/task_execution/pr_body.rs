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
            match incomplete(&text) {
                None => return Ok(Some(text)),
                Some(Incomplete::NoBlock) => format!("it printed no `{LINES_BEGIN}` block"),
                Some(Incomplete::Lacks(lacks)) => return Err(incomplete_refusal(&lacks, base)),
            }
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

/// What a lines added block the script printed lacks.
enum Incomplete {
    /// No begin marker at all: the script printed no block.
    NoBlock,
    /// A block begun and not finished, or not one: what it lacks, in af's words.
    Lacks(String),
}

/// What `text` lacks of the block `scripts/check-pr-report.py` accepts: exactly one begin
/// marker, the end marker after it, and between them a body `scripts/pr-lines.py`'s own parser
/// reads back ([`body_problem`]); `None` when it has all of them.
fn incomplete(text: &str) -> Option<Incomplete> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let at = |marker: &str| -> Vec<usize> {
        (0..lines.len())
            .filter(|index| lines[*index] == marker)
            .collect()
    };
    let (begins, ends) = (at(LINES_BEGIN), at(LINES_END));
    let Some(&begin) = begins.first() else {
        return Some(Incomplete::NoBlock);
    };
    if begins.len() > 1 || ends.len() > 1 {
        return Some(Incomplete::Lacks(format!(
            "it printed {} lines added blocks, and a pull request carries exactly one",
            begins.len().max(ends.len())
        )));
    }
    let Some(&end) = ends.first().filter(|end| **end > begin) else {
        return Some(Incomplete::Lacks(format!(
            "its block has the begin marker `{LINES_BEGIN}` and no end marker `{LINES_END}` \
             after it"
        )));
    };
    body_problem(&lines[begin + 1..end]).map(Incomplete::Lacks)
}

/// The table header and its separator row, as `scripts/pr-lines.py` writes them.
const HEADER: [&str; 3] = ["Directory", "Added", "Share"];
const SEPARATOR_ROW: &str = "|:--|--:|:--|";
/// The eighth blocks a share's bar is drawn with.
const BLOCKS: &str = "▏▎▍▌▋▊▉█";

/// What is wrong with the block's body `lines`, by the rules of `parse` in
/// `scripts/pr-lines.py`, which `scripts/check-pr-report.py` reads the block with: the title
/// `**Lines added: <figure>**`, then the table header, its separator and at least one row per
/// directory, each directory once; or `**Lines added: none**` alone. Blank lines do not count.
/// `None` when the parser reads it back. A figure or share in non-ASCII digits, which the
/// script never prints, is refused here even where Python's `\d` would read it.
fn body_problem(lines: &[&str]) -> Option<String> {
    let lines: Vec<&str> = lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect();
    let shown = |line: &str| preview::short(&preview::text(line), 60);
    let Some(first) = lines.first() else {
        return Some(
            "its block has no title line `**Lines added: <figure>**` between its markers".into(),
        );
    };
    let Some(total) = title_figure(first) else {
        return Some(format!(
            "its block's first line is not the title `**Lines added: <figure>**`: `{}`",
            shown(first)
        ));
    };
    if total == "none" {
        return lines.get(1).map(|more| {
            format!(
                "its block's `**Lines added: none**` is the whole block, but more follows: `{}`",
                shown(more)
            )
        });
    }
    let header = format!("| {} |", HEADER.join(" | "));
    if lines.len() < 4 {
        return Some(format!(
            "its block's title is not followed by the table: the header `{header}`, its \
             separator `{SEPARATOR_ROW}` and one row per directory"
        ));
    }
    if row_cells(lines[1]).is_none_or(|cells| cells != HEADER) {
        return Some(format!(
            "its block's line after the title is not the table header `{header}`: `{}`",
            shown(lines[1])
        ));
    }
    let separator = |cell: &String| {
        let inner = cell.strip_prefix(':').unwrap_or(cell);
        let inner = inner.strip_suffix(':').unwrap_or(inner);
        !inner.is_empty() && inner.bytes().all(|b| b == b'-')
    };
    if !row_cells(lines[2]).is_some_and(|cells| cells.len() == 3 && cells.iter().all(separator)) {
        return Some(format!(
            "its block's header is not followed by its separator `{SEPARATOR_ROW}`: `{}`",
            shown(lines[2])
        ));
    }
    let mut names: Vec<String> = Vec::new();
    for line in &lines[3..] {
        let Some([name, figure, share]) =
            row_cells(line).and_then(|c| <[String; 3]>::try_from(c).ok())
        else {
            return Some(format!(
                "its block has a line that is not a row of three cells `| directory | figure | \
                 bar share |`: `{}`",
                shown(line)
            ));
        };
        if name.is_empty() || !is_figure(&figure) || !is_share(&share) {
            return Some(format!(
                "its block has a row that is not `| directory | figure | bar share |` (a figure \
                 as `412`, `1.5k` or `23k`; the share as `40%` or `<1%` after its bar): `{}`",
                shown(line)
            ));
        }
        if names.contains(&name) {
            return Some(format!(
                "its block has two rows for `{}`",
                preview::short(&preview::text(&name), 60)
            ));
        }
        names.push(name);
    }
    None
}

/// The cells of a table row written with both outer `|`, a `\|` read as a `|`, each trimmed;
/// `None` for any other line. `row_cells` of `scripts/pr-lines.py`.
fn row_cells(line: &str) -> Option<Vec<String>> {
    let text = line.trim();
    if text.chars().count() < 2
        || !text.starts_with('|')
        || !text.ends_with('|')
        || text.ends_with("\\|")
    {
        return None;
    }
    let (mut cells, mut cell, mut escaped) = (Vec::new(), String::new(), false);
    for c in text[1..text.len() - 1].chars() {
        if escaped {
            if c != '|' {
                cell.push('\\');
            }
            cell.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '|' {
            cells.push(cell.trim().to_owned());
            cell.clear();
        } else {
            cell.push(c);
        }
    }
    let mut last = cell.trim().to_owned();
    if escaped {
        last.push('\\');
    }
    cells.push(last);
    Some(cells)
}

/// The figure of `**Lines added: FIGURE**`, as `scripts/pr-lines.py` writes it: `none`, or a
/// figure [`is_figure`] accepts.
fn title_figure(line: &str) -> Option<&str> {
    line.strip_prefix("**Lines added: ")
        .and_then(|rest| rest.strip_suffix("**"))
        .filter(|figure| *figure == "none" || is_figure(figure))
}

/// A count as `scripts/pr-lines.py` writes it: `0`, up to three digits, or up to three digits
/// with an optional decimal and `k` (`18.9k`).
fn is_figure(figure: &str) -> bool {
    let digits = |text: &str| {
        (1..=3).contains(&text.len())
            && text.bytes().all(|b| b.is_ascii_digit())
            && !text.starts_with('0')
    };
    if figure == "0" || digits(figure) {
        return true;
    }
    let Some(thousands) = figure.strip_suffix('k') else {
        return false;
    };
    match thousands.split_once('.') {
        Some((whole, tenth)) => {
            digits(whole) && tenth.len() == 1 && tenth.bytes().all(|b| b.is_ascii_digit())
        }
        None => digits(thousands),
    }
}

/// A share as `scripts/pr-lines.py` writes it: a bar of at least one eighth block, one space,
/// and `<1%` or one to three digits and `%`.
fn is_share(share: &str) -> bool {
    let Some((bar, percent)) = share.split_once(' ') else {
        return false;
    };
    let number = percent.strip_suffix('%').unwrap_or("");
    !bar.is_empty()
        && bar.chars().all(|c| BLOCKS.contains(c))
        && (percent == "<1%"
            || ((1..=3).contains(&number.len()) && number.bytes().all(|b| b.is_ascii_digit())))
}

/// The refusal of a block the script began and did not finish: what it lacks, and the command
/// that shows what the script prints.
fn incomplete_refusal(lacks: &str, base: Option<&str>) -> String {
    let message = format!(
        "{PR_LINES} printed an incomplete lines added block, so no pull request body was printed: \
         {lacks}. The pull request check refuses a body without the whole block; see what the \
         script prints, and fix it:"
    );
    let mut script = format!("python3 -I {PR_LINES} --repo .");
    if let Some(base) = base {
        script.push_str(&format!(" --base {}", shell_words::quote(base)));
    }
    let mut lines =
        crate::tui::panes::tasks::bounded(&message, WIDTH - "af task report: ".len(), "");
    lines.extend(super::runs::command_lines(
        "  ",
        &script,
        WIDTH - "af task report: ".len(),
        "    ",
    ));
    lines.join("\n")
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

        // GA final fix item 3: a block begun and not finished is refused, naming what it lacks.
        for (printed, lacks) in [
            (
                "print('<!-- af-pr-lines:v1 -->')\n",
                "its block has the begin marker `<!-- af-pr-lines:v1 -->` and no end marker \
                 `<!-- /af-pr-lines -->` after it",
            ),
            (
                "print('<!-- /af-pr-lines -->')\nprint('<!-- af-pr-lines:v1 -->')\n",
                "no end marker `<!-- /af-pr-lines -->` after it",
            ),
            (
                "print('<!-- af-pr-lines:v1 -->')\nprint('<!-- /af-pr-lines -->')\n",
                "its block has no title line `**Lines added: <figure>**` between its markers",
            ),
            (
                "print('<!-- af-pr-lines:v1 -->')\nprint('| dir | 3 |')\n\
                 print('<!-- /af-pr-lines -->')\n",
                "its block's first line is not the title `**Lines added: <figure>**`: `| dir | 3 |`",
            ),
            (
                "print('<!-- af-pr-lines:v1 -->')\nprint('**Lines added: 1**')\n\
                 print('<!-- /af-pr-lines -->')\nprint('<!-- af-pr-lines:v1 -->')\n",
                "it printed 2 lines added blocks, and a pull request carries exactly one",
            ),
        ] {
            std::fs::write(&script, printed).unwrap();
            let error = refused(Some("v1")).unwrap_err();
            assert!(flat(&error).contains(lacks), "{printed}: {error}");
            assert!(
                flat(&error).contains("printed an incomplete lines added block"),
                "{error}"
            );
            assert!(
                error.ends_with("python3 -I scripts/pr-lines.py --repo . --base v1"),
                "{error}"
            );
            for line in error.lines() {
                assert!(
                    line.chars().count() + "af task report: ".len() <= WIDTH,
                    "{line}"
                );
            }
        }
        // GA second fix item 1: a title alone is not a block the parser reads back; the title
        // and the table, or `**Lines added: none**` alone, are.
        for (printed, verdict) in [
            (
                "print('<!-- af-pr-lines:v1 -->')\nprint('**Lines added: 1**')\n\
                 print('<!-- /af-pr-lines -->')\n",
                Err(
                    "its block's title is not followed by the table: the header \
                     `| Directory | Added | Share |`, its separator `|:--|--:|:--|` and one row \
                     per directory",
                ),
            ),
            (
                "print('<!-- af-pr-lines:v1 -->')\nprint('**Lines added: 18.9k**')\nprint()\n\
                 print('| Directory | Added | Share |')\nprint('|:--|--:|:--|')\n\
                 print('| crates | 18.9k | \u{2588}\u{2588} 100% |')\n\
                 print('<!-- /af-pr-lines -->')\n",
                Ok(()),
            ),
            (
                "print('<!-- af-pr-lines:v1 -->')\nprint('**Lines added: none**')\n\
                 print('<!-- /af-pr-lines -->')\n",
                Ok(()),
            ),
        ] {
            std::fs::write(&script, printed).unwrap();
            match (refused(None), verdict) {
                (Ok(body), Ok(())) => assert!(body.contains("**Lines added: "), "{body}"),
                (Err(error), Err(lacks)) => {
                    assert!(flat(&error).contains(lacks), "{printed}: {error}");
                }
                (got, want) => panic!("{printed}: {got:?}, expected {want:?}"),
            }
        }
    }

    #[test]
    fn the_title_figure_is_the_one_pr_lines_writes() {
        for title in ["none", "0", "7", "999", "1k", "18.9k", "999.9k"] {
            let line = format!("**Lines added: {title}**");
            assert_eq!(title_figure(&line), Some(title), "{title}");
        }
        for title in ["", "07", "1000", "1.2", "18.95k", "0k", "1,2k", "many"] {
            let line = format!("**Lines added: {title}**");
            assert_eq!(title_figure(&line), None, "{title}");
        }
        assert_eq!(title_figure("Lines added: 7"), None);
    }

    /// The bodies the parity test reads with both parsers: what `scripts/pr-lines.py` prints,
    /// and every way a body can depart from it.
    fn bodies() -> Vec<Vec<&'static str>> {
        let header = "| Directory | Added | Share |";
        let separator = "|:--|--:|:--|";
        let row = "| crates | 412 | \u{2588}\u{2588}\u{2588} 80% |";
        let other = "| docs | 1.5k | \u{258f} <1% |";
        vec![
            vec![],
            vec![""],
            vec!["**Lines added: none**"],
            vec!["", "**Lines added: none**", ""],
            vec!["**Lines added: none**", "| x |"],
            vec!["**Lines added: 1**"],
            vec!["**Lines added: 0**"],
            vec!["**Lines added: 1**", header, separator],
            vec!["**Lines added: 412**", "", header, separator, row],
            vec!["**Lines added: 2k**", header, separator, row, other],
            vec!["**Lines added: 2k**", header, separator, row, row],
            vec![
                "**Lines added: 2k**",
                "| Directory | Added |",
                separator,
                row,
            ],
            vec![
                "**Lines added: 2k**",
                "Directory | Added | Share",
                separator,
                row,
            ],
            vec!["**Lines added: 2k**", header, "|---|---|", row],
            vec!["**Lines added: 2k**", header, "|:-:|-:|:--|", row],
            vec!["**Lines added: 2k**", header, "|:--|--x|:--|", row],
            vec!["**Lines added: 2k**", header, separator, "| crates | 412 |"],
            vec![
                "**Lines added: 2k**",
                header,
                separator,
                "| | 412 | \u{2588} 5% |",
            ],
            vec![
                "**Lines added: 2k**",
                header,
                separator,
                "| a | 0412 | \u{2588} 5% |",
            ],
            vec!["**Lines added: 2k**", header, separator, "| a | 41 | 5% |"],
            vec![
                "**Lines added: 2k**",
                header,
                separator,
                "| a | 41 | \u{2588}5% |",
            ],
            vec![
                "**Lines added: 2k**",
                header,
                separator,
                "| a | 41 | \u{2588} 1000% |",
            ],
            vec![
                "**Lines added: 2k**",
                header,
                separator,
                "| a | 41 | x 5% |",
            ],
            vec![
                "**Lines added: 2k**",
                header,
                separator,
                "| a\\|b | 41 | \u{2588} 5% |",
            ],
            vec![
                "**Lines added: 2k**",
                header,
                separator,
                "| a | 41 | \u{2588} 5% \\|",
            ],
            vec![
                "**Lines added: 2k**",
                header,
                separator,
                "| a | 23k | \u{2588} 5% |",
                "x",
            ],
            vec!["| crates | 412 |"],
            vec!["**Lines added: many**"],
        ]
    }

    /// GA second fix item 1: af's reading of a block's body agrees with `parse` of
    /// `scripts/pr-lines.py`, which `scripts/check-pr-report.py` uses, on every body above: the
    /// test asks the script itself, af never does.
    #[test]
    fn the_block_rules_are_the_parsers() {
        let script = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .join(PR_LINES);
        let cases = bodies();
        let program = "import importlib.util, json, sys\n\
             spec = importlib.util.spec_from_file_location('pr_lines', sys.argv[1])\n\
             module = importlib.util.module_from_spec(spec)\n\
             spec.loader.exec_module(module)\n\
             cases = json.load(sys.stdin)\n\
             print(json.dumps([module.parse(lines)[0] is not None for lines in cases]))\n";
        let mut child = std::process::Command::new("python3")
            .args(["-I", "-c", program])
            .arg(&script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let input = serde_json::to_vec(&cases).unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), &input).unwrap();
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let accepted: Vec<bool> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(accepted.len(), cases.len());
        assert!(accepted.iter().any(|a| *a) && accepted.iter().any(|a| !*a));
        for (lines, parsed) in cases.iter().zip(accepted) {
            let problem = body_problem(lines);
            assert_eq!(problem.is_none(), parsed, "{lines:?}: {problem:?}");
        }
    }
}
