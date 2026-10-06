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

Both have to hold up against the reviewer they serve. A check that runs the pull request's own
code can be edited by that pull request. A table row is easy to fake with a placeholder, or with
a cell that holds only an HTML comment and so shows nothing, and a block pasted inside a code
fence shows as code, not as a report. A recorded model is free text in a plan and may be a path
into a home, auth or state directory. A node's Worker can change when `af task refresh` selects
another pipeline, and a refresh first settles the Attempts a dead writer left pending, which is
recovery, not work; a resumed run may likewise only publish a result an earlier run already
selected. A refresh also clears the Task's execution outputs, review rounds included.

What a reviewer of a change made by af asks first is how it was made and how its review went:
which pipelines ran, with which Workers and checks, and what each review round found. A table of
per-Task cost columns answers neither.

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
- **Run the check on `pull_request` and pin the checker by hash in the workflow.** Under
  `pull_request` the workflow file itself comes from the pull request, so the pin can be edited
  with it. Rejected.
- **`pull_request_target` checking out the head to read the description from the tree.** That
  runs pull request code with the base's context. Rejected: the description is in the event
  payload, and nothing from the head is needed.
- **Redact known paths (the state directory, `$HOME`) from a recorded model.** Covers only the
  machine that rendered the report, and only the paths it knows; a denylist of path segments
  still lets a URL or a drive-letter path through. Rejected for an allow-list of what a model
  identity looks like.
- **A `mixed` Worker for a node bound to several Workers.** Correct, but loses each Worker's
  share. Rejected for one row per node and Worker binding.
- **Count every lease that recorded work as a run.** Counts a resume that only published a
  result an earlier run selected, or only finished the Task, as a run, and its span as active
  time, though it began no work (issue #191). Rejected: a run is a lease in which an Attempt
  started.
- **One summary table of per-Task cost columns (Task, Kind, Pipeline, Outcome, Rounds,
  Attempts, Tokens, Active time, Wall time) and a totals line.** The first layout. It repeats
  the pipeline name on every row and says nothing about what the pipeline does or what the
  review found. Rejected for pipeline lines first and one row per round with its findings.
- **Count findings from each reviewer's result.** Two reviewers that report the same problem
  would count it twice, and a reviewer's raw result is not what the round decided. Rejected for
  the reduce step's `FindingSet@1`, which records each finding once.
- **Count review rounds from the Task's current execution outputs.** `af task refresh` and a
  review handoff clear those outputs, so a round recorded before them disappears from the count
  (issue #191). Rejected for the rounds the Task log's execution records published.
- **A command that prints the block, and a trusted check that requires it.** Chosen.

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
3. **What it reports.** Each reported Task is one **round**, numbered by its 1-based position in
   the order given. Per Task: round; ID; kind; pipeline as `name@version`; outcome as `af task
   show` states it; review rounds; review findings; runs; Attempts, failed Attempts by reason
   class and the tokens charged to them; chargeable tokens; wall time; active time; and per node
   the role, the Worker as Provider kind, model and effort (`codex gpt-6-sol/high`, or
   `command`), Attempts, tokens, elapsed time and the gate's check results with their spans.
   Totals sum Tasks, rounds, Attempts (failed), tokens and active time. Beside the Tasks, the
   report lists each distinct **pipeline** their current plans run, in first-use order.
   - A **pipeline** is read from the Task's current plan: its root pipeline's name and version,
     and its **steps**, every node of the compiled graph that runs a Worker (Worker, Verify,
     FixVerify, reviewer and scatter nodes) and every check node, which is the **gate**. Kernel
     bookkeeping nodes (seal, bind, reduce, accept, select, root inputs) and Provider admission
     nodes are left out. A step's role is its Worker slot's role (`implement`, `review`,
     `evaluate`, …), or `gate`; its Worker is the plan's binding of its slot, named under the
     model rule below; a gate lists its checks, local and remote, in name order. Steps are in
     dependency order: a node's stage is its depth in the compiled graph, every node it reads or
     is conditioned on coming first, and nodes of one stage with one role, Worker and check list
     are one step, so parallel reviewers bound to one Worker are one step with several nodes.
     Two Tasks whose plans bind one `name@version` differently list it twice.
   - **Review rounds** are every `af/TaskReviewRound@1` the Task's execution records published,
     read from the Task log, each counted once. A source refresh and a review handoff clear the
     Task's execution outputs but not its log, so a round recorded before either still counts.
   - **Findings** come from the review rounds' reduce steps. A complete round's
     `af/TaskReviewRound@1` names the `review.kernel/FindingSet@1` its reduce step wrote; that set
     also carries earlier rounds' findings, so a round contributes the entries whose
     `last_seen_round` is the set's own round. Each finding is counted once, by `finding_id`,
     at the `severity` the last round that saw it recorded, and the counts are by severity:
     blocker, major, minor. The review **ran** when a reviewer Attempt began or a round wrote
     its findings; the **gate failed** when a check result the Task recorded failed and the
     review did not run; a **failed reviewer** is a reviewer node with at least one failed
     Attempt. A reviewer node is a Review frontend's reviewer or scatter node, or a Worker node
     whose result a review reduce step reads. A Task whose plan has no review step, records no
     round and ran no reviewer has no findings, and neither has a collected Task.
   - A **run** is a writer lease in which an Attempt of the Task started. Every Task command
     holds its own lease, at its own epoch: `af task start --execute` is the first run and every
     `af task run` that resumed it with new work is another, while approving, refreshing or
     delivering a Task is not. An epoch is a run when it recorded a `Started` execution record and
     did not refresh the source. Planning, settling, publishing an already selected result,
     finishing the Task, and the `Released` record and `Abandoned` settlement that recovery
     writes when a command takes its lease and settles or releases an earlier writer's pending
     Attempts, are not new work: a resume after every Attempt finished, which only publishes and
     finishes, is not a run, and a lease that wrote `SourceRefreshed` never is. A collected
     Task's execution records are gone, so each of its recorded executions counts as work and
     only its refreshes are told apart.
   - Each started Attempt's **role and Worker** are resolved from the plan that Attempt ran
     under, never from the node's first Attempt. The breakdown has one row per node, role and
     Worker: a node keeps one row while every plan binds it to the same Worker, and appears once
     per Worker otherwise, its rows summing to the node's Attempts and tokens; `af/task-report@1`
     lets a node repeat only under another Worker. `af task refresh` restores the Task's captured
     catalog and keeps its requested pipeline, so it rebinds a node only when the requested
     pipeline no longer fits and the Task's fallback `select` chooses another pipeline with a
     node of the same name — for example when the preferred pipeline's Provider is no longer
     configured on the machine. That path is reachable, and
     `crates/af/tests/it/task_report_command.rs` runs it end to end: a model Worker's Attempt
     abandoned under the first plan and a command Worker's Attempt under the refreshed one are
     charged to their own Workers.
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
     outcome, tokens and event times; its Attempts, nodes, rounds, findings and pipeline are
     unknown.
4. **The block.** The default output is Markdown between the exact lines
   `<!-- af-task-report:v1 -->` and `<!-- /af-task-report -->`, in this order:
   1. the heading `### af task report`;
   2. one line per pipeline, each its own paragraph: `**name@version**:` and its steps, stages
      joined by ` → ` and the steps of one stage by ` + `. A step reads as its role and, in
      parentheses, its Worker or a gate's check names, led by its node names when it has several
      nodes or shares its stage, as the renderer's test of five rounds prints:
      `**afactory/implementation-reviewed@1.0.0**: implement (codex gpt-6-sol/high) → gate
      (clippy, fmt, test) → review (bugs, correctness: codex gpt-6-sol/high) → evaluate (claude
      claude-opus-5-5/high)`;
   3. one table whose header is exactly the six columns Round, Task, Outcome, Findings, Tokens
      and Active, in that order, with one row per Task in the order given, and a last row whose
      Task cell is `Total: N Attempts` (with `(M failed)` when any failed), whose Tokens and
      Active cells are the totals and whose other cells are empty. The Findings cell reads
      `6 major, 1 minor` (the nonzero counts, blocker, major, minor), `none` when the review ran
      and found nothing, `gate failed` when the gate failed and the review did not run, `not
      run` when the plan reviews but no reviewer has begun, `—` for a Task without a review and
      `unknown` for a collected one, followed by `; N reviewer(s) failed` when a reviewer's
      Attempt failed;
   4. per round a collapsed `<details>` element whose summary is `Round N · TASK_ID` with its
      runs, wall time and failed Attempts by reason class, holding the per-node table (Node,
      Role, Worker, Attempts, Tokens, Elapsed, Checks).

   Token counts carry thousands separators (`205,295`) in the Markdown only. It renders on
   GitHub and reads as plain text in a terminal. The `v1` in the marker is the contract's
   version: once released, a change to the markers or the columns is a new version, and the
   check accepts the versions it knows. Before the first release the v1 layout changed in place
   from the first layout above to this one.
5. **The document.** `--json` prints one `af/task-report@1` document, `schemas/task-report-v1.json`,
   with the same figures: tokens as exact decimal text, times in milliseconds; a `pipelines`
   array (name, version, and steps with stage, role, nodes, Worker kind and model, and check
   names); and per Task its `round` and its `findings` (`blocker`, `major`, `minor`,
   `review_ran`, `gate_failed`, `failed_reviewers`).
6. **Privacy.** The report names a Provider only by kind, model and effort. It never contains a
   Provider registry ID or label, a principal, an auth directory, a state directory, a home path,
   a credential, a prompt or Worker output. Every object of the document is closed, so the schema
   refuses any of them.
   - **Cells.** Every value in the Markdown is sanitized display text that can neither end a
     table cell nor open markup: a backslash and a `|` are written as the character references
     `&#92;` and `&#124;`, before `<` and `>` are encoded, so no recorded value can end its cell
     in a table, or open markup in a pipeline line or the `<summary>` line.
   - **Model identity.** A recorded model may be a path, a URL or an account, so the report
     copies it only when it passes one allow-list rule,
     `review_core::task::task_report::is_model_identity`: at most 96 characters; only
     `A-Z a-z 0-9 ._:+-` and at most one `/`, so no `@` and no email address; the first
     character, and the first character after the `/`, alphanumeric; no `..`; no `:` directly
     before the `/`, so no `://`; no drive-letter prefix such as `C:`; and, as a further
     refusal, neither side of the `/` the name of a home, auth or state directory (`home`,
     `users`, `root`, `tmp`, `var`, `private`, `state`, `auth`, `auth.*`, in any case). Anything
     else is `unknown` in both forms. The
     schema's `model` pattern states the same rule, and the document's validation applies it;
     the Rust rule's unit test, the renderer's test of both forms, and a schema parity test
     that holds the pattern to the Rust rule all judge one table, `TASK_REPORT_MODEL_CASES`.
     It accepts `gpt-6-sol/high`, `claude-opus-5-5`, `gpt-5.3-codex-spark` and
     `us.anthropic.claude-opus-5-5-v1:0`, and refuses `file:///etc/passwd`, `C:/secrets/key`,
     `C:\key`, `/etc/x`, `~/x`, `a//b`, `a/b/c`, `../x`, `alice@example.com`, `model@host` and
     the empty value.
7. **The check.** `.github/workflows/pr-report.yml` runs on `pull_request_target` (`opened`,
   `edited`, `synchronize`, `reopened`, `ready_for_review`) with `contents: read`. It runs only
   trusted code: `pull_request_target` runs the workflow as the base branch holds it, the job
   checks out the base branch, never the pull request's head or merge ref, and runs the base's
   `scripts/check-pr-report.py` on the event payload GitHub hands the job
   (`$GITHUB_EVENT_PATH`): no API call, no token, and no code from the pull request. Its
   concurrency group is the pull request number, since `github.ref` is the shared base branch
   under this event. A pull request can therefore change neither the workflow nor the checker
   that judges it; a change to either applies from the pull requests after it merges. A pull
   request opened by `dependabot[bot]`, or from a `release/` branch, is exempt — neither is made
   by an af Task.
   Every other pull request passes only when its description holds exactly one well-formed
   block: both markers in order; at least one pipeline line (`**name@version**:` followed by
   its steps) before the table; the table header equal to the six v1 columns, exactly and in
   order; a separator row of that width; at least one round row; and a last row whose Task cell
   starts with `Total:`. The checker splits a row as GitHub does (a backslash escapes the next
   character), and every non-blank line after the separator, up to the first blank line, is a
   row whether or not it starts or ends with `|`, since GitHub renders both forms as rows. Every
   row has exactly six cells, and every row before the `Total:` row is a round row with nonempty
   Round, Task and Outcome cells. A cell is nonempty only when it has visible content once its
   HTML comments, closed or left open, are removed, so `<!--x-->` fills no cell (issue #191).
   A marker counts only outside code (issue #191): a marker line inside a fenced code block
   (opened by ```` ``` ```` or `~~~` of any length, with any info string, and closed by a fence
   of the same character at least as long, or left open to the end) or in an indented code
   block (four columns of indent after a blank line, an HTML comment block not being one) is
   text, and a description whose only block is shown as code fails with a message saying the
   block is inside a code block. Each missing or malformed part fails with its own message — a
   missing pipeline line, a missing column, an extra column and reordered columns each have
   theirs, as do a one-cell placeholder, a short or long row (named by its line), an empty
   required cell, a missing or misplaced `Total:` row, a separator of the wrong width and a
   block inside code — and every failure says how to produce the block with `af task report`.
   `scripts/test-check-pr-report.py`, run by `make check`, accepts the checked-in block the real
   renderer printed for a test Store (`fixtures/task-report/`), and the renderer's own tests
   require it still prints that block, so the checker and the renderer cannot drift apart.

## Consequences

- A reviewer sees, in the description, which pipelines produced a change and how — each step's
  Worker and the gate's checks — then each round's outcome, what its review found by severity,
  and at what cost in tokens and active time, and per round its Attempts and failures: the
  kernel's figures, not an author's recollection.
- The check proves the block is there and well formed, not that its figures match a Store: CI
  cannot reach one. Pasting an edited block remains possible and remains a review matter, like
  any other false statement in a description.
- A pull request cannot make its own report pass by changing the checker or the workflow; such
  a change takes effect only for the pull requests after it merges, as any base-branch change.
- A change made outside af Tasks cannot be merged through the normal path without saying so; the
  check fails until the description carries a report. Dependabot and release pull requests stay
  automatic.
- A model the report cannot vouch for reads `unknown`, so a report can lose an exotic but real
  model name, one longer than 96 characters or with two `/`s; the plan in the Store still holds
  it.
- A node refreshed onto another Worker appears once per Worker in the breakdown. A pipeline
  line shows the Task's current plan, so a pipeline a Task left by a refresh is not listed.
- Findings are the reduce step's, so two reviewers that report one problem count once, and a
  finding an earlier round recorded counts only in a round that saw it again. A round that did
  not complete wrote no finding set: its reviewers' raw results are not counted.
- A run that only finishes what an earlier run settled is not a run, so a Task resumed only to
  publish reports one run fewer and less active time than the leases it took.
- Once released, adding a column, renaming a marker or changing the `Total:` row is a contract
  change: a new marker version, schema and checker together, never an edit that old
  descriptions fail. This layout replaced the first one in place only because neither had been
  released.
- The command reads and never writes, so it is safe on a Store a running Task holds; a figure a
  live Task has not recorded yet is reported as what the Store holds at that moment.
