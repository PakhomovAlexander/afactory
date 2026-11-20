# ADR-2026-11-17-bound-implementation-driving-to-active-frames: Active Scenario deadlines

**Status:** proposed (2026-11-17).

## Context

Issue 834's source candidate already contains explicit startup admission, native terminal-outcome
repairs, scoped implementation publication and retained foreground driving. A completed child
frame remains in the journal as evidence. The driver nevertheless included its elapsed limit in
both expiry and wake scheduling, allowing old evidence to stop unrelated later parent work.

## Considered options

- Extend or reset deadlines on resume: rejected; owner authority and resource bounds stay fixed.
- Delete completed frames: rejected; their identities and accounting remain necessary evidence.
- Schedule from unfinished frames: selected; it matches native ordered Scenario execution.

## Decision

Compute one deadline frontier for retained driver expiry and wake scheduling. It includes the
Run's elapsed limit, startup expiry, unfinished frames' elapsed limits and active steps' deadlines.
Completed frames retain their exact bytes and accounting but no longer constrain later work.
An unfinished ancestor still constrains its descendants. Resume reservations remain bounded and
are never refunded by process loss. This changes no persisted shape or historical Store.

The combined issue 834 capability retains only the following scoped departures from ADR-0274:

- Clause 2's accepted-only observation permits explicit terminal `task_outcome` evidence for
  bounded fresh native repairs, as specified by the
  [repair decision](2026-11-16-route-terminal-native-outcomes-to-bounded-repairs.md).
- Clause 4's later signed decision is replaced only for `publish_implementation_pr_v1` by the
  complete owner-confirmed startup scope and fresh native publication guards, as specified by the
  [publication decision](2026-11-16-publish-implementation-under-native-startup-scope.md).
- Clause 6's observe-once cadence permits retained continuous driving and scoped idle Waits
  without new dispatch allowance, as specified by the
  [driver decision](2026-11-16-drive-scoped-implementation-with-retained-progress.md).

Those are the only generic publication, Wait and repair clauses superseded. Generic exact signed
publication remains available. Native acceptance and independent whole-change verification remain
mandatory; terminal findings never become publication acceptance. No merge capability, signing-key
retention, synthesized signature, direct model dispatch or arbitrary shell dispatch is introduced.

## Consequences

A journal-reopen regression completes a child before its bound, resumes the parent after that
child's bound and checks unchanged dispatch/accounting and the parent's still-enforced deadline.
Existing isolated native scoped tests exercise initial gate and review failures, fresh repairs,
independent verification, delayed CI, uncertain publication, CLI restart, boundary refusals and
exact cumulative Task accounting for one PR. They use controlled Provider and GitHub IO, local
gates and disposable Git repositories. They establish no paid-provider or live-publication claim,
physical VM reboot survival or mid-Worker termination recovery.

This correction uses only the captured source's native interfaces. It imports no support candidate
for issues 815 or 805 and makes no claim that those unmerged changes compose successfully. External
publication, independent verification of this candidate and operator deployment remain later steps.
