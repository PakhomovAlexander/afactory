//! `af task report IDS… --pr-body` (ADR-0211): the pull request description this repository's
//! template requires, ready for `gh pr create --body-file`: the template with the `af task
//! report` block between its `<!-- af-task-report:v1 -->` markers, and the lines added block
//! `scripts/pr-lines.py` prints between its `<!-- af-pr-lines:v1 -->` markers when the script
//! exists (ADR-0142, ADR-0195). A checkout that has the script and cannot run it to a block gets
//! no body at all: a body without its block is one the pull request check refuses.

use std::path::{Path, PathBuf};
use std::time::Duration;

use af_contracts::task::execution_summary::{TASK_REPORT_BEGIN, TASK_REPORT_END};

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
    let cause = match af_process::run_supervised(&mut command, None, PR_LINES_TIMEOUT) {
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
const HEADER: [&str; 3] = ["Area", "Changes", "Share of additions"];
const INDENT: &str = "&nbsp;&nbsp;&nbsp;&nbsp;";
const KINDS: [&str; 3] = ["main", "tests", "other"];
const SEPARATOR_ROW: &str = "|:--|--:|:--|";

/// What is wrong with the block's body `lines`, by the rules of `parse` in
/// `scripts/pr-lines.py`, which `scripts/check-pr-report.py` reads the block with: the title
/// `**Lines added: <figure>**`, then the table header, its separator and at least one row per
/// directory, with crates and their code kinds nested in the same table; or
/// `**Lines added: none**` alone. Blank lines do not count.
/// `None` when the parser reads it back. Numeric correctness belongs to canonical regeneration
/// from commits: rounded figures cannot prove exact parent sums.
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
    let mut paths = Vec::new();
    let mut stack: Vec<(String, bool, String)> = Vec::new();
    let mut branches = Vec::new();
    let mut parents = Vec::new();
    for line in &lines[3..] {
        let Some([area, changes, share]) =
            row_cells(line).and_then(|c| <[String; 3]>::try_from(c).ok())
        else {
            return Some("its block has a line that is not a row of three cells".into());
        };
        let (mut name, mut depth) = (area.as_str(), 0);
        while let Some(rest) = name.strip_prefix(INDENT) {
            depth += 1;
            name = rest;
        }
        let unbold = |text: &str| {
            text.strip_prefix("**")
                .and_then(|t| t.strip_suffix("**"))
                .map(str::to_owned)
        };
        let bold = unbold(name).is_some();
        let (name, changes, share) = if bold {
            let (Some(name), Some(changes), Some(share)) =
                (unbold(name), unbold(&changes), unbold(&share))
            else {
                return Some("its block has an inconsistent bold row".into());
            };
            (name, changes, share)
        } else {
            (name.to_owned(), changes, share)
        };
        if depth > 2 || depth > stack.len() {
            return Some("its block has a missing hierarchy parent".into());
        }
        stack.truncate(depth);
        if !safe_name(&name) {
            return Some("its block has an invalid hierarchy name".into());
        }
        let Some((added, deleted)) = changes.strip_prefix('+').and_then(|s| s.split_once("/−"))
        else {
            return Some("its block has invalid changes".into());
        };
        if !is_figure(added)
            || !is_figure(deleted)
            || (added == "0" && deleted == "0")
            || !is_share(&share)
        {
            return Some("its block has invalid changes or share".into());
        }
        if (depth == 0 && !bold)
            || (depth == 2 && bold)
            || (depth > 0
                && (stack[0].0 != "crates"
                    || (depth == 2 && (!stack[1].1 || !KINDS.contains(&name.as_str())))))
        {
            return Some("its block has an invalid hierarchy or bold row".into());
        }
        let denominator = stack.last().map_or(total, |row| row.2.as_str());
        if (denominator == "0" && share != "—")
            || (denominator != "0" && (share == "—" || (added == "0") != (share == "0%")))
        {
            return Some("its block has an invalid addition share denominator".into());
        }
        let mut path: Vec<String> = stack.iter().map(|row| row.0.clone()).collect();
        path.push(name.clone());
        if paths.contains(&path) {
            return Some("its block has two rows in the same hierarchy parent".into());
        }
        if depth > 0 {
            parents.push(path[..depth].to_vec());
        }
        if depth == 1 && bold {
            branches.push(path.clone());
        }
        paths.push(path);
        stack.push((name, bold, added.to_owned()));
    }
    if branches.iter().any(|path| !parents.contains(path)) {
        return Some("its block has a hierarchy branch with no children".into());
    }
    None
}

/// Names contain literal safe characters or canonical numeric entities for syntax/whitespace.
/// An escaped ampersand cannot become structural indentation or bold markup.
fn safe_name(mut name: &str) -> bool {
    let quoted = name.starts_with("&#34;") && name.ends_with("&#34;");
    let mut whitespace = false;
    if name.is_empty() {
        return false;
    }
    let escaped = |c: char| {
        c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c) || "\\|&<>`*_[]~↳\"".contains(c)
    };
    while !name.is_empty() {
        if let Some(rest) = name.strip_prefix("&#") {
            let Some((digits, rest)) = rest.split_once(';') else {
                return false;
            };
            let Some(c) = digits.parse::<u32>().ok().and_then(char::from_u32) else {
                return false;
            };
            whitespace |= c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c);
            if !escaped(c) || digits != u32::from(c).to_string() {
                return false;
            }
            name = rest;
        } else {
            let c = name.chars().next().expect("nonempty name");
            if escaped(c) || c == '/' {
                return false;
            }
            name = &name[c.len_utf8()..];
        }
    }
    !whitespace || quoted
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
/// with an optional decimal and `k` (`18.9k`); thousands have no digit cap.
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
    let digits = |text: &str| {
        !text.is_empty() && !text.starts_with('0') && text.bytes().all(|b| b.is_ascii_digit())
    };
    match thousands.split_once('.') {
        Some((whole, tenth)) => {
            digits(whole) && tenth.len() == 1 && tenth.bytes().all(|b| b.is_ascii_digit())
        }
        None => digits(thousands),
    }
}

/// A parent-relative addition percentage, or an undefined zero-denominator share.
fn is_share(share: &str) -> bool {
    if matches!(share, "—" | "<1%" | "0%" | "100%") {
        return true;
    }
    share.strip_suffix('%').is_some_and(|n| {
        (1..=2).contains(&n.len()) && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit())
    })
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
                     `| Area | Changes | Share of additions |`, its separator `|:--|--:|:--|` and one row \
                     per directory",
                ),
            ),
            (
                "print('<!-- af-pr-lines:v1 -->')\nprint('**Lines added: 18.9k**')\nprint()\n\
                 print('| Area | Changes | Share of additions |')\nprint('|:--|--:|:--|')\n\
                 print('| **docs** | **+18.9k/−0** | **100%** |')\n\
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

    /// Both parsers read valid generated reports and reject malformed structure/count/share rows.
    #[test]
    fn the_block_rules_are_the_parsers() {
        let script = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .join(PR_LINES);
        let program = r#"import importlib.util, json, sys
spec = importlib.util.spec_from_file_location('pr_lines', sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
entries = [(3, 2, 'crates/a/src/lib.rs'), (0, 4, 'crates/a/tests/t.rs'),
           (0, 2, 'crates/b/src/lib.rs'), (1, 1, 'crates/direct'), (0, 8, 'gone/a')]
valid = m.render_entries(entries)
zero = m.render_entries([(0, 2, 'crates/a/src/lib.rs')])
safe = m.render_entries([(1, 0, 'crates/↳ &nbsp;**雪\t/src/lib.rs'), (1, 0, 'root')])
cases = [([], False), (['**Lines added: none**'], True),
         (['**Lines added: none**', 'extra'], False),
         (['**Lines added: 1**'], False), (['**Lines added: 1000**'], False)]
collisions = []
for name in ['a', ' &nbsp;**雪|\t\u00a0', 'a (file)']:
    collisions.append(m.render_entries([(3, 0, f'crates/{name}/src/lib.rs'),
        (0, 2, f'crates/{name}'), (1, 0, f'crates/{name} (file)'),
        (1, 0, f'crates/{name} (file2)/src/lib.rs')]))
collisions.append(m.render_entries([(3, 0, 'crates/a/src/lib.rs'), (0, 2, 'crates'),
                                   (1, 0, 'crates (file)')]))
for body in [*collisions, valid, zero, safe, m.render_entries([(2500000, 1, 'docs/a')]),
             m.render_entries([(1, 0, 'crates')]),
             m.render_entries([(1, 0, 'crates/\u00a0\u00a0\u00a0\u00a0/src/lib.rs')])]:
    cases.append((body.splitlines(), True))
for before, after in [
    ('Area', 'Directory'), ('|:--|--:|:--|', '|---|---|'),
    ('**gone**', '**&#160;fake**'), ('**gone**', '**&#034;fake**'),
    ('**gone**', '**a&#999999999999999999999999;**'),
    ('**gone**', '**fake↳**'), ('**gone**', '**fake/**'),
    ('+3/−2', '+3/-2'), ('+3/−2', '+3/−2.2'),
    ('+3/−2', '+3/−1000'), ('**100%**', '**1000%**'),
    ('&nbsp;&nbsp;&nbsp;&nbsp;**a**', '&nbsp;&nbsp;&nbsp;&nbsp;'*3+'**a**'),
    ('+3/−6', '+0/−0'), ('+3/−2', '+03/−2'),
    ('**100%**', '**101%**'), ('**100%**', '**—**'), (' 0% ', ' <1% '),
    ('**crates**', 'crates'), ('**+4/−9**', '+4/−8'),
    ('&nbsp;&nbsp;&nbsp;&nbsp;**a**', '&nbsp;**a**'),
    ('&nbsp;&nbsp;&nbsp;&nbsp;**a**', '&nbsp;&nbsp;&nbsp;&nbsp;a'),
    ('&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;tests', '&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;shared'),
    ('**gone**', '**gone/**'), ('**gone**', '**&nbsp;gone**')]:
    assert before in valid, before
    cases.append((valid.replace(before, after).splitlines(), False))
for transform in [lambda rows: rows[:4]+rows[5:], lambda rows: rows+rows[4:5],
                  lambda rows: rows[:5]+rows[6:]]:
    cases.append((transform(valid.splitlines()), False))
for rows, expected in cases:
    assert (m.parse(rows)[0] is not None) == expected, rows
print(json.dumps(cases))
"#;
        let output = std::process::Command::new("python3")
            .args(["-I", "-c", program])
            .arg(script)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let cases: Vec<(Vec<String>, bool)> = serde_json::from_slice(&output.stdout).unwrap();
        for (lines, expected) in cases {
            let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
            let problem = body_problem(&borrowed);
            assert_eq!(problem.is_none(), expected, "{lines:?}: {problem:?}");
        }
    }
}
