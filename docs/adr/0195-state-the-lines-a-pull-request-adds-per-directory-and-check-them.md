# ADR-0195: State the lines a pull request adds per directory, and check them

**Status:** accepted (2026-10-22).

## Context

A pull request here carries its `af task report` (ADR-0142), so a reviewer sees how the change
was made and what its review found. What the reviewer does not see, before opening the diff, is
how big the change is and where it lands: whether a pull request that says "telemetry streams"
is ten thousand lines of crates or ten thousand lines of fixtures, how much of it is tests, and
how much is documentation. GitHub shows one total of additions and deletions, which says
nothing about the shape; `git diff --stat` says it per file, which for a large change is
hundreds of rows. The owner asked for the figure per directory in every description — `docs/10k
tests/5k` — and for CI to check it; then, reading that one-line form on the first pull requests
that carried it, for something easier on the eye.

The check has the constraints of ADR-0142: only trusted code may judge a pull request, so the
checker runs from the base branch under `pull_request_target`; the description is read from the
event payload; and a figure nobody verifies is a figure nobody trusts. Unlike the Task report,
whose Store stays on the author's machine, the lines a pull request adds are a property of its
commits, which CI can reach: the figures can be recomputed and compared, not only shaped.

## Considered options

- **Leave it to the reader.** GitHub's "+12,345 −678" and the files tab. The total hides the
  shape, the files tab needs scrolling and mental arithmetic for a large change, and nothing
  says how much of the addition is tests. Rejected.
- **A free-form line in the description.** Written differently by each author, or not at all,
  and an estimate reads like a measurement. Rejected: one command prints the line and one
  check requires it, as ADR-0142 does for the report.
- **A bot that writes the figures into the description, or comments them.** Needs a token with
  write access to pull requests under `pull_request_target`, which the report check was designed
  not to hold; a comment is easier to ignore than a failed check; and an edit by a bot races the
  author's edits. Rejected: the author pastes the block, the check verifies it.
- **Compute the figures in CI from GitHub's pull request files API.** Needs the `pull-requests`
  permission and an API call per hundred files, and its additions differ from git's at the
  edges (renames below GitHub's threshold, files it marks as too large). Rejected for fetching
  the pull request's commits as objects into the base checkout and reading them with git, the
  same command the author ran.
- **Check out the head to count.** Runs or builds nothing by itself, but every step after it
  would see the pull request's files. Rejected: `git fetch` writes objects only; `git diff
  --numstat --no-ext-diff` reads them without a checkout, so nothing from the pull request is
  on disk as a file, let alone run (the fetch runs no hook of the fetched tree).
- **Per crate rather than per top-level directory.** Thirty rows for a change that touches the
  workspace, in a block meant to be read at a glance. Rejected for the first path component,
  with two refinements: a file under a `tests` directory at any depth counts as `tests`, since
  that is where integration tests live (`crates/af/tests/it`) and the question "how much of
  this is tests" is the one most asked, and a file at the root counts under its own name, so a
  `Cargo.lock` change is not hidden in a directory it is not in. Unit tests inside a source
  file (`#[cfg(test)]`) count with their crate: a line count cannot see them.
- **Exact counts.** `crates/23147 tests/23286` is precise and unreadable, and every rebase
  changes it. Rejected for a rounded figure — exact below 1000, to a tenth of a thousand below
  10k, to a thousand from there — which the check compares as the tool renders it, so a
  rebase that moves a large directory by a few lines leaves the figure as it was.
- **One line, `crates/12k docs/10k tests/5k`.** The form first asked for, and built first: nine
  directories on one line read as a wall, with nothing to say how big the whole is or how the
  directories compare. Rejected, on the owner's reading, for a title with the total and a table
  with one row per directory — its figure, and its share as a bar and a percentage — which
  GitHub renders as a table; the markers, the figures and the check are the same.
- **Count deletions too, or net lines.** Deletions are the easier half to review, and net lines
  hide a rewrite as a small change. Rejected; additions alone, as asked.
- **Tolerate a stale block after a push.** The figures then say nothing about what is being
  reviewed. Rejected: the check runs on every push and edit, refuses a stale block, and prints
  the right line to paste.

## Decision

1. **The rule.** Every pull request description states the lines the pull request adds, per
   directory, in a `Lines added` block between the markers `<!-- af-pr-lines:v1 -->` and
   `<!-- /af-pr-lines -->`, under its `af task report` block. `AGENTS.md` and `CONTRIBUTING.md`
   state it, and the pull request template holds the section, the markers and the command.
2. **The tool.** `python3 scripts/pr-lines.py [--repo DIR] [--base REF] [--head REF]` prints
   the block for `BASE...HEAD` — the three-dot range GitHub shows, `origin/main` and `HEAD` by
   default, a stacked pull request naming its base branch — from `git diff --numstat -M
   --no-ext-diff`. Each added line counts under the first component of its path; a file under
   a directory named `tests` at any depth counts under `tests`; a file at the repository root
   counts under its own name. A rename adds what changed in the file; a binary file adds
   nothing; a directory with nothing added is left out.
3. **The block.** A title with the total, `**Lines added: 18.9k**`, a blank line, and a table
   with the header `| Directory | Added | Share |`, its separator `|:--|--:|:--|`, and one row
   per directory, the largest first and ties by name: the directory (a `|` in its name escaped
   as `\|`), its figure, and its share as a bar followed by a percentage of the total. A figure
   is the exact count below 1000; `n.tk` or `nk` to a tenth of a thousand from 1000 to below
   10,000 (`1k`, `7.9k`, `5k` rather than `5.0k`); `nk` to a thousand from 10,000 (`23k`);
   halves round up. The bar is the row's count against the largest row's, in up to twelve
   cells drawn in eighth blocks (`▏▎▍▌▋▊▉█`), a sliver at least; the percentage rounds half
   up and reads `<1%` below a half. `**Lines added: none**` alone when nothing was added.
4. **The check.** `scripts/check-pr-report.py`, run by the `PR report` workflow from the base
   branch on every opening, edit, push and reopening, requires, beside the report block, exactly
   one lines block that GitHub renders as text (the rules of ADR-0142 on code blocks, raw HTML
   and comments apply) holding the title and the table of that form; it then counts the pull
   request's lines itself, from the `base.sha` and `head.sha` of the event, renders the block
   again and refuses one that differs, printing the block to paste. For that the job checks out
   the base branch with its history and fetches the pull request's commits as objects
   (`refs/pull/N/head`, then the event's head by hash if the ref has moved on); it checks out
   nothing of them. The block is compared as rendered — the total, and each row's directory,
   figure, bar and share — with the rows in any order. Dependabot pull requests and `release/`
   pull requests of this repository are exempt, as they are from the report. Locally,
   `python3 scripts/check-pr-report.py --body FILE --diff BASE HEAD` runs the same check; without
   `--diff` it checks the shape alone and says so.
5. **Tests.** `scripts/test-pr-lines.py` builds a repository with additions, a rename, a
   deletion, a binary change and a base that moved on, and checks the counts, the figures, the
   bars and shares, the rendering, the parse and the comparison through both scripts' real
   entries; `make check` runs it.

## Consequences

- A reviewer reads the size and the shape of a change before the diff, and the figures are the
  commits', not the author's estimate: a stale block fails the check with the block to paste.
- Each push costs the author one `pr-lines.py` run and, when the block changed, one description
  edit, as a new Task costs a report update. The figures move seldom; the bars and shares,
  drawn finer, move more often, so the rule is to run the tool after every push and paste when
  it differs, and the check says exactly what to paste.
- The `PR report` job fetches the base branch's history and the pull request's commits, a few
  seconds more than the single-commit checkout it used. It still runs only the base's code,
  still reads the description from the event, and still holds no token beyond `contents: read`:
  the repository is public, so the fetch needs none; were it private, the fetch would fail and
  the check with it, loudly, never silently passing.
- The block counts what `git diff --numstat` counts: lines of text files. Generated or vendored
  files count like any other, under their directory; a reviewer who wants them apart asks for a
  directory of their own.
