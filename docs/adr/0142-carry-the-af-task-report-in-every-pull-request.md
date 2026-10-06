# ADR-0142: Carry the `af task report` of its Tasks in every pull request

**Status:** accepted (2026-10-06)

## Context

Afactory is built with Afactory: implementation and verification run as af Tasks, and the
kernel records what each one did — its plan, Attempts, failures, tokens and times — in the Store
on the machine that ran it. None of that reaches the pull request. A reviewer sees the diff and
the checks, but not whether the change came out of a Task at all, which pipeline produced it,
how many review rounds it took, how many Attempts failed and why, or what it cost. Those figures
exist only as `af task show --json` documents nobody pastes, and they are the evidence that the
working agreement in `AGENTS.md` is being followed.

Two things are missing: a command that turns recorded Tasks into a short, reviewable summary, and
a rule, enforced where pull requests are reviewed, that every change carries one.

## Considered options

- **A free-form note in the description.** Nothing to build, and nothing to check: each author
  writes different figures in a different shape, an estimate reads like a measurement, and a
  missing note is noticed by nobody. Rejected.
- **A bot comment posted by CI.** A comment needs a token with write access to pull requests,
  and CI has no Store to read the figures from: the Tasks ran on the author's machine. The bot
  could only nag, and a nag in a comment is easier to ignore than a failed check. Rejected.
- **Verify the report against the Store in CI.** The strongest check, and not reachable: the
  Store is local state outside the repository, holding Provider bindings and auth directories
  that must never be uploaded, and CI cannot see it. A check that would need the Store published
  is a weaker boundary, not a stronger gate. Rejected; the check below verifies the block's
  shape, and the figures in it are the kernel's because the kernel prints them.
- **A command that prints the block, and a check that requires it.** Chosen.

## Decision

1. **The rule.** Every change to this repository is made through af Tasks — implementation and
   verification pipelines — and its pull request description carries the `af task report` of
   those Tasks. `AGENTS.md` and `CONTRIBUTING.md` state it, and the pull request template holds
   the section, the markers and the command.
2. **The command.** `af task report TASK_ID... [--repo DIR] [--state DIR] [--json]` reports one
   or more Tasks of one Store, in the order given. An unknown ID, or one named twice, is an error
   that names it and prints nothing else. It is read-only: it opens the Store read-only, reads
   what `af task show` and `af task list` read (Task records, execution records, review rounds,
   run reports, attempt walls and the event times of the Task log), never dispatches a Worker,
   contacts a Provider, writes the Store or changes a Task.
3. **What it reports, per Task.** ID; kind; pipeline as `name@version`; outcome as `af task show`
   states it; review rounds; runs; Attempts, failed Attempts by reason class and the tokens
   charged to them; chargeable tokens; wall time; active time; and per node the role, the Worker
   as Provider kind, model and effort (`codex gpt-6-sol/high`, or `command`), Attempts, tokens,
   elapsed time and the gate's check results with their spans. A totals line sums Tasks, rounds,
   Attempts (failed), tokens and active time.
   - A **run** is a writer lease during which the Task executed work. Every Task command holds its
     own lease, at its own epoch: `af task start --execute` is the first run and every `af task
     run` that resumed it is another, while approving, refreshing or delivering a Task is not.
     Settling or releasing an earlier writer's pending Attempts, which every command that takes
     a lease does first, is not work, and a lease that refreshed the source is never a run
     ([ADR-0143](0143-judge-the-pull-request-report-with-trusted-code-and-per-attempt-figures.md)).
   - Each Attempt's **Worker** is resolved from the plan that Attempt ran under. A node is one
     row per distinct Worker binding, so after `af task refresh` binds a node to another Worker,
     no Attempt's tokens are charged to a Worker it did not use.
   - **Wall time** runs from the Task's first recorded event to its last; a collection tombstone
     is the collector's event, not the Task's. **Active time** is the sum of the runs' spans, each
     from its first event to its last, so time spent waiting between runs is not counted as work.
   - A failed Attempt's **reason class** is its typed retry feedback code (`provider_failure`,
     `process_failure`, `invalid_output_contract`, …), `abandoned` for an Attempt recovery
     settled, and `unclassified` for a failure that recorded no feedback. Diagnostic prose is
     never read for it.
   - **Nothing is estimated.** A figure the Store does not record is absent from the JSON document
     and printed as `unknown`; a sum over an unknown part is unknown. A node's elapsed time is its
     attempt walls, or its Attempts' start and settlement times, and is unknown unless every
     Attempt recorded one. A collected Task (ADR-0135) keeps its row from the tombstone: kind,
     outcome, tokens and event times; its Attempts, nodes, rounds and pipeline are unknown.
4. **The block.** The default output is Markdown between the exact lines
   `<!-- af-task-report:v1 -->` and `<!-- /af-task-report -->`: a heading; one summary table, one
   row per Task, with the columns Task, Kind, Pipeline, Outcome, Rounds, Attempts, Tokens, Active
   time and Wall time; the totals line, starting `**Totals:**`; and each Task's node breakdown in
   a collapsed `<details>` element. Token counts carry thousands separators (`205,295`). It
   renders on GitHub and reads as plain text in a terminal.
   The `v1` in the marker is the contract's version: a change to the markers or the columns is a
   new version, and the check accepts the versions it knows.
5. **The document.** `--json` prints one `af/task-report@1` document, `schemas/task-report-v1.json`,
   with the same figures: tokens as exact decimal text, times in milliseconds.
6. **Privacy.** The report names a Provider only by kind, model and effort. It never contains a
   Provider registry ID or label, a principal, an auth directory, a state directory, a home path,
   a credential, a prompt or Worker output. Every object of the document is closed, so the schema
   refuses any of them, and every value in the Markdown is sanitized display text that can
   neither end a table cell nor open markup: a backslash and a `|` are written as the character
   references `&#92;` and `&#124;`. A model value is copied only when it looks like a model
   identity — 1 to 128 letters, digits and `._:/@+-`, not starting with `/` or `~`, without
   `..`, and with no path segment that is hidden or names a home, auth or state directory —
   and is `unknown` in both forms otherwise, since a recorded model may be a path.
7. **The check.** `.github/workflows/pr-report.yml` runs on `pull_request_target` (`opened`,
   `edited`, `synchronize`, `reopened`, `ready_for_review`) with `contents: read`. It runs only
   trusted code: `pull_request_target` runs the workflow as the base branch holds it, the job
   checks out the base branch, never the pull request's head or merge ref, and runs the base's
   `scripts/check-pr-report.py` on the event payload GitHub hands the job
   (`$GITHUB_EVENT_PATH`): no API call, no token, and no code from the pull request. A pull
   request can therefore change neither the workflow nor the checker that judges it; a change to
   either applies from the pull requests after it merges. A pull request opened by
   `dependabot[bot]`, or from a `release/` branch, is exempt — neither is made by an af Task.
   Every other pull request passes only when its description holds exactly one well-formed
   block: both markers in order, the summary table header with every column, a separator row of
   the header's width, at least one Task row, and the totals line. A Task row has exactly the
   header's number of cells, split as GitHub splits them, and nonempty Task, Kind, Pipeline and
   Outcome cells. Each missing or malformed part fails with its own message, and every failure
   says how to produce the block with `af task report`.
   `scripts/test-check-pr-report.py`, run by `make check`, accepts the checked-in block the real
   renderer printed for a test Store (`fixtures/task-report/`), and the renderer's own tests
   require it still prints that block, so the checker and the renderer cannot drift apart.

## Consequences

- A reviewer sees, in the description, which Tasks produced a change, through which pipelines,
  in how many rounds, Attempts and failures, and at what cost — the kernel's figures, not an
  author's recollection.
- The check proves the block is there and well formed, not that its figures match a Store: CI
  cannot reach one. Pasting an edited block remains possible and remains a review matter, like
  any other false statement in a description.
- A change made outside af Tasks cannot be merged through the normal path without saying so; the
  check fails until the description carries a report. Dependabot and release pull requests stay
  automatic.
- Adding a column, renaming a marker or changing the totals line is a contract change: a new
  marker version, schema and checker together, never an edit that old descriptions fail.
- The command reads and never writes, so it is safe on a Store a running Task holds; a figure a
  live Task has not recorded yet is reported as what the Store holds at that moment.
