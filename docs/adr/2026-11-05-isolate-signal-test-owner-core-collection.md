# ADR-2026-11-05-isolate-signal-test-owner-core-collection: Isolate signal-test core collection

Status: proposed; accepted on merge. Date: 2026-11-05.

## Context

Issue #671 records two hosted Linux jobs timing out at the five-second owner wait in
`test_cancellation_forwards_each_supported_signal`, on SIGQUIT, before Rust compilation.
The gate owner forwards cancellation, reaps its owned children, then `finish()` restores the
signal's default disposition and signals itself to preserve the exact negative exit status.
SIGQUIT's default action also enters core-dump handling before termination becomes waitable.
The fixture command handles SIGQUIT and exits 17, so the owner's final self-signal is the
core-generating boundary in this test.

The hosted delay's precise cause is **not established**. The coordinator's unchanged Python
3.12 and 3.15.2 focused runs passed; its complete Python 3.15.2 wrapper run also passed. Neither
Python 3.15.2 alone nor a slow hosted core collector has been demonstrated as the cause.

## Options

- Skip SIGQUIT, increase deadlines or relax status/cleanup assertions: rejected; these lose
  the contract under test.
- Change production termination or add an environment bypass: rejected; no production defect
  has been demonstrated.
- Change host core policy: rejected; tests must not configure their host.
- Set only `RLIMIT_CORE=0`: rejected; Linux ignores this limit for piped core collectors
  ([core(5)](https://man7.org/linux/man-pages/man5/core.5.html)).
- Disable dumpability inside the disposable test owner: selected. Linux
  [PR_SET_DUMPABLE](https://man7.org/linux/man-pages/man2/PR_SET_DUMPABLE.2const.html)
  controls whether the calling process produces a core upon a core-generating signal.

## Decision

Launch the supported-signal cancellation fixture through a test-only bootstrap in
`scripts/test-gate-owner.py`. In that new owner process, set `PR_SET_DUMPABLE` to zero and
check it with `PR_GET_DUMPABLE`, then import and execute the unchanged production owner.
Failure to establish this policy fails the fixture. No production switch is introduced.
The executed command regains dumpability across exec; assert this, and assert that the outer
test runner's dumpability stays unchanged. Keep actual signal delivery, all four supported
signals, exact negative exit statuses, owned-child disappearance and the five-second wait.

## Evidence and limits

A disposable Python 3.15.2-slim Linux container with network disabled, two CPUs, 256 MiB
memory, read-only scripts/root filesystem and a private 96 MiB `/tmp` tmpfs reported
`core_pattern=core`. With an unlimited process-local core limit, a controlled ordinary
owner run forwarded SIGQUIT to a handler that recorded signal 3 and exited 17. The owner
then returned `-3`, with `waitid` reporting `CLD_DUMPED` and a 13,340,672-byte core file.
The test bootstrap ran the same command: signal receipt remained 3 and status remained
`-3`, but `waitid` reported `CLD_KILLED` and no core file existed. Both completed in about
0.02 seconds after cancellation. This demonstrates exclusion of the core boundary, **not**
reproduction of the hosted timeout. No host policy was changed and no piped collector was
installed or exercised. Hosted CI must still confirm whether this isolation resolves #671.

With the same isolation limits and read-only scripts mount, the candidate's complete
`test-class-python.py test-gate-owner.py /tmp/gate-owner.xml` run passed all 14 tests on
Python 3.15.2-slim, both as the default container user (21.259 seconds) and as UID/GID
65534 (20.939 seconds). The focused four-signal cancellation case also passed as UID/GID
65534 on Python 3.12-slim (1.798 seconds). These are local Linux checks, not hosted CI.

## Consequences

The signal assertion no longer depends on the disposable owner's host core collection.
Production signal semantics, PID ownership, cleanup, deadlines, Linux coverage and the
accepted structured contract2/single-call behavior remain unchanged. Other ownership and
fallback tests still execute the ordinary owner. This does not establish that every possible
cause of delayed termination has been removed.
