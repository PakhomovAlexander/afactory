# ADR-0114: Budget CLI Task fixtures for loaded machines, never for a fast one

**Status:** accepted (2026-09-22); amended 2026-10-08
([short fixed walls](#amendment-2026-10-08-short-fixed-walls)),
which applies the same remedy to the fixture processes of tests whose subject is not a deadline,
including a debug-only, raise-only provider probe timeout setting.

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
its 60s verification reserve and every dispatch bound (the amendment below grows that reserve
with its reviewers' Attempt walls).

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
  complete verification reserve next to the widened total; since the amendment below, the
  reserve as the sum of the allowances it protects.
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
  `review-process`'s held-pipe unit tests, its read-failure and cancelled-drain unit tests, and
  its cancellation test.
- **Container fixture writers.** `review-sandbox`'s runtime-fixture writer, for the broken
  runtime and cancelled container tests; the deadline tests keep its 5 s.
- **Source deadlines.** `review-source-task`'s transport and source tests.
- **Provider unit tests in `af`.** The identity-recheck wrapper's Attempt wall and the
  synthetic-login waits of the auth handoff adapter.
- **Readiness, Store and Check walls.** `review-pipeline`'s captured command cancellation,
  controlled check sequence and runtime-store observer, and the Check wall and deadline option
  of the legacy check-order test, whose two successful Checks never meet either.
- **Provider probes in `af` integration tests.** `af`'s provider status, subscription, version
  and Claude usage probes (15 s, 10 s and Claude's 30 s status probe) are production deadlines
  no test parameter reached, so a debug build reads one test setting,
  `AF_TEST_PROVIDER_PROBE_TIMEOUT_MS`, following the precedent of `AF_TEST_CLOCK_QUANTUM_MS` and
  `AF_TEST_FREE_BYTES`. It exists only under `#[cfg(debug_assertions)]`, so a release binary
  never reads it; it can only raise a probe's timeout, and a value at or below the production
  timeout, outside 1 ms to ten minutes, or not a number is ignored; an Attempt deadline still
  bounds every probe; and no Worker or provider CLI receives it, since both start in an isolated
  command environment. The `af` integration tests' shared `af()` helper sets it to the load-safe
  two minutes, and every test helper that clears the environment before starting `af` sets it
  again afterwards: the PTY spawns of `provider_registry` and `tui`, and `provider_auth_handoff`'s
  fixtures, whose completed handoff reaches the fake provider's status probe. That reaches
  `provider_registry::status_keeps_a_default_context_whose_status_probe_failed` and every other
  test that meets a probe without being about its timeout. The native-model `task_file` fixture
  asserts the setting reached neither a probe nor a model call, and the `provider_auth_handoff`
  fake CLIs that it reached neither a probe nor a login.
- **Native-model reviewer Attempt walls.** The `task_file` native-model fixture's two reviewers
  get the load-safe wall as their Attempt wall instead of their manifests' 5 s. Each starts a
  Python fake provider twice, for its identity recheck and its model call, so 5 s was a race.
  Their verification reserve grows with them: it stays exactly the protected allocation it
  encodes, the 45 s Provider admission allowance, both reviewer walls and the 5 s check, now
  290 s instead of 60 s; and the Task wall keeps its ten minutes of slack above that reserve.
  The test that pinned the old 60 s literal now pins that sum, and that the parts are the
  fixture's own.

`changelog.d/test-perf-load-walls.md` lists every test changed and every one left alone.

What stays exact:

- A test whose subject is a timeout or deadline keeps every wall it sets, including the
  fixture-preparation walls inside it: it asserts `TimedOut`, a deadline refusal or an
  elapsed-time bound, and widening its wall would change what it proves. A test that only sets
  a short wall, or passes a deadline option, without asserting one of those is not such a test:
  it gets the load-safe wall. Where a short wall is how a test ends its fixture and the timeout
  was only implied, the test now asserts that timeout. ADR-0124's rule is unchanged: a budget a test asserts is
  protected by running that test alone, never by widening it.
- Every elapsed-time assertion keeps its bound, as do the waits in which a killed process must
  disappear: those bounds are the promptness the test exists to prove. Lease and heartbeat tests
  measure real time and are untouched.
- No production constant changes, and release behaviour is unchanged: `af provider status`
  keeps its 15 s probe in every release binary, and the probe-deadline unit tests of
  `providers::installation` run with it. The debug-only probe setting above is the one test
  parameter added, and it can only raise.
- No reserve shrinks. The only reserve that changes, the native-model `task_file` fixture's,
  grows by exactly what its reviewers' Attempt walls grew, so it still covers the Provider
  admission allowance, both Attempt walls and the check. Token and Attempt limits keep their
  fixture values.
- `optimization_configuration` is not a wall problem: its tests set no wall, and their failures
  are #149, reflink cache materialization.
- `.config/nextest.toml` is unchanged: no retries, and the run-alone list keeps its members.

### Consequences

- A passing test's duration is unchanged. Every widened wall bounds a process that exits on its
  own, and the supervisor's drain grace, not the wall, ends a held pipe, so only a hung fixture
  waits for the two minutes, and only to fail.
- The two-minute value is documented where it is set, with the failure it prevents, so that it
  is not read as slack and collapsed back toward a run's elapsed time.
- A new test with a fixture process uses `LOAD_SAFE_WALL` unless its subject is the deadline.
