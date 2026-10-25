# ADR-0205: Integrate Telemetry into the performance laboratory as diagnostics and advisory experiments

**Status:** proposed (2026-10-24); its experiments' execution, verification, upload and
collection are amended by
[ADR-0206](0206-contain-telemetry-experiments-and-collect-only-results-verified-under-their-authority.md).
The consolidated integration includes published main and
[ADR-0208](0208-bind-calibration-dispatch-to-the-checked-main-source-and-separate-group-arguments.md).
The old cohort bytes and pinned collector remain frozen; its changed producer requires a new
cohort after the integrated repair is committed. Renumbered from unmerged ADR-0199 after
published main's ADR-0204; published-main ADR0199–0204 retain their identities.

## Context

Telemetry has landed (ADR-0153): `af telemetry runs`, `show`, `status` and `config` print
versioned JSON documents with committed schemas, and three credential-free bench scripts measure
Telemetry's own cost and read latency (design §12). The telemetry design (§13) proposed that its
runtime level replace the laboratory's `perf-trace` feature and its Perfetto export. Two facts
stand in the way:

- The native perf-probe installs no telemetry subscriber.
- Discovery, startup and state-read commands deliberately record no telemetry.

So telemetry covers one CLI case of the suite (`cli_task_run_check`) and no native case. The
owner asked for telemetry in diagnostic reruns and reports. They asked for continuous
measurement of telemetry's overhead, default behaviour, stream handling and read latency across
pull requests and releases. And they asked for an investigation of the `authority_runtime_new`
slowdown. The initial draft also froze `performance.yml` for the hosted calibration cohort of
23 to 29 October. The integrated source preserves that cohort as disabled historical evidence;
published main and ADR-0208 change the workflow producer. The timing verdict is unchanged.

## Considered options

- **Replace `perf-trace` with telemetry everywhere.** Rejected: it would remove the phase
  coverage of every native case and every non-recording CLI case, for spans those paths never
  emit.
- **A new diagnostic recipe kind `telemetry`.** Rejected: the frozen plan's recipe kind is part
  of `af.perf-run/6`, so this needs `af.perf-run/7` and every plan fixture and digest with it,
  for an option that adds no build of its own. Telemetry is a runtime level of the same binary,
  selected like the execution mode, not a build.
- **Parse segments or the index directly.** Rejected: the reader would be a second, more
  permissive reader of the Telemetry Store, and it would drift from the product's own readers.
- **Fold the bench scripts into the comparison as verdict cases.** Rejected: it would change the
  suite, the calibration and the frozen workflow mid-cohort. The scripts' documents are also not
  laboratory samples.
- **Attach the scripts' JSON to runs as loose files.** Rejected: such files carry no identity of
  the binary, source, authority or host, no record of failed runs, and no rule for when two of
  them may be compared.

## Decision

1. **Telemetry captures in diagnostic children.** `cli.py profile --telemetry spans|content`
   is an explicit option of a CPU-profile or trace child, never of a counter child.
   - **Recording.** For each selected case that records telemetry, and for each side, the
     side's diagnostic AF binary runs the case's command once more at that level, in the
     case's private namespace, judged by its oracle. In the same namespace the same binary's
     `af telemetry runs|show|status|config --json` follow. Each document is bounded while it is
     written and admitted as evidence. It is read only through the product's committed schema
     by a strict evaluator (`perflab.jsonsubset`) that refuses any keyword it does not
     implement.
   - **Derivation.** From those four documents alone the child derives the run and its
     executions, bound to the case, side, source and binary. It also derives phase rows (union
     of intervals and the product's self time, never a sum across overlaps), a bounded
     timeline, a Perfetto export without any attribute, and an evidence state: `complete` only
     with nothing open, unsealed, dropped, recovered, unreadable, problematic or incomplete,
     otherwise `partial` with every reason. Each shown execution needs exactly one segment
     observation, matching its state and complete capture metadata. Missing observations or
     unknown metadata make the history partial; repeated observations, contradictory states
     and counts are refused. Report admission repeats this reconciliation.
   - **Matrix.** Every other selected case is an omission naming the supported matrix
     (`no_telemetry_subscriber`, `records_no_telemetry`). The trace recipe keeps their phase
     coverage.
   - **Contracts.** `af.perf-profiles/5` records the level, its perturbing label and every
     capture. `af.perf-report/3` adds each case's telemetry state from linked children. The
     report derives every capture again from the archived documents and refuses a child whose
     figures differ. Earlier versions are refused.
2. **Telemetry experiments.** `fixtures/performance/telemetry.toml`
   (`af.perf-telemetry-lab/2`) names exactly the three bench scripts, their documents, schemas
   and levels, and one bounded profile per trigger. `cli.py telemetry-run` measures a baseline
   and a candidate binary on one host, with one measurement authority identified by the SHA-256
   of every file it reads and checked again at the end.
   - **Rounds.** The order of the sides alternates by round, and both sides of a round share
     one seed. Every planned run is recorded with its status.
   - **Judgment.** Each side is judged by the script's own rule, recomputed from its pairs, so
     the full-scale and minimum-pair requirements stay the script's.
   - **Dimensions.** The level, metric and shape are explicit dimensions. Cross-revision
     figures are set side by side only between documents of one round whose protocol, corpus
     and machine agree, on one host; anything else is `incompatible` or `missing` with its
     reason.
   - **Contract and history.** `af.perf-telemetry-result/1` keeps the raw documents and capped
     logs and is verified offline (`telemetry-verify`). `telemetry-history` keeps verified
     bundles in a durable root and groups them only by configuration, authority and host
     (`af.perf-telemetry-history/2`). `content` is never an experiment. ADR-0206 contains every
   execution, admits a document only as its planned run and verifies only under the exact
   authority.
3. **A separate workflow.** `.github/workflows/performance-telemetry.yml` runs the experiments
   on pull requests (base as control and baseline), nightly, on publication and on dispatch.
   - **Build apart.** Each side is built in its own job, and only its binary reaches the
     measuring job, by SHA-256.
   - **Containment.** Read-only permissions, no secrets, pinned actions, checkouts without
     credentials; every execution of a candidate runs contained, and only an admitted result is
     uploaded (ADR-0206).
   - **Status.** Advisory, with the profile's retention.
   - **Historical cohort.** The old cohort file stays byte-identical, pinned by a test. The
     integrated `performance.yml` includes published main and ADR-0208's dispatch-head repair;
     a new cohort must be declared from the final committed reviewed producer.
4. **An investigation entry point.** `cli.py probe --trace-output PATH` writes the measured
   request's validated phase trace and prints its phase summary and region breakdown. The
   investigation note records what it measured.

## Consequences

- **Supported matrix.** The matrix is explicit and the `perf-trace` recipe stays. Replacing it
  would take telemetry spans in the probe and in the non-recording commands, and a measured
  equivalence.
- **Versions.** Profiles and reports move to new versions, and readers refuse the old ones as
  ADR-0145 requires. The retained p8 evidence keeps its own versions and is read by no current
  reader.
- **No verdict effect.** Telemetry results never enter a verdict, a calibration or a required
  check. A candidate can falsify at most its own run's advisory document; everything else is
  contained and verified again under its exact authority (ADR-0206).
- **Collection.** A separate, bounded telemetry collection lane keeps verified results in a
  durable history (ADR-0206); the archive collector does not fetch these artifacts.
- **Runtime investigation.** The `authority_runtime_new` investigation found three whole Task
  projections in `TaskRuntime::new`, the first in `begin_task_execution` at 34–46% of the
  region on a loaded host. That share alone does not explain the historical +33–34%; the
  hypothesis is unconfirmed. It made no optimization: the candidate changes touch the Store's
  execution authority, and their semantic tests do not exist yet
  ([`performance-runtime-new-investigation.md`](../performance-runtime-new-investigation.md)).
