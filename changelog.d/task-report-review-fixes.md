- `af task report` and the `PR report` check are harder to fool. The check now runs on
  `pull_request_target` from the base branch's checker, so a pull request can change neither
  the check nor its workflow, and it refuses placeholder, short and long Task rows, empty Task,
  Kind, Pipeline or Outcome cells and a separator row narrower than the header. The report
  copies a Worker's model only when it looks like a model identity and prints `unknown`
  otherwise, so a path-valued model never reaches a description. It encodes `\` and `|` so a
  value stays in its cell, charges each Attempt to the Worker of the plan it ran under (one row
  per Worker after `af task refresh`), counts neither a refresh nor the recovery of a pending
  Attempt as a run or as active time, and writes token counts with thousands separators
  (`205,295`) in Markdown
  ([ADR-0143](docs/adr/0143-judge-the-pull-request-report-with-trusted-code-and-per-attempt-figures.md)).
