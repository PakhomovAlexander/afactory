# ADR-0206: Continue a Task from its reviewed candidate, and give judging Workers the base

**Status:** accepted (2026-10-23). Extends
[ADR-0117](0117-bind-task-inputs-to-recorded-task-outputs.md) (a continuation binds its source as
a bound derived tree is bound) and [ADR-0134](0134-bind-any-declared-root-port-to-recorded-task-outputs.md)
(the findings port is a declared root input, and never decides the Pipeline). Keeps
[ADR-0028](0028-prioritize-wise-token-use-and-minimum-worker-context.md) (exact, minimal Worker
context), [ADR-0033](0033-configured-workers-authorize-declared-input-delivery.md) and
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md).

## Context

Most implementation Tasks end `changes_requested`: 71 of 134 on one machine, 266 of 396 on
another. Every follow-up was a new Task on the same base whose goal carried the previous candidate
and the findings as prose, a 25 to 32 KB prefix a script generated in five variants, or, for an
agent without a worktree, the candidate's patch compressed into the 64 KiB requirements payload,
which the implementer then discarded. Twelve Tasks and 7.4M tokens went into one fix. The
implementer had no typed findings input, and verification Workers had no base tree: `base` was
not bindable, and the verification Pipelines declare only history, requirements and source, so
a before/after claim could not be checked and an agent base64-encoded the base into the goal
(issues #137, #245, #246, #247). Task files repeated the same limits and Pipeline everywhere, and
the standing rules were pasted into every goal.

## Decision

1. **Continue from the reviewed candidate.** `af task run NEW --from TASK [--round N] [--goal
   FILE]`, or a Task file's `"continue": {"task": "TASK", "round": N}`, plans a new Task from
   TASK's own captured Task file. TASK must be finished in the same Store and must have recorded a
   review round; the round is the last one unless named. The candidate that round reviewed is the
   new Task's `source`, bound as an `inputs.source` binding of a derived tree is: re-rooted over
   the identical Manifest with an `af.task-source-origin/2` origin that names TASK. The goal is
   TASK's goal unless `--goal` replaces it. Without `--execute` the command plans and the plan
   waits for `--confirm-plan`, as every plan does. `continue` excludes `inputs.source` and
   `inputs.findings`, and `findings` joins the root ports no binding may name.
2. **Findings are a typed artifact on a declared port.** The round's selected reviewer results
   and the evaluator's last verdict, when it did not pass, become one `af/TaskReviewFindings@1`
   (`af.task-review-findings/1`, `schemas/task-review-findings-v1.json`): what it continues (Task, revision,
   result, round, round artifact, candidate Snapshot), each finding under the ID the implementer
   answers by (`bugs-1`, `correctness-2`, `evaluator`) with who said it, the artifact it came from,
   its severity, place, title, body and fix, and the required reviewers that recorded nothing. It
   is input data on the selected Pipeline's optional `findings` root port, outside the 64 KiB
   requirements bound; no acceptance, verification or budget crosses the Task boundary through
   it. The selected Pipeline, or every Pipeline accepting the kind, must declare the port alike,
   or the plan is refused. The kernel's `kernel/implementation-reviewed` and
   `kernel/implementation-ux` declare it and wire it to their implementer, whose contract takes it
   optionally; `kernel/review-light` declares it for a Pipeline that calls it. The implementer's
   instructions say to resolve or dispute each finding and to list each by ID as `fixed`,
   `disputed` or `out of scope`.
3. **Lineage is recorded and shown.** The findings artifact is kept in the revision's provenance
   (the Store's provenance list keeps it across selection and refresh), so every view reads the
   lineage from the revision: `af task show` prints `continues TASK round N`, the inspection
   document carries `continues`, and `af task report` names it in the row. The inspection
   document's shape changed, so it is `af/task-inspection@13`; `@12` is neither written nor read.
4. **Judging Workers get the base.** A Worker that judges its source without writing it (a
   reviewer, an evaluator, a verifier) gets the tree before the change materialized read-only as
   `base/`, the sibling of its `tree/`, so it reads `../base`, and `../diff-subject.json`
   (`af/diff-subject@1`, `schemas/diff-subject-v1.json`): both Snapshots and every path whose
   content or mode differs, with the content digest on each side. The base is the source
   Snapshot's recorded base, a sealed candidate's parent, or, for a bound tree, the parent of the
   tree it was bound from; a Task file may name it with `"base": {"commit": "SHA"}`, captured at
   plan time as `af/TaskBase@1` (`schemas/task-base-v1.json`) and kept in the provenance. Neither
   file is under the Worker's tree, so neither is sealed back or counts as an edit, and both go
   with the sandbox. Neither is a port: no Worker contract changes and no Pipeline has to bind a
   `base`, which stays unbindable. A writer gets no base; it changes the tree itself.
5. **Short Task files.** Only `task_id`, `kind` and `goal` are required, and an omitted `schema`
   reads as `af.task-file/1`. `[task.defaults]` of `.af/af.toml`, read from the Authority
   Snapshot, supplies `limits`, `pipeline` and `fallback`; a field the Task file names wins whole,
   and the default Pipeline applies only to a kind it accepts. `af task plan` prints on stderr
   what came from the defaults, and the revision records the effective values. `context` names
   files of the source tree, with an optional `#section`, and `.af/rules.md` of the Authority
   Snapshot holds standing rules (a declared `.af/` entry). Both are recorded by content digest
   as one `af/TaskReadFirst@1` (`schemas/task-read-first-v1.json`) in the provenance and listed,
   rules first, at the end of the Worker's requirements text, so a changed file is a changed plan.
6. **The Task file stays `af.task-file/1`.** Its new fields are optional and absent on round
   trip, so a file without them keeps its exact revision and plan, as the `inputs` table did; a
   release that does not know them refuses a file that uses them, which is the direction
   ADR-0145 asks for. Every record this decision adds is a new versioned type with its schema and
   parity test.

## Consequences

- A follow-up Task costs one command and no prose: the candidate is a Snapshot, the findings an
  artifact, and the implementer starts from the tree the reviewers saw.
- Every judging Attempt on a candidate materializes one more tree. It is read-only and removed
  with the sandbox; a large repository pays its materialization per Attempt.
- A Worker in a container sees only its bind-mounted tree, so `../base` is a local-sandbox
  facility; a Worker must not assume it exists. The judging Workers' own instructions are
  project packages this decision does not edit; until they name `../base`, the diff subject's
  `base` field says where the tree is.
- Not decided here: structured acceptance (`{"test": …, "claim": …}` entries reported green or
  red in the gate receipt) and the reuse of a warm template for the base tree. Both change other
  contracts (the check receipt, the warm layers) and take their own decision.
