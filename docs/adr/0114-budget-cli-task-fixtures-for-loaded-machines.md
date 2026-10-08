# ADR-0114: Budget CLI Task fixtures for loaded machines, never for a fast one

**Status:** accepted (2026-09-22); amended 2026-10-08
([short fixed walls](#amendment-2026-10-08-short-fixed-walls)),
which applies the same remedy to the fixture processes of tests whose subject is not a deadline.

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

## Amendment, 2026-10-08: short fixed walls

Issue #206, for tests whose subject is not the deadline. Under a loaded gate (seven nextest
threads, one-minute load 7-28) tests that give a fixture process a short fixed real-time wall
failed although they pass alone: a fake Codex or
Claude provider given a 5 s total wall, a model-runner or supervisor fixture given 1-10 s, a
Jira source fixture given a 5 s deadline, or a cancellation test that waits 2-5 s for its
fixture to become ready before it cancels. Starting a shell or Python on such a machine took
about 5 s, so the test raced scheduling and reported a timeout its subject never asked about.
The defect is the one above in a smaller envelope: the wall belongs to the fixture, not to the
behaviour under test.

### Decision

A test whose subject is not a timeout or deadline gives every fixture process a named,
documented load-safe wall, `LOAD_SAFE_WALL`, two minutes, with the reason written beside it. It
replaces the total wall the test passes to a provider adapter, a `ModelRunner`, the supervisor,
or a source, and the window in which it waits for a fixture to become ready, or for the shared
Store, before it acts. The families it names:

- **Fake-provider walls.** `review-runner-codex` and `review-runner-claude`: the auth-failure,
  capture, final-message, worker, model-usage and structured-reply tests, and the shared native
  cancellation fixture. The three runner test binaries share one definition,
  `crates/review-runner/tests/it/support/load_safe_wall.rs`, as they already share that
  fixture.
- **Model-runner and supervisor walls.** `review-runner`'s `model_supervision` tests and
  `review-process`'s held-pipe unit tests and cancellation test.
- **Source deadlines.** `review-source-task`'s transport and source tests.
- **Provider unit tests in `af`.** The identity-recheck wrapper's Attempt wall and the
  synthetic-login waits of the auth handoff adapter.
- **Readiness and Store waits.** `review-pipeline`'s captured command cancellation, controlled
  check sequence and runtime-store observer.

`changelog.d/test-perf-load-walls.md` lists every test changed and every one left alone.

What stays exact:

- A test whose subject is a timeout or deadline keeps every wall it sets, including the
  fixture-preparation walls inside it: it asserts `TimedOut` or a deadline refusal, and widening
  its wall would change what it proves. ADR-0124's rule is unchanged: a budget a test asserts is
  protected by running that test alone, never by widening it.
- Every elapsed-time assertion keeps its bound, as do the waits in which a killed process must
  disappear: those bounds are the promptness the test exists to prove. Lease and heartbeat tests
  measure real time and are untouched.
- No production constant changes. A production deadline that only a new test-only parameter
  could raise is not raised: `af provider status` keeps its 15 s probe, and the test that hits
  it is reported as not fixed.
- No fixture budget or reserve changes. The native-model CLI fixture of `task_file` already has
  its ten-minute Task wall (above); its reviewers' 5 s Attempt walls are verifier allowances
  that, with the 45 s Provider admission allowance and the 5 s check, exactly fill the pinned
  60 s verification reserve, so they cannot grow without widening that reserve, and are left
  as they are.
- `.config/nextest.toml` is unchanged: no retries, and the run-alone list keeps its members.

### Consequences

- A passing test's duration is unchanged. Every widened wall bounds a process that exits on its
  own, and the supervisor's drain grace, not the wall, ends a held pipe, so only a hung fixture
  waits for the two minutes, and only to fail.
- The two-minute value is documented where it is set, with the failure it prevents, so that it
  is not read as slack and collapsed back toward a run's elapsed time.
- A new test with a fixture process uses `LOAD_SAFE_WALL` unless its subject is the deadline.
