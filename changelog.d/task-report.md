- `af task report TASK_ID...` summarizes what recorded Tasks cost and how they ran: pipeline,
  outcome, review rounds, runs, Attempts with each failure's reason class and charged tokens,
  chargeable tokens, active and wall time, and per node the Worker by Provider kind and model,
  Attempts, tokens, elapsed time and check results. It prints one Markdown block between
  `<!-- af-task-report:v1 -->` and `<!-- /af-task-report -->`, ready for a pull request
  description, with token counts written with thousands separators (`205,295`), or with `--json`
  one `af/task-report@1` document (`schemas/task-report-v1.json`). Active time sums the Task's
  runs, so waiting between `af task run`s is not counted, and neither a refresh nor the recovery
  of a pending Attempt counts as a run or as active time; a figure the Store does not record is
  shown as unknown. Each Attempt is charged to the Worker of the plan it ran under, so a node
  that `af task refresh` moved onto another Worker has one row per Worker. It only reads the
  Store and never prints a Provider label, path, credential, prompt or Worker output: a
  Worker's model is copied only when it is a model identity (at most 96 characters of letters,
  digits and `._:+-` with at most one `/`, and no `@`, URL, drive letter or `..`) and is `unknown`
  otherwise, and every cell encodes `\` and `|` so a value stays in its cell. This repository
  now requires the block in every pull request description: changes are made through af Tasks,
  and the `PR report` workflow, which runs on `pull_request_target` from the base branch's
  checker so a pull request can change neither the check nor its workflow, refuses a
  description without exactly one well-formed block — the summary header must be the nine v1
  columns in order, and placeholder, short and long Task rows (with or without their outer
  `|`), empty Task, Kind, Pipeline or Outcome cells and a separator row of the wrong width
  fail — apart from Dependabot and `release/` pull requests
  ([ADR-0142](docs/adr/0142-carry-the-af-task-report-in-every-pull-request.md)).
