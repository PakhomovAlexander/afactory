- `af task report TASK_ID...` summarizes how recorded Tasks ran and what they cost, as one
  Markdown block between `<!-- af-task-report:v1 -->` and `<!-- /af-task-report -->`, ready for
  a pull request description, or with `--json` one `af/task-report@1` document
  (`schemas/task-report-v1.json`). The block leads with one line per pipeline the Tasks ran, its
  steps in dependency order with each step's Worker by Provider kind and model and the gate's
  check names, parallel steps of one role grouped
  (`review (bugs, correctness: codex gpt-6-sol/high)`), and `**unknown pipeline**: not
  retained` once for Tasks whose plan was collected or never made; then one row per Task, each
  a round, with its outcome, its review findings by severity (`6 major, 1 minor`, `none`,
  `unknown` when a round has no complete finding set, `gate failed`, or `—` without a review,
  and how many reviewers failed), tokens with thousands separators
  (`205,295`) and active time, and a `Total:` row of Attempts (failed), tokens and active time;
  then per round its runs, wall time, failed Attempts by reason class and charged tokens, and per
  node the Worker, Attempts, tokens, elapsed time and check results. Findings are counted once
  each as the round's reduce step recorded them, and review rounds come from the Task's log, so
  a round recorded before `af task refresh` still counts. Active time sums the Task's runs, the
  leases in which an Attempt started, so waiting between `af task run`s, a refresh, the recovery
  of a pending Attempt and a resume that only publishes a settled result are not counted; a
  figure the Store does not record is shown as unknown. Each Attempt is charged to the Worker of
  the plan it ran under, so a node that `af task refresh` moved onto another Worker has one row
  per Worker. It only reads the Store and never prints a Provider label, path, credential,
  prompt or Worker output: a Worker's model is copied only when it is a model identity (at most
  96 characters of letters, digits and `._:+-` with at most one `/`, and no `@`, URL, drive
  letter or `..`) and is `unknown` otherwise, and every cell encodes `\` and `|` so a value stays
  in its cell. This repository now requires the block in every pull request description: changes
  are made through af Tasks, and the `PR report` workflow, which runs on `pull_request_target`
  from the base branch's checker so a pull request can change neither the check nor its
  workflow, refuses a description without exactly one well-formed block — a pipeline line, the
  six round-table columns Round, Task, Outcome, Findings, Tokens and Active in order, at least
  one round row and a last `Total:` row; placeholder, short and long rows (with or without their
  outer `|`), a Round, Task or Outcome cell that is empty or holds only an HTML comment, a
  separator row of the wrong width, and a block inside a code fence or indented as code, even
  right after a heading, a thematic break, a fence, an HTML block or a list item, fail —
  apart from Dependabot and `release/` pull requests
  ([ADR-0142](docs/adr/0142-carry-the-af-task-report-in-every-pull-request.md)).
