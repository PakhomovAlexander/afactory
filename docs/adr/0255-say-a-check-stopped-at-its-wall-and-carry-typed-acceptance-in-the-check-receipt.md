# ADR-0255: Say a check stopped at its wall, and carry typed acceptance in the check receipt

**Status:** accepted (2026-10-26). Supersedes in part
[ADR-0205](0205-rerun-a-failed-test-alone-report-the-gate-and-retry-a-failed-gate-on-the-same-candidate.md):
its rejection of a new `af/TaskCheckReceipt` version. Extends
[ADR-0140](0140-run-a-declared-check-through-a-gate-pull-request.md) (a remote result is never
stopped here), [ADR-0207](0207-key-warm-check-layers-by-content-lock-them-split-the-build-deadline-and-seal-without-generated-files.md)
(its phase deadlines are walls) and
[ADR-0212](0212-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md)
(the structured acceptance it left undecided). Keeps
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md) and
[ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md).

## Context

GitHub issue #331 carried two follow-ups of the GA polish campaign that both need a new check
receipt, so they ship together.

**Item 4, check-wall diagnostics.** The per-check wall above one hour exists: `check_wall_ms`
(90 minutes in this repository's `.af/code-policy.toml`) bounds a check Attempt, a
`check_process_wall_ms` bounds each check of it, and under `[warm]` the build and test phases
have their own deadlines (ADR-0207). What the kernel did with a check it killed there was wrong:
the result is `not_run` without an exit code, exactly the shape of a check that never started,
so the gate read `not_run`, the Task ended `finished/gate_failed/preflight_failed` ("a required
check stopped before its tests ran, or never started") and the only trace of the wall was the
runner's reason text. Seven gates of the GA campaign were lost to that confusion: an operator who
read "preflight" fixed a preflight that had passed.

**Item 1, structured acceptance.** A Task's acceptance lived as prose inside `requirements`. The
evaluator had no typed list to judge, nothing recorded which items a verdict covered, and the
check receipt, which every judging Worker already receives, carried nothing of it.

## Considered options

- **A `cause` on `CheckResult@1`.** Rejected: that payload is the Review Kernel's, shared with
  `CheckCompleted@1` events and review gates, and it is deliberately free of wall-clock values so
  its identity is reproducible.
- **A sidecar beside the receipt, as ADR-0205 did for the check report.** Rejected for both
  items: the sidecar is evidence the Workers never receive, and the issue asks for the cause and
  the acceptance *in* the receipt the evaluator reads.
- **A new cause in the Task state vocabulary (`check_wall`).** Rejected: it changes every
  printed document that carries the state (`af/task-inspection@13`, the list, the report and
  more) for one fact the existing `check_failed` states truthfully: the check ran and the kernel
  failed it.
- **Acceptance only inside the receipt.** Rejected: the evaluator would find it only by reading
  another input's payload; the issue asks for its own input.
- **Bump the Task file to `af.task-file/2`.** Rejected: the field is optional and absent on round
  trip, exactly as `context`, `base` and `continue` were added (ADR-0212 §6); a file without it
  keeps its exact revision and plan, and a release that does not know it refuses a file that uses
  it, the direction ADR-0145 asks for.
- **One new receipt version for both items, a new acceptance record and an optional root port
  (chosen).**

## Decision

1. **`af/TaskCheckReceipt@2`** (`schemas/task-check-receipt-v2.json`) is the one version step.
   It keeps `plan_id`, `snapshot_id`, `policy_id`, `outcome` and `checks` (name to result
   artifact) and adds two optional fields, each absent rather than empty:
   - `stopped`: per check the kernel stopped after it started, `{"cause": "wall", "wall_ms",
     "elapsed_ms"}`. `wall` is the closed cause: the check's process ran until the timeout it
     ran under (the per-check wall, or what was left of the check Attempt's wall after
     preparation) or a warm phase deadline (the build's, or the tests' from the build's end),
     and the kernel killed it there. `wall_ms` is that wall; `elapsed_ms` how long the check ran,
     as the host measured it. A check that never started, ended on its own, was cancelled or ran
     remotely has no entry, and a stopped check never passed; the check node's admission and
     every verification refuse a receipt that says otherwise.
   - `acceptance`: the Task's acceptance record ID and its items, verbatim (item 1).

   `af/TaskCheckReceipt@1` is neither written nor read. A Pipeline or Worker contract that
   declares it is refused by name when it is compiled ("af/TaskCheckReceipt@1 is the check
   receipt before ADR-0255 …; declare af/TaskCheckReceipt@2 instead and re-pin the package with
   `af catalog digest`"), so planning and `af catalog test` refuse it, and a recorded receipt of
   that type is refused by name where verification or the evaluator's admission reads it. `af
   catalog digest` still prints such a package's pin: a pin is the package's bytes, and the
   pinned release that runs this repository's `.af/` reads it.
2. **A wall stop reads as one everywhere.** The gate's cause is `check_failed`
   (`check_flaky_then_failed` beside flaky tests), never `preflight_failed`, whose meaning is
   now "a required check stopped at a step before its tests, or never started". `af task show`
   (`why:`), `af task show --checks`, `af task report` (the row's `detail`) and the run's stderr,
   in text and under `--json`, say `check NAME stopped at its 90 min wall after 88 min` and what
   to do: make the check faster, or raise its wall in `.af/code-policy.toml`
   (`check_wall_ms`, `check_process_wall_ms` or `[warm] build_wall_ms`) and plan the Task again;
   the state's `next:` still names `af task run ID --retry-gate` when the kernel would run the
   gate again under the same wall. Only a check that did not start says `never started`. The
   checks document carries the receipt's entry as `stop`, so it is `af/task-checks@3`
   (`schemas/task-checks-v3.json`); `@2` is neither written nor read. Below a minute the
   rendering counts seconds (`2 s`), so a short test wall reads as one.
3. **Structured acceptance.** A Task file's optional `acceptance` is a list of 1 to 32 items,
   `{"id": NAME, "text": TEXT}`, each ID once and the text nonblank and at most 4096 characters.
   It is captured at plan time as one `af/TaskAcceptance@1` (`af.task-acceptance/1`,
   `schemas/task-acceptance-v1.json`) kept in the revision's provenance (and kept across
   selection and refresh, as ADR-0212's records are), so a changed list is a changed plan. The
   check node reads it from the plan's revision and copies it into its receipt; the check node's
   admission and verification refuse a receipt whose acceptance is not exactly the Task's.
4. **The evaluator receives it as its own input and judges it by ID.** The record is bound to the
   selected Pipeline's optional `acceptance` root port (`af/TaskAcceptance@1`, one), which a
   Pipeline wires to its evaluator's optional `acceptance` input. The Pipeline the Task file
   names, or every captured Pipeline accepting its kind, must declare that port alike, or the
   plan is refused before anything is recorded, with the Pipeline named: its evaluator could
   never judge the items. Declaring the port is not enough: the compiled plan must hand the
   bound record to a `verify` evaluator node as that node's own input, or it is refused when it
   is compiled (`declares the acceptance input but hands it to no verify evaluator node`); and
   the Task's kind must run under a domain that judges items by ID, the code domain of the
   implementation profiles, or the plan is refused naming the kind. A Review Task's reviewers
   are read by the review domain, which reduces their findings and never reads an item ID, and
   a Document, report or optimization domain never reads the items either, so their pass would
   verify without them: those kinds are refused too. Both plan-time refusals name the Task
   file and are its typed `task_file_invalid` diagnosis of the `acceptance` field (`af/error@2`:
   `file`, `field`, the item IDs as `value`, `next_action` the `af task plan --file` that plans it
   again), as `limits.wall_ms` is refused. At output admission a passing evaluation over a receipt with
   items is refused unless its evaluator's own `acceptance` input names the receipt's record. No
   binding may supply the port; the Task file's list fills it. An
   evaluation that passes the Task names every item ID as a word of its `reason`
   (`bounds: met`); one that leaves an item out is refused at output admission as a reply that
   broke its output contract, naming the items, so the evaluator answers again within its
   Attempts or the Task does not verify. Verification applies the same rule to what it reads.
   `af/TaskEvaluation@1` does not change: the judgement by ID is a rule on its `reason`.
5. **Without `acceptance`, nothing changes.** No record is written, no port is bound, the receipt
   has no `acceptance`, and the Task's revision and plan are byte for byte what they were.
6. **Migration.** Every Pipeline and Worker package that names `af/TaskCheckReceipt@1` renames it
   `af/TaskCheckReceipt@2` and is re-pinned with `af catalog digest`; nothing else in a package
   changes. The packages this release generates and every fixture package in this repository do.
   This repository's own `.af/` is run by its pinned release, which reads `@1`, so it and the
   fixtures staged to install into it (`fixtures/kernel-experiment`,
   `fixtures/remote-checks/packages`) take the migration in the change that moves the pin to the
   release carrying this decision; the tests that run a copy of them under this `af` take it on
   their copy. Adding the `acceptance` port to the kernel's own Pipelines and evaluators is that
   project's choice, not this decision's. The performance laboratory measures one Task on both
   sides of a pair, so a pair across this decision gives each side the receipt its AF writes:
   `perflab.clicases.side_receipts` reads, from the source commit the plan resolved for each side
   and never from its binary, whether it holds `schemas/task-check-receipt-v2.json`, and the
   task fixture's untimed setup (`fixtures/performance/cli/setup.sh`) lays
   `task-roots/receipt-v1/` (the evaluator package naming `@1`, and its catalog pin) over the
   fixture repository for a side before it. Receipt admission is not relaxed on either side.

## Consequences

- A check killed at its wall is named as one in every rendering, with its wall and how long it
  ran, and its next step is about the wall, not a preflight.
- A Task can state its acceptance as typed items, and its evaluator's pass says which items it
  judged; a pass that skips one does not verify.
- Every Store holding a `@1` receipt and every project package naming `@1` is refused by this
  release (ADR-0145); a Task in flight is finished under the release that started it.
- The receipt is no longer reproducible across runs when a check was stopped, because it carries
  the measured elapsed time; a receipt without a stopped check is as reproducible as before.
- Not decided here: acceptance items bound to a named test whose green or red the gate reports,
  and the per-item verdict as typed evaluator output. Both would change `af/TaskEvaluation@1` or
  the check runner's contract and take their own decision.
