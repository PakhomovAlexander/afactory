# ADR-0269: Let CI maintain the lines added block

**Status:** proposed (2026-10-30). Supersedes in part
[ADR-0195](0195-state-the-lines-a-pull-request-adds-per-directory-and-check-them.md), whose
rejections of a bot that writes the figures and of tolerating a stale block it replaces, and the
`contents: read` alone of the `PR report` job in
[ADR-0142](0142-carry-the-af-task-report-in-every-pull-request.md); it keeps the rest of both.

## Context

ADR-0195 has every pull request description carry the lines it adds per directory, between
`<!-- af-pr-lines:v1 -->` and `<!-- /af-pr-lines -->`, and has the `PR report` check render the
block again from the pull request's commits and refuse a description whose block is missing or
differs. It rejected a bot that writes the figures, because the write needs a token with write
access to pull requests under `pull_request_target`, which the report check was designed not to
hold, and because a bot's edit races the author's; and it rejected tolerating a stale block,
because the figures would then say nothing about what is reviewed.

The block is pasted when the pull request is opened. Every push, and every merge from main into
a long-lived branch, can move a figure, a bar or a share, and then the check goes red for a
mechanical reason: nothing about the change or its Tasks is wrong, only a block nobody had a
reason to refresh. Each time it costs the author a run of `scripts/pr-lines.py`, an edit and
another round of the check, and a red check on a pull request that is otherwise ready reads as
a problem with the change. The owner decided on 2026-10-30 that a pull request must not fail the
check for that reason, while a figure nobody verified must still never stand.

## Considered options

- **Keep refusing the stale block (ADR-0195).** Correct figures, at a round per push. Rejected
  by the owner's decision: the cost lands on every long-lived pull request, for no finding.
- **Tolerate a stale block: check the shape only.** No write, and no round, but the figures in
  the description would no longer be the commits'. Rejected, as ADR-0195 rejected it.
- **Comment the block, or report it in the check's summary.** No description write, but the
  description stays stale and the reader has two places to look. Rejected.
- **Let CI write the block into the description.** The check already renders the block from the
  commits; writing it where the author would have pasted it makes the figures the commits' on
  every push without the author's help. The write is bounded: the job holds `pull-requests:
  write` beside `contents: read`, still checks out only the base branch and runs no code of the
  pull request, and the one call that writes sends the description's `body` alone, changed only
  between the lines markers. Chosen.

## Decision

1. **The check maintains the block.** The `PR report` workflow (`.github/workflows/pr-report.yml`,
   `pull_request_target`, base checkout only) runs `scripts/check-pr-report.py` on the event
   payload with `--refresh-into FILE`. The check renders the block from the pull request's commits
   exactly as ADR-0195 says. A block that matches passes and writes nothing. A block that differs,
   or a description without the markers, passes when the Task report block is well formed, and
   FILE is written; the job then reads the description once more through the API
   (`GET /repos/{owner}/{repo}/pulls/{number}`), so an edit made since the event is kept, runs the
   same check with `--body` on it and the event's `--diff BASE HEAD`, and writes the result back
   (`PATCH`, the `body` field alone, `gh api -F body=@FILE`) with the job's `GITHUB_TOKEN`. The job
   prints that it refreshed the block.
2. **What the write changes.** Only the text between the one pair of lines markers, replaced by
   the rendered block, the markers kept; without the markers, the whole block, inserted after a
   blank line right after the `<!-- /af-task-report -->` marker (at the end of a description
   without exactly one report block). Markers inside a code block or a raw HTML block are text, as
   the check reads them: they stay, and the real block is inserted. Every other byte of the
   description is kept, its line endings included; the inserted lines end as the line they follow.
   The refreshed description must pass both checks before it is written. Two lines blocks, or a
   marker without its pair, have no one place for the block: the check refuses them as before.
3. **A refused write fails as before.** When `gh` exits non-zero — the token cannot write that
   pull request's description, or the API fails — the job runs the check on the event payload
   without the flag, which refuses the stale or absent block and prints the block to paste. The
   check prints the block for an absent or misshapen block too, not only for a stale one.
4. **What stays.** The Task report block is the author's, and its rules, messages and exemptions
   are those of ADR-0142: a missing or malformed report block fails the check, and nothing is
   written. `scripts/pr-lines.py`'s counting, figures, rounding, bars, shares and markers are those
   of ADR-0195, and so are the exemptions. Authors may paste the block or leave the template's
   markers empty; `python3 scripts/check-pr-report.py --body FILE --diff BASE HEAD` still runs the
   comparison locally, and `--refresh-into FILE` writes the refreshed description there.
5. **The loop ends.** GitHub starts no workflow run for an edit made with the job's
   `GITHUB_TOKEN`; a run that did would find the block equal and write nothing. The workflow's
   `concurrency` group, the pull request number with `cancel-in-progress`, stays.
6. **Tests.** `scripts/test-check-pr-report.py` checks the replacement as a pure function — a stale
   block replaced in place, absent markers inserted after the report block, inserted at the end
   without one, markers inside a code fence left alone, CRLF and mixed line endings kept, no place
   for two blocks or an unpaired marker — and `--refresh-into` through the script's real entry on a
   repository of its own: stale and absent blocks written, a matching block writing nothing, a
   malformed report block failing with its message and writing nothing.

## Consequences

- A push or a merge from main no longer turns the check red for its lines block: the figures in
  the description are the commits' after every run, and the author pastes the block only when
  the job cannot write it.
- The job holds `pull-requests: write`. A pull request still cannot change the workflow or the
  checker, and none of its code runs, so the token is used only by the base's code, to write the
  description of the pull request the event names. A directory name in the pull request's commits
  reaches the description through the rendered table, as it did when the author pasted it; a
  name that would break the block's shape fails the check before the write.
- Between the API read and the write there is a window, a fraction of a second, in which an
  author's edit would be overwritten; a later edit raises its own run. The event's own description
  decides whether to write, and the description as it is now is what is written.
- ADR-0195's statement that the job holds no token beyond `contents: read` no longer holds, and
  ADR-0142's that the check makes no API call holds for the check, not for the job: the check
  still reads the description from the event payload, and the job makes one read and one write.
