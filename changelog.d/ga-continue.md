- `af task run NEW --from TASK [--round N] [--goal FILE]` plans a new Task from a finished
  reviewed Task: its source is the candidate that review round reviewed, not the base, and its
  implementer receives the round's reviewer findings and the evaluator's verdict as one
  `af/TaskReviewFindings@1` document on the Pipeline's optional `findings` input; nothing is pasted into
  the goal. A Task file says the same with `"continue": {"task": "TASK", "round": N}`. `af task
  show` prints `continues TASK round N`, `--json` carries `continues` in
  `af/task-inspection@13` (`@12` is no longer written or read), and `af task report` names it in
  the row
  ([ADR-0206](docs/adr/0206-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md)).
- The kernel's `kernel/implementer` and `kernel/implementer-long` take the optional `findings`
  input and list each finding by ID as `fixed`, `disputed` or `out of scope`;
  `kernel/implementation-reviewed`, `kernel/implementation-ux` and `kernel/review-light` declare
  it, re-pinned in `.af/task-catalog.toml`.
- Every reviewer, evaluator and verifier gets the tree before the change read-only at `../base`
  and `../diff-subject.json` (`af/diff-subject@1`, each changed path with its digest on each
  side). The base is a sealed candidate's parent or a bound tree's; a Task file may name it with
  `"base": {"commit": "SHA"}`.
- A Task file needs only `task_id`, `kind` and `goal`: `[task.defaults]` of `.af/af.toml` supplies
  `limits`, `pipeline` and `fallback`, and `af task plan` prints what came from it. `context`
  names files the Worker reads first and `.af/rules.md` holds standing rules; both are listed at
  the end of the Worker's requirements with the digest of the bytes the Task captured. `af help
  task-file` describes the shape.
