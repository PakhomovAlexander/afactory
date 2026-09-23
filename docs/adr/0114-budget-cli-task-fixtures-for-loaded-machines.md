# ADR-0114: Budget CLI Task fixtures for loaded machines, never for a fast one

**Status:** accepted (2026-09-22)

## Context

A Task deadline is absolute wall-clock: `af task start` and `af task plan` record
`deadline_unix_ms = now + limits.wall_ms` once, at creation. Every later admission is measured
against it, and `TaskBudget::prepare` refuses a dispatch whose own Attempt wall plus the
still-required verification reserve no longer fits before that instant, with `Task deadline
protects still-required verification`.

The generated-plan integration tests in `crates/af/tests/task_planning.rs` hold one Task open
across a long chain of real subprocesses: several `af` invocations, Git commits, a catalog
export, `catalog test`, `catalog sync`, minisign signing and Python Workers. The fixture tickets
budget 60s in total and reserve 10s–20s of it for verification, so the last Attempt of a valid
resume had to be prepared within roughly 35s–45s of Task creation. That is comfortable when the
tests run alone and is not comfortable under `make check`'s four test threads, where those
subprocesses compete with the rest of the repository suite. Three tests —
`generated_definition_exports_without_approval_and_a_second_developer_reuses_it_without_planning`,
`generated_implementation_embeds_review_and_adds_only_its_admitted_history_constructor` and
`generated_nested_plan_waits_for_exact_approval_then_resumes_with_shared_accounting` — therefore
failed intermittently on a correct refusal: the kernel was protecting a reserve it could no
longer honor, because the test had spent the Task's wall budget on scheduling latency. The same
loaded full gate later exposed the identical assumption in `task_catalog` after its real catalog
sync, Git mutation and offline-resume sequence, and in `task_file`'s native-model account-change
cases after they planned, proved a pre-dispatch refusal, restored the account and resumed.

The defect is in the fixture's resource envelope, not in the enforcement. A Task whose deadline
is consumed by the wall clock must refuse new work, and a reserve that cannot be honored must
not be quietly released.

## Considered options

- **Retry the resume, or accept the deadline error as a non-failure.** Rejected: it hides the
  one signal that distinguishes an exhausted Task from a broken one, and would let a real
  regression in deadline or reserve accounting pass the gate.
- **Shrink the fixtures' `verification` reserve so more of the 60s budget is dispatchable.**
  Rejected: the reserve is the invariant these tests exist to exercise. Making the protected
  allocation smaller to fit an unprotected one inverts the rule.
- **Make the deadline advance only while a Worker runs, or make the clock injectable in the
  production path.** Rejected for this Task: a Task deadline is a promise about wall-clock,
  a scheduler-time deadline would stop bounding real elapsed cost, and a test-controlled clock
  in the CLI is a new production surface no requirement asked for.
- **Serialize the repository test suite, or serialize this test target.** Rejected: it trades
  every other suite's wall time for one file's timing assumption and leaves the same fixture
  one slower machine away from failing again.
- **Give the fixture Tasks a wall budget an order of magnitude above their slowest observed
  sequence (chosen).** The deadline stops being the threshold the test races, while every other
  bound the tests rely on keeps its exact value.

## Decision

CLI Task fixtures that hold one Task across a chain of subprocesses are budgeted for the slowest
machine the gate runs on, not the fastest. `crates/af/tests/task_planning.rs` starts every Task
it creates — including the second developer's consuming Task — with a documented ten-minute wall
(`PLANNING_WALL_MS`), replacing the fixture tickets' 60s. The long catalog sync and offline-resume
test applies the same test-local total (`CATALOG_TASK_WALL_MS`) after copying its shared fixture;
the committed reusable fixture remains unchanged for tests that need its original limits. The
native-model helper similarly uses `NATIVE_MODEL_TASK_WALL_MS` for the Task total while retaining
its 60s verification reserve and every dispatch bound.

The margin is added to the total budget and taken from nothing. Per-Attempt walls (5s in every
fixture Worker manifest and in the fixture code policy), Attempt counts, token budgets and the
verification reserve keep their fixture values, so the Attempt-limit, token-limit and call-limit
refusals these tests cover still bind exactly as before. No production code changes: `prepare`,
`install_graph` and the reserve projection are untouched, and their fail-closed behavior remains
covered by exact-clock unit tests rather than by a loaded machine's latency.

The margin is documented where it is set, with the failure it prevents, so a future maintainer
does not read ten minutes as a slow test and collapse it back toward a sequence's elapsed time.

## Consequences

- The three tests no longer depend on subprocess scheduling for a correct outcome; the
  `task_planning` target passes repeatedly with four test threads.
- A genuinely hung fixture is still bounded: each Attempt keeps its own 5s wall, each Task keeps
  its Attempt count, and the suite is bounded by the gate, not by the Task deadline.
- `crates/review-attempt/tests/task_budget.rs` pins the refusal's exact boundary and wording at
  both a small and a large budget, so widening a fixture's wall can never be mistaken for
  permission to widen what a deadline protects.
- `crates/af/tests/task_planning.rs` pins the margin itself: the admitted Task limits must leave
  every Attempt the Task may start, plus the whole reserve, with the documented slack to spare,
  and the reserve, token and Attempt limits must still be the fixture's own.
- `crates/af/tests/task_catalog.rs` pins that its local override changes only the total Task wall;
  the shared fixture's tokens, Attempt count and complete verification reserve remain exact.
- `crates/af/tests/task_file.rs` pins the native-model fixture's token and Attempt limits and its
  complete verification reserve next to the widened total.
- Other CLI Task suites keep their fixture budgets. If the same symptom appears there, the same
  remedy applies — raise that suite's wall budget with the same documentation, never its reserve.
