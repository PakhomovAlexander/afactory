- `af task report TASK_ID...` summarizes what recorded Tasks cost and how they ran: pipeline,
  outcome, review rounds, runs, Attempts with each failure's reason class and charged tokens,
  chargeable tokens, active and wall time, and per node the Worker by Provider kind and model,
  Attempts, tokens, elapsed time and check results. It prints one Markdown block between
  `<!-- af-task-report:v1 -->` and `<!-- /af-task-report -->`, ready for a pull request
  description, or with `--json` one `af/task-report@1` document (`schemas/task-report-v1.json`).
  Active time sums the Task's runs, so waiting between `af task run`s is not counted; a figure the
  Store does not record is shown as unknown. It only reads the Store and never prints a Provider
  label, path, credential, prompt or Worker output. This repository now requires the block in
  every pull request description: changes are made through af Tasks, and the `PR report` workflow
  refuses a description without one, apart from Dependabot and `release/` pull requests
  ([ADR-0142](docs/adr/0142-carry-the-af-task-report-in-every-pull-request.md)).
