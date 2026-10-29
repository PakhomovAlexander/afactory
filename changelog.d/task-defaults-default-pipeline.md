- This repository's `.af/af.toml` `[task.defaults]` names `default/implementation`, the default
  pipeline every Afactory change runs through, instead of `kernel/implementation-reviewed`: a
  Task file of this repository that names no pipeline plans on it, with limits sized for it
  (tokens 8000000, max_attempts 16, wall_ms 43200000; verification tokens 2000000, attempts 9,
  wall_ms 18000000), the limits of the default-pipeline example in `docs/tasks.md`, whose
  verification Attempts rise from 8 to 9: the planner reserves one verification Attempt for each
  judging node (checks, bugs, correctness, uix, evaluator) and one for each of the four Provider
  admissions it protects, and 8 refused every plan of `default/implementation`
  ([ADR-0253](docs/adr/0253-the-default-task-pipeline-lives-in-the-catalog.md)).
