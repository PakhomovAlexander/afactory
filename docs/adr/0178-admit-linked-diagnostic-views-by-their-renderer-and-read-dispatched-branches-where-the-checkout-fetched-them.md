# ADR-0178: Admit linked diagnostic views by their renderer, and read dispatched branches where the checkout fetched them

**Status:** accepted (2026-10-20). Closes package p6a of the
[continuous performance laboratory](../design/continuous-performance.md). It repairs the gate
failure and the three root findings left by
[ADR-0177](0177-schedule-continuous-runs-from-trusted-control-and-move-builds-once-to-bounded-shards.md),
whose decisions stand except where this ADR amends them. No persisted contract changes: the
report stays `af.perf-report/2`, the schedule `af.perf-schedule/1`, the profiles
`af.perf-profiles/4`.

## Context

The p6a sealed source failed the mandatory gate once, and root checks confirmed three findings
on it:

1. **A fixture without the lifecycle** (`p6a-gate-diagnostic-fixture`).
   `tests/toolrun_supervised.py` built its `Profile` with `__new__` and no `retained`. Since
   ADR-0177, `Profile.halted` reads `retained` before every launch, so the aggregate cases raised
   `AttributeError` before they ran, and `diagnostic_tools` failed under the native supervisor.
2. **Integrity taken for safety** (`p6a-root-child-page-admission`). `report --diagnostic`
   verified a child's bundle hashes and identities, then copied its `diagnostic.html` and linked
   it as the detailed view. A child whose page was replaced by script-bearing HTML without a
   CSP, its manifest recomputed, was accepted, copied and linked. Linked sanitized SVG was
   trusted the same way, by its hash.
3. **Documents that disagree** (`p6a-root-child-source-identity`). `admit_child` compared the
   profiles' sources with the parent, but not the frozen child plan's resolved sources. A child
   plan naming another candidate, its digest and the profiles' `plan_digest` recomputed, was
   accepted.
4. **Advertised branches that never resolve** (`p6a-root-dispatch-checkout-refs`). The manual
   workflow advertises `refs/heads/…` for base, head, control and authority. The pinned
   `actions/checkout` with `fetch-depth: 0` fetches `+refs/heads/*:refs/remotes/origin/*` and
   `+refs/tags/*:refs/tags/*` and creates only the event's branch locally. The control step and
   `ci._dispatch_ref` resolved `refs/heads/NAME` directly, so every other fetched branch was
   `unresolved_ref`.

## Considered options

1. **Give `Profile` a permissive default for `retained`.** Rejected: production stays fail
   closed. The fixture is repaired to the constructor's lifecycle instead.
2. **Rewrite the copied child page in the report.** Rejected: the copy would no longer be the
   archived child. A derived report never rewrites a child bundle.
3. **Scan the supplied page or SVG for `<script>` and `on…`.** Rejected: a denylist misses what
   it does not name. The view must be what the trusted renderer makes.
4. **Drop branch selectors from dispatch.** Rejected: the manual workflow advertises them.
5. **Resolve `NAME` or `origin/NAME` through `rev-parse`.** Rejected: its DWIM search through
   `refs/`, `refs/tags/` and `refs/heads/` guesses. Each ref is read exactly.

## Decision

1. **The fixture is the constructor's lifecycle.** The supervised aggregate profile sets
   `retained = []`, `stage = None` and `work` as `Profile.__init__` does, the other fields the
   step reads unchanged. A new `aggregate_halted` case keeps scratch for recovery and asserts in
   `diagnostic_tools.rs` that the step is refused `cleanup_uncertain` before anything is
   written. The 32-byte cap and 16-byte room, deadlines, cancellation and every existing
   assertion are unchanged.
2. **A child's documents must agree** (`report._consistent`).
   - **Parent.** The frozen child plan's `resolved`, `selectors`, `trigger`, `tier`,
     `authority` and `build_inputs` must equal the parent's. So must its profile's environment
     ID, environment digest and build.
   - **Plan.** The profiles' sources must be the child plan's own resolved baseline, candidate
     and authority. Their recipe ID, recipe digest, features, symbols and frame pointers must be
     the child plan's. Their capture frequency and duration must be the child plan's
     `diagnostic` bounds.
   - **Selection.** The profiles must select each case the child plan froze, with its case ID,
     exactly once, and only those. Every capture, render, trace and counter must be one of
     them.
   - **What stays.** The diagnostic recipe stays the child's own, separate from the parent's,
     and omissions and partial evidence are kept as they are. None of this needs a real
     capture.
3. **A view is the trusted renderer's** (`report._admit_views`). Every check runs before the
   report writes a file.
   - **Flamegraphs.** Each sanitized flamegraph must be exactly what `svgsafe.sanitize` makes
     of its original, removal counts included. An original the sanitizer refuses is refused.
   - **Traces.** Each kept trace is validated as `af.perf-trace/1`. Its `perfetto.json` must be
     `phasetrace.perfetto` of it, and the profiles' span, drop, open, async, region and coverage
     claims must be the trace's.
   - **Page.** `diagnostic.html` must be byte for byte the page `diagnostics.page` draws from
     the admitted profiles and traces, with the renderer's CSP, escaping and local links. A page
     or SVG holding a script, an event attribute or an external reference is refused, never
     linked.
   - **The copy.** The copied child is still its exact bundle, and the parent's evidence and
     verdict never change.
4. **Branches are read where the checkout fetched them** (`ci._branch`, the workflow's control
   step).
   - **Branches.** `refs/heads/NAME` is the commit of `refs/remotes/origin/NAME`, or of the
     local `refs/heads/NAME`, each read exactly with `git show-ref --verify`. When both exist
     and name different commits, the selector is refused `ambiguous_ref`. When neither exists,
     it is `unresolved_ref`.
   - **Everything else.** Full SHAs and `refs/tags/` resolve as before. Short and bare names
     are still refused, and the plan freezes the resolved SHAs.
   - **The control step.** The workflow's control step applies the same rule in shell before
     any harness code is imported. Nothing reads a credential.

## Consequences

- **Tests.**
  - **Admission.** `test_report_diagnostics.Admission` makes children with the actual driver: a
    no-capture child, a captured profile child and a trace child. It admits all three and
    checks that neither the parent nor any child bundle changes.
  - **Hash-consistent negatives.** Each one is resealed with a recomputed manifest, profiles
    hashes and plan digest. They cover a page that is a script, has an event attribute, has an
    external link or lacks its CSP; a sanitized or original SVG with an inline script, an
    `onload` or an external `xlink:href`; an original holding a DTD; and a Perfetto export
    that is not its trace's. They also cover a child plan naming another candidate, control,
    selector or authority; profiles naming other sources, recipe, capture bounds or selection;
    and a captured child naming another candidate. All are refused with nothing written. With
    the two new checks removed, the twelve page, SVG and captured-candidate negatives are
    accepted.
  - **Fetched branches.** `test_ci` and `test_performance_workflow` fetch a disposable origin
    with the checkout's exact refspecs into a new checkout of `main`, with no local candidate
    or control branch. Dispatched branch, tag and SHA selectors plan with exact identities.
    Absent branches are unresolved, and a disagreeing local branch is ambiguous.
  - **Supervisor.** `diagnostic_tools` runs the repaired fixture through the native supervisor.
- **Pending operational evidence.** Unchanged from ADR-0177. No hosted run, real capture,
  calibration or profile is inferred from these fixture tests.
- **Not changed.** The 15-second lease, the 8-MiB envelope cap, the renderer's CSP, every
  diagnostic view and link, every verdict bound and every persisted contract version.
