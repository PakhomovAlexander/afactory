# ADR-0205: Rerun a failed test alone, report the gate, and retry a failed gate on the same candidate

**Status:** accepted (2026-10-23). Supersedes in part
[ADR-0124](0124-run-tests-in-parallel-processes-and-link-them-once.md): the consequence of its
`retries = 0` that a test failing once in the parallel run always fails the gate.
Superseded in part by
[ADR-0255](0255-say-a-check-stopped-at-its-wall-and-carry-typed-acceptance-in-the-check-receipt.md):
its rejection of a new `af/TaskCheckReceipt` version; the receipt is `af/TaskCheckReceipt@2`.

## Context

Across 414 recorded runs of af Tasks on afactory itself, 111 ended at the gate. Of the 120 saved
failure sets, 67 held only af's own load-flaky tests: the starter repair, the heavy-review and
interrupted-repair tests, one source-capture test, the concurrent pipes test and a TUI test, all
of which pass on the same tree when the host is quiet. Sixty of those runs ended after a whole
implementer Attempt (22M tokens whose candidate no reviewer read): a failed gate ends the Task,
the sealed candidate cannot be checked again, and every launcher script around af plans the
same work again under a new Task, up to eight times.

Three more things made a failed gate cost more than it had to. Nothing recorded how loaded the
host was when a check failed, or with how many test threads, so a load flake and a real
regression read alike. A gate that stopped at a project preflight (a stale protected-harness
pin) read as a failed test run. And two gates ran 15 minutes cold with `cargo_target cold
toolchain_unresolved`, after 1.1M implementer tokens, because a toolchain the Snapshot pins was
not installed in the kernel's rustup home and nothing asked before the implementer ran.

On a Linux host with Rust coreutils and umask 0077 every gate failed until fixtures were
patched: seed fixtures assumed umask 022, a protected-prefix test assumed `/bin/ls` resolves
below `/bin/`, a real-build test needed `cc` on the sanitized `PATH`, and the harness pin
generator captured the author sandbox's `$HOME/.cargo/.package-cache`.

## Considered options

- **nextest `retries`.** Rejected: a retry runs beside the same parallel load that failed the
  test, so it does not tell a load flake from a defect, and nextest reports a passing retry as a
  pass, hiding the flake.
- **Lower `TEST_THREADS`.** Rejected by ADR-0124 for everyone's sake, and it keeps nothing
  from the external load a Task gate shares the host with.
- **Re-plan the Task with the same candidate.** Rejected: a new Task is a new plan, a new
  admission and new Provider probes; the candidate a reviewer never read is still discarded.
- **Make a failed check not end the Task.** Rejected here: the outcome vocabulary is another
  package's decision, and an operator, not the kernel, decides that a gate failure was the
  host's.
- **A new version of `af/TaskCheckReceipt@1` carrying the new fields.** Rejected: every
  committed pipeline in every consuming repository names `af/TaskCheckReceipt@1` as a port type;
  the facts are evidence beside the receipt, not part of what reviewers receive.
- **Rerun failed tests alone once, report the gate in its own artifact, and let an operator
  retry a failed gate on the same candidate in its own Task log event (chosen).**

## Decision

1. **The isolation rerun.** After the parallel nextest run of a gate (`scripts/nextest-gate.py`),
   a run that failed because tests failed (nextest's exit status 100) runs each failed test
   again, one at a time, with no other test alive (`--test-threads 1`, one exact filter each).
   A test that passes alone is printed `flaky:` and does not fail the step; one that fails both
   times fails it as before, with nextest's status. The rerun is bounded: more than 25 failed
   tests are not rerun at all, and reruns stop after 15 minutes; either way the step fails with
   the whole list. Any other failure (a build that failed) is never rerun. `--repeat N` runs a
   selection N whole times in a row and never reruns anything (`make test-repeat`), because it is
   how a flake is proved fixed. A flake reported by the rerun is still a defect to fix in the
   test or the kernel (ADR-0124); the rerun only stops it from discarding a candidate.
2. **The check report, `af/TaskCheckReport@1`.** Every local check af runs receives
   `AF_CHECK_REPORT`, a file below its runtime directory, and may append `af.check-report/1`
   lines to it: a `step` line per gate step (its label, its stage — `preflight`, `lint`,
   `build`, `tests` or `release` — its exit status, its command and, where the gate names one,
   the command that fixes it), and one `tests` line (test threads, the failed tests, the flaky
   tests with both outcomes, and what the rerun did). After the check, af records one
   `af/TaskCheckReport@1` per check node Attempt beside its receipt, referencing it: per started
   check, the host's 1-minute load average at the check's start and end and the CPUs available
   to af, measured by af, and the check's account, `absent`, `admitted` or `refused` with the
   reason. The account never decides the check's status; its exit code does, as the receipt
   records. `af task show` prints, per check node, where a check stopped (stage, step and fix),
   the flaky tests and the tests that failed twice each on their own line, and the host's load
   for a check that did not pass or had flaky tests. This repository's `Makefile` tags every
   step with its stage, and its protected-harness preflight with its fix.
3. **`af task run ID --retry-gate`.** A Task that finished unsatisfied with a check receipt that
   did not pass, and after which no paid node started an Attempt (no reviewer or evaluator ran,
   so the last failure was the gate's), can be reopened at that gate: the Store appends
   `TaskGateRetried@1`, its own event of the Task's log beside `TaskTransition@5` (as
   `TaskExecutionStarted@1` is, ADR-0153), naming the lease, the finished result and each failed
   gate's node and published output. It clears that output and every output and invocation after
   it, retires their selected Attempts, which keep their settlement and charge, grants the gate
   node one more Attempt than its compiled allowance, and runs again; the gate's own invocation
   stays, because its sealed candidate does not change. Nothing is re-planned or re-admitted, no
   new Task is made and no Provider is probed. The Store refuses it when the plan's approval or
   deadline has passed, or the Task's or call's Attempt, token or wall limits leave no room for
   the new Attempt, with that reason, so a reopened Task can always run. It refuses a Task that
   was verified or delivered, whose last failure was not a gate, whose gate is followed by owned
   children, an experiment or an Integration, or that runs a captured Campaign Review Round. A
   Task that ended at a failed gate is `finished/gate_failed/CAUSE` in the one Task state every
   rendering prints (`af help task-states`), and its one `next:` line is `af task run ID
   --retry-gate` exactly when these rules would reopen it, otherwise `af task show ID --checks`:
   the state never names a command the kernel refuses for the reasons above. The check report
   lines and `gate retried: NODE ran again on the same candidate` are text beside that state, and
   `af task show --json` (`af/task-inspection@12`) carries the same facts under `gate`
   (`check_reports`, `retries`). A `--retry-gate` run is a run of the Task for `af task report`,
   as it starts a check Attempt; an `af task refresh` is not.
4. **Toolchains are resolved at admission.** Before a plan is admitted, and so before any
   implementer Attempt, af resolves on the Task's captured source Snapshot the toolchain a field
   names for its local checks: under `[warm]` build caches, the toolchain the Snapshot's
   `rust-toolchain.toml` pins, probed exactly as the check's warm layer probes it (the kernel's
   rustup home, `RUSTUP_AUTO_INSTALL=0`); with a machine mapping, the `[rust_toolchain]` request
   against it. A toolchain that does not resolve refuses the Task with the policy file, the field
   and the probe's answer; nothing runs and nothing is charged. A Snapshot that pins nothing names
   nothing to refuse, and a probe that fails at check time keeps that check cold with its reason
   (ADR-0131).
5. **Fixtures are portable.** A fixture sets the modes it asserts instead of taking them from
   the umask, resolves a system tool by its real path and accepts every prefix the policy under
   test lists (the Rust coreutils multicall binary included), finds a C linker where rustup's
   host toolchain does (`CC`, then the sanitized `PATH`, then `~/.local/bin`), and the protected
   harness pin ignores the author's own `HOME` and Cargo state and, outside a Git work tree, what
   the root `.gitignore` ignores, so a pin made in a Worker sandbox after a build there equals
   one made on a developer machine. Fixture walls whose subject is not a deadline are the
   load-safe wall (ADR-0114), and a test that waits for a terminal or a file event waits on the
   observable state it reads.

## Consequences

- A gate a load flake failed no longer discards its candidate: it passes with the flake on its
  own line, and the report names the host's load beside it. A test that fails alone still fails
  the gate.
- A Task that ended at a failed gate is retried with one command; its accounting keeps the
  failed check Attempt and adds the new one, and the plan and the candidate are the ones the
  developer confirmed.
- A Task whose pinned toolchain this machine lacks stops before its implementer, with the field
  and the fix, instead of building cold.
- A release before this one cannot read a Task log that holds `TaskGateRetried@1`; it fails
  closed on the unknown event type (ADR-0145). `af/TaskCheckReport@1` is evidence beside the
  receipt, among an Attempt's raw artifacts, which every reader selects by type.
- Every check of every Task now records one more artifact. A gate that writes no report records
  `absent`; nothing a check reports can change its result.
