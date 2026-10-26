# ADR-0215: Give the guarded improvement fixture a controlled timing margin

**Status:** proposed (2026-10-26).

## Context

The guarded experiment compares whole-command `elapsed_ms`, including real builds and
setup. Under load, the disposable 1,500 ms slowdown produced an 8,148 ms baseline median
and a 10,790 ms candidate median: removing the sleep still regressed. The fixture needs
a larger controlled signal for its successful comparison without changing the staged
objective or relying on retries to hide a failure.

## Decision

Use a 7,500 ms sleep in the disposable source revision for the successful `improve`
scenario. Before tampered-harness and malformed-report/refusal scenarios, commit a second
disposable consumer revision with the original 1,500 ms sleep. Derive the tampered
candidates' removal edit from that second revision. These commits exist only in the test's
owned consumer repository. All original assertions remain.

A fast constructed regression reads the actual staged objective and invokes the kernel's
comparator. The observed 8,148/10,790 ms pair regresses; conservatively crediting only one
additional 6,000 ms baseline sleep improves by the unchanged ten-percent threshold.
Three 7,500 ms validations consume 22,500 ms within the unchanged 30,000 ms probe sample
timeout. These constructed receipts are arithmetic regression coverage, not measurements.

Export three homogeneous `af.perf-kernel-receipts/3` documents: `missing-inputs` with
`slowdown_ms = 0`, `improve` with `slowdown_ms = 7500`, and `refusal` with
`slowdown_ms = 1500`. The last contains all six tampered candidates and all three
malformed-report/refusal Tasks. Assert this exact ordered partition and retain every
Task's complete receipt. Each document passes the unchanged schema and laboratory reader
before export, using `receipts-<pid>-<phase>.json` filenames in
`AF_PERF_KERNEL_RECEIPTS`. A fast regression checks membership, retained payloads, filenames
and both validators, and refuses missing, extra or reordered Tasks. The former single
7,500 ms document mislabeled the other source phases.

The staged policy stays byte-identical. Real builds and timing, three repetitions, Worker
budgets, protected guard, golden oracle, tampered assertions and production behavior stay
unchanged. The implementation Worker runs formatting and the fast regression; the separate
native kernel gate owns the full repository check. No ad hoc twenty-repeat proof or full
suite runs inside the implementation Worker.

## Consequences

The successful fixture has a larger controlled signal while refusal scenarios retain their
shorter baseline. This margin addresses the observed loaded pair, not arbitrary host load;
gate failures remain failures. This is test reliability work and declares no replacement
cohort, speedup or deployment.
