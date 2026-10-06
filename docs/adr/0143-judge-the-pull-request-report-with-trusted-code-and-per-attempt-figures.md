# ADR-0143: Judge the pull request report with trusted code and per-Attempt figures

**Status:** accepted (2026-10-06)

## Context

[ADR-0142](0142-carry-the-af-task-report-in-every-pull-request.md) added `af task report` and
the `PR report` check. Review found six places where the shipped command or check could
mislead the reviewer it serves:

- The checker accepted any table row as a Task row, so a one-cell placeholder (`| x |`), a short
  row, a row without its Task, or a separator narrower than the header passed.
- The workflow ran on `pull_request` and executed the checker from the pull request's own
  checkout, so a pull request could edit the checker or the workflow that judged it.
- The report copied a Worker's model verbatim. A model value is free text in a plan, and a path
  into a home, auth or state directory would have been published in the description.
- A cell escaped `|` as `\|` but left a backslash alone, so a value such as `m\|x` became
  `m\\|x`, an escaped backslash followed by a cell boundary.
- A node's Worker came from the plan of its first Attempt. After `af task refresh` binds the
  node to another Worker, later Attempts' tokens were charged to the earlier Worker.
- Every lease that recorded an execution counted as a run. `af task refresh` first settles or
  releases the Attempts a dead writer left pending, so a refresh counted as a run and its lease
  time as active time.

## Considered options

- **Keep `pull_request` and pin the checker by hash in the workflow.** The workflow file itself
  comes from the pull request under `pull_request`, so the pin can be edited with it. Rejected.
- **`pull_request_target` checking out the head to read the description from the tree.** That
  runs pull request code with the base's context. Rejected: the description is in the event
  payload, and nothing from the head is needed.
- **Redact known paths (the state directory, `$HOME`) from the model.** Only covers the
  machine that rendered the report, and only paths it knows. Rejected for an allowlist of what a
  model identity looks like.
- **A `mixed` Worker for a node bound to several Workers.** Correct but loses each Worker's
  share. Rejected for one row per node and Worker binding.
- **Count a lease as a run only when it began an Attempt.** A resumed run that only finished or
  published the result would not count. Rejected for treating recovery as not-work.

## Decision

1. **Trusted check.** `.github/workflows/pr-report.yml` runs on `pull_request_target` with
   `contents: read`, checks out the base branch (never the head or merge ref), runs the base's
   `scripts/check-pr-report.py`, and reads the description only from the event payload. Its
   concurrency group is the pull request number, since `github.ref` is the shared base branch
   under this event. The workflow's comment and ADR-0142's check section say why.
2. **Well-formed rows.** The checker splits a row as GitHub does (a backslash escapes the next
   character), requires the separator row to have the header's width, and accepts a Task row
   only with exactly the header's number of cells and nonempty Task, Kind, Pipeline and Outcome
   cells. A placeholder row, a short or long row, an empty required cell and a separator of the
   wrong width each fail with their own message.
3. **Model identity.** `review_core::task::task_report::is_model_identity` is the one rule: 1 to
   128 letters, digits and `._:/@+-`, not starting with `/` (nor `~`, outside the alphabet), no
   `..`, and no `/`-separated segment that starts with `.` or is `home`, `users`, `root`, `tmp`,
   `var`, `private`, `state`, `auth` or `auth.*` in any case. The reader writes `unknown` for
   anything else, in both forms; the document's validation and the schema's `model` pattern
   refuse it, and a parity test holds the pattern to the Rust rule value by value.
4. **Cells.** A cell writes a backslash as `&#92;` and `|` as `&#124;`, before `<` and `>`, so no
   recorded value can end its cell in the table or in the `<summary>` line.
5. **Per-Attempt Workers.** Each started Attempt's role and Worker come from the plan that
   Attempt ran under, and the breakdown has one row per node, role and Worker. A node keeps one
   row while every plan binds it to the same Worker. `af/task-report@1` allows a node to repeat
   only under another Worker.
6. **Runs.** An epoch is a run when it recorded work and did not refresh the source. A
   `Released` record and an `Abandoned` settlement, which only recovery writes, are not work; a
   lease that wrote `SourceRefreshed` is never a run. A collected Task's execution records are
   gone, so its recorded executions count as work and only its refreshes are told apart.
7. **Readable tokens.** The Markdown writes token counts with thousands separators (`205,295`);
   the JSON keeps exact decimal text.

## Consequences

- A pull request cannot make its own report pass by changing the checker or the workflow; such
  a change takes effect only for the pull requests after it merges, as any base-branch change.
- A model the report cannot vouch for reads `unknown`, so a report can lose an exotic but real
  model name; the plan in the Store still holds it.
- A refreshed node can appear twice in the breakdown, once per Worker, and its rows sum to the
  node's Attempts and tokens.
- `fixtures/task-report/` was regenerated from the renderer; the checker test still runs the
  real renderer's block.
