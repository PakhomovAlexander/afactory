# ADR-2026-11-16-drive-scoped-implementation-with-retained-progress: Bounded continuous driving

**Status:** proposed (2026-11-16).

## Context

Scoped implementation publication needs several genuine preparation checkpoints inside its
immutable 120000 ms step deadline. Applying a 30000 ms idle delay to every checkpoint can spend
that deadline without publication. Repeating manual resumes hides that scheduling defect and
provides no retained resume allowance after CLI loss.

## Considered options

- Extend deadlines or reset them on resume: rejected; authorization and elapsed bounds stay fixed.
- Give every pending response immediate retries: rejected; uncertainty and idle CI can hot-loop.
- Accept caller counters or arbitrary progress strings: rejected; neither proves native progress.
- Retain signing keys: rejected; the existing explicit startup scope supplies publication authority.

## Decision

For the explicit scoped implementation capability, supersede only ADR-0274's observe-once drive
cadence and dispatch accounting for repeated idle Wait observations. Generic signed publication,
performance publication, Task acceptance, repair limits and external human merge stay unchanged.
For checkpoints within one foreground process, this also supersedes only the full-preflight-per-resume
clause of the [startup decision](2026-11-16-admit-implementation-startup-scope.md): revalidate the
complete pool on process startup, retaining live native dispatch and final mutation guards.

Admit a complete driver schedule before Task execution. The default idle cadence is 30000 ms;
two idle polls must fit strictly within each publication step's effective immutable deadline.
Validate every branch, including conditional repairs, before `start --execute`, native admission
and scheduled Run creation. The generated recipe explicitly pins publication to 120000 ms.

The pinned workspace adapter has exactly two closed publication checkpoints: checked native
implementation/independent-verification proof, then checked owned preparation/lineage. Each is
journaled before its typed response. Native proof validation remains mandatory; no arbitrary
command Worker or synthetic accepted proof is admitted. A progress response carries its exact
intent and input digest. Pending, uncertainty, missing responses and observation text carry no
credit. A skipped response can lose a credit; it cannot manufacture one.

The Scenario Store retains the observed finite frontier and consumed acknowledgements. One
transaction consumes an available checkpoint and a resume reservation before calling the runtime.
Repeated/replayed checkpoints cannot earn another credit. A restart retains its not-yet-due
reservation even if the Run was left running. Resume count is bounded at 512; checkpoints, idle
polls and crashed calls all consume that same allowance. Task, dispatch, repair, scope and time
bounds are unchanged. Re-observing one scoped Wait intent spends no new dispatch allowance.

`af scenario drive RUN` is a foreground service against the existing Store. An external supervisor
may restart that exact command and identity. It sleeps until due scheduling or an immutable
boundary; it installs no service. Startup preflight runs once per service process, while native
per-dispatch binding and final fresh mutation guards remain live. Standalone scoped resumes use
the retained schedule too. Ant ticks drain only newly recorded finite progress.

Elapsed/expiry and exhausted resume allowance stop the Run with a retained typed driver blocker.
Other runtime failures retain their exact diagnostic beside the terminal Run. Unreadable journal
bytes remain a fail-closed corruption error; no repair or replacement of corrupt evidence occurs.

Pre-publication native repairs export against their original committed baseline through bounded,
checked native continuation and source-binding receipts. A same-PR continuation stops at the exact
published predecessor. Export receipt 2 distinguishes that baseline from the accepted Task's
immediate source; request 2 selects these semantics explicitly. Generic object export still grants
no publication authority, and scoped publication independently validates complete Run membership.

Store 14 adds driver 1 records. Adapter response 2 adds the two closed progress outcomes; GitHub
journal 7 retains the checkpoint. Writers, readers, schemas and current parity tests change
together; prior versions are refused without rewriting historical Stores or captured evidence.

## Consequences

The actual default driver can advance genuine preparation immediately and preserves idle cadence.
Native fixture evidence distinguishes CLI process restart from untested physical VM reboot and
mid-Worker termination. Deterministic local Provider/Git/API fixtures establish no live GitHub
permission, paid-provider behavior, or measured performance improvement. Original issue integration
and independent verification of the delivered stage remain operator work.
