# Changelog

Every release has a section here before it is tagged: `scripts/release.sh X.Y.Z --compat "…"`
writes it from the pull requests merged since the previous release, and the release workflow
publishes the section as the release notes. The **Authority compatibility** line is mandatory:
it says whether committed `.af/` policy keeps working as is, needs `af onboard --refresh-lock`,
or needs a documented hand edit.

This file includes the public alpha and recent release candidates. Earlier releases are described on their
[GitHub release pages](https://github.com/PakhomovAlexander/afactory/releases), and git history
keeps their sections. In short: `0.7.0` and `0.7.1` reviewed from a `.review/` directory;
`0.8.0` moved project authority to `.af/` and made a project pin the `af` release it runs; the
`0.9.0` release candidates brought the common Task runtime, Task files and the Worker warm
layers. `0.9.0-rc.5` was tagged but never published — its macOS check leg failed before the
publish step — so everything it carried shipped in `0.9.0-rc.6`.

## [Unreleased]

Changes since the last release are notes under [`changelog.d/`](changelog.d/), one file per
pull request; the release pull request collects them here.

## [0.14.0-rc.1] - 2026-10-29

### Authority compatibility

Committed .af/ policy valid under 0.13.0 needs no hand edit. Run af onboard --refresh-lock to pin 0.14.0-rc.1 and its signed bytes. State written by other releases remains unsupported under ADR-0145.

### Changes

- release: v0.13.0-rc.2 (#337)
- chore(deps): bump actions/setup-python from 7.0.0 to 7.1.0 in the actions-minor-and-patch group (#339)
- release: v0.13.0 (#344)
- perf: connect telemetry experiments and diagnostics to the performance lab (#338)
- test: preserve private AF fixtures across filesystems (#350)
- fix(perf): run real telemetry fixtures within bounded scratch (#354)
- fix(perf): bind CPU symbol evidence and prevent perf cache cleanup failures (#356)
- perf: declare the replacement hosted calibration cohort (#377)
- docs: rewrite the README for people and agents (#383)
- Stabilize guarded macOS performance gate (#345)
- feat(ci): add label-controlled test classes and fast/full releases (#342)
- test: prove release profiles in CI without publishing (#396)
- fix: land reviewed telemetry and test-validation follow-ups (#404)
- ci: default every PR to fast validation (#405)
- gate: fast Task gate, manual kernel measurement, targeted Worker tests (ADR-0251, ADR-0252) (#419)
- feat(perf): run Performance Lab independently on a locked VM (#398)
- Add the default Task pipeline to the catalog (ADR-0253) (#422)
- tui: open the browser before any Store is read (#402) (#407)
- Agent UX, first tier: sandbox-aware probes, agent entry point, classified refusals (#392)
- fix(perf): confirm container removal by a typed outcome, within its allowance (#388) (#417)
- feat(task): close out #331: attempt-wall sealing, worker input trees, check walls, typed acceptance, error@2 recovery, stress proofs (#428)
- Agent UX, next tier: additive Task onboarding, review JSON and first screens, Task waits (#423)
- Agent UX, later tier: native reviewer schemas, command catalog, findings handoff, attention view, event stream (#429)
- catalog: carry the kernel's diff_subject input in the default reviewer packages (main's default pipeline does not compile since #428) (#437)
- Derive Task gate evidence from the check report, one tests record per run (#346) (#438)
- Batch scratch enforcement syncs within the existing verification budget (#442)
- tests: make the macOS kernel gate pass again on main (wrapped next lines under a long TMPDIR, read-only copies in the receipt migration) (#440)
- Agent entry point: four rules, skills Codex loads, refusals that name the next command (#363, #394, #434) (#444)
- tui: shape the browser around what a person looks for (limits always, the Project's view, f for what runs) (#446)
- docs: correct the long-TMPDIR counts in the local executor note (#447)
- fix(provider): match errno values on word boundaries (#448)
- fix(handoff): keep the requested change and verification in a long agent prompt (#450)
- fix(task): refuse a planned Task on every event poll; bound the follower test (#451)
- fix(catalog): complete the effects of four commands (#453)
- Review screens: compact plan path list, onboarding preview width, starter boilerplate (#454)
- chore(af): make the default pipeline afactory's [task.defaults] (#449)
- Offer a light default pipeline for small changes (ADR-0262) (#452)
- perf(tui): bare af's first browser frame as a performance target (ADR-0258, Task B of #402) (#439)
- tests: read a wrapped next line exactly as a shell does (follow-up to #440) (#445)

Calibration groups now take separate ID, CPU model and pinned registry-image arguments,
preserving `@` inside both CPU names and complete OCI references. Calibration dispatches carry
the checked full main SHA: a moved or missing source is visibly refused as a non-member before
planning or measurement. The retained operator entry point supports genuine preflight with
`--dry-run` while disabled, preserves durable one-shot intent and ambiguous-request latches,
and has synthetic parser, workflow and operator regression coverage. The old cohort remains
frozen; a new cohort must be declared from the committed repaired producer with fresh history,
future dates and exact observed groups. No deployment, calibration readiness or activation is
claimed.

Make performance seed-refusal fixtures exercise both `0022` and `0077` creation umasks,
restore the process umask even on setup failure, and compare original modes and bytes after
refusal. Model macOS protected executable resolution in the samply capability fixture so
Linux system symlinks cannot change its expectations, retaining flags, version, self-built
and hardened-runtime coverage. Production admission and profiler protections, golden oracle
bytes, gates and budgets are unchanged.

- Contain telemetry experiments and collect only results verified under their authority. Every
  execution of `cli.py telemetry-run` runs through one executor. `--mode linux_container`, the
  only mode for a pull request, gives each execution its own container of the pinned image with
  only its own scratch directory writable, bounded while it writes. An uncertain cleanup or a
  changed binary, authority, corpus or result stops the run. A document is admitted only as its
  planned run, with every figure recomputed (read latency included), and a result verifies only
  under exactly its measurement authority. The workflow builds in the pinned image and uploads
  only what `telemetry-admit` verified and found credential-free. The new
  `cli.py telemetry-collect` lane keeps verified results in a locked, bounded history beside the
  unchanged archive collector
  ([ADR-0215](docs/adr/0215-contain-telemetry-experiments-and-collect-only-results-verified-under-their-authority.md)).

- Bring Telemetry into the performance laboratory while preserving the timing verdict and old
  calibration cohort bytes. The integrated `performance.yml` includes published-main changes
  and the dispatch-head repair; it requires a fresh cohort from the final committed reviewed
  producer. `cli.py profile --telemetry spans|content`
  records each eligible CLI case per side and reads it back through that side's own
  `af telemetry` documents and their committed schemas. The child page and report show phase
  rows, a timeline and a Perfetto file, with partial evidence named and never complete
  (`af.perf-profiles/5`, `af.perf-report/3`). The advisory `Performance telemetry` workflow and
  `cli.py telemetry-run|verify|history` measure Telemetry's overhead, invocation cost and read
  latency on a baseline and a candidate binary, judged by each script's own rule and compared
  only per level on one host. `cli.py probe --trace-output` gives the `authority_runtime_new`
  investigation a phase breakdown
  ([ADR-0214](docs/adr/0214-integrate-telemetry-into-the-performance-lab-as-diagnostics-and-advisory-experiments.md)).

Performance Lab reference generation now constructs deterministic letters, padding and source-file
fill from their literal periods and scans the exact restricted ASCII domain without Python loops
per character. The independent oracle still rederives every expected fact for the supplied registry;
all committed golden bytes are unchanged. Six unprofiled samples on one host reduced the focused
two-document median from 3.934 s to 0.987 s; this is not a product or complete-gate speed claim.
The [measurement report](../docs/performance-reference-throughput.md) retains every sample and digest.

Accounting fixtures renew their existing three-second writer lease through setup and operation
boundaries. Task dispatch now releases an admitted but unstarted reservation when cancellation
or authority checks stop it before launch, keeping the original scheduler failure. Ordinary
accounting appends may revalidate and retry once after a proven suffix of only the same live
writer's heartbeat renewals; other changes, expiry, stale writers and owned cross-log fences
still refuse. Review's outstanding-Attempt error names pending IDs and scheduler reports.
Deterministic regressions cover those failure paths; no earlier intermittent failure is
claimed resolved without its original evidence
([ADR-0218](docs/adr/0218-reapply-an-accounting-append-once-after-only-this-writers-heartbeat-renewals.md)).

Repair three repository planning fixtures that substituted command Workers under
`distinct_models = true`: reuse the published model-preserving replacements, isolated fake
Provider probes, admission reservations and check-tool PATH, with same-model and command
refusals before dispatch. Fix the credential fixture's whole-message `sk-` collision with a
trusted executable path by checking complete synthetic secret markers, retaining exact
withheld-line and safe-stderr assertions, and exercising an explicit `task-sk-parent` directory;
production redaction is unchanged. Regenerate the protected harness inventory and policy with
the existing generator (output is byte-identical); preserve `.af/`, the old cohort and
the captured integrated `performance.yml`, including published-main and dispatch-head changes.
At this earlier fixture-repair checkpoint, Rust 1.88.0 formatting and all three staged-generator
tests passed. The focused credential, positive/negative planning, original scratch/source-mutation report and
lease-fencing cases could not compile because cached Cargo dependencies were missing and
`static.crates.io` DNS was unavailable; Clippy was blocked for the same reason. At that checkpoint,
the report lease conflict remained unreproduced and unresolved: the exact append permit checks
sequence and expiry inside an IMMEDIATE transaction, and ADR-0128 documents a reserve-renewal race, but
neither identifies this failure's cause. No lease, deadline, retry or suite policy changed;
sealed-gate, independent review and goal acceptance were pending. Subsequent deterministic
settlement evidence and its limits are recorded in
[the settlement repair report](docs/performance-task-settlement-repair.md); final acceptance
must come from the enclosing Task's sealed-source gate, independent reviews and evaluation.

- Reconcile the mechanical Performance Lab merge with published main: keep published ADR0001–0213
  and their references, resolve the ADR index conflict, and renumber the eight unpublished lab
  decisions to ADR0214–0221. Preserve the published CLI isolation fixture and changelog, release
  and policy bytes, historical hashes and measurement evidence. Align documentation, source,
  help and configuration references, and regenerate schema descriptions and protected inventory
  and staged policy with the existing generators. Substantive decisions and behavior are unchanged; the
  [numbering table](docs/performance-telemetry.md#adr-numbering-after-source-reconciliation)
  records the simultaneous renumbering. Scope implementation and deployment limitations to their
  recorded checkpoints; current operations require dated operator records. No cohort is declared
  or activated and no deployment is performed by this reconciliation. The final sealed-source
  gate, independent reviews and goal evaluation remain the enclosing Task's acceptance work.

Repair trusted local telemetry execution under a Linux ancestor subreaper. Keep the script
leader unreaped while ending its owned process group, distinguish terminal processes from
executing survivors with complete bounded evidence, including a zombie leader with live
threads, and keep failed observations uncertain.
Leftover live descendants still fail the run, deadlines still stop the group, and cancellation
or active disk-check errors clean up before propagating. Isolated subreaper regressions reproduce
both predecessor failures without changing the verdict sample bound or telemetry profiles.
The ancestor and inner regression helpers resolve the captured source from their own file path;
all six modes launch without inherited `PYTHONPATH` and from a temporary working directory.

Fix telemetry collection repeatedly selecting the same newest run prefix when completed
history exceeds the per-cycle run cap. List within the existing page bound, then rotate
bounded processing through a locked, atomically written attempted-run position. Old verified
results can reach history; missing, expired and refused results and higher attempts remain
eligible within lookback. A fixed upper run ID keeps new arrivals from extending a rotation.
Incomplete listings and capped or interrupted cycles report their
limits. The new `af.perf-telemetry-collection-position/1` scheduling record fails closed on
invalid state; the telemetry runbook documents operator reset and reachability limits.
Artifact admission, history bounds and the archive lane retain their contracts.

The native gate now has a dedicated Linux child owner around its complete execution.
It promptly reaps exited orphans while tests run, so an outer AF subreaper does not
accumulate gate zombies. Each owner waits only for its own children and reserves the
gate leader's status; normal execution lets live descendants finish. After leader exit,
live descendants have ten seconds to drain before the gate fails and performs bounded
termination. Cancellation forwards the signal, then escalates after two seconds and
allows two seconds to confirm cleanup. An unconfirmed cleanup is reported as a failure.
The body and ordinary descendants retain the enclosing supervisor's process group, so its
immediate SIGKILL reaches even adopted orphans that clear their environment. Cooperative
cleanup signals only unreaped child PIDs, never the enclosing group or unrelated siblings.
This is lifecycle management, not containment; an uncatchable kill of the owner still
requires outer-supervisor reaping, and descendants that detach and clear their marker can
escape that cleanup. Non-Linux hosts execute the original gate body directly after restoring
Python-ignored SIGPIPE and available SIGXFZ/SIGXFSZ to default signal dispositions.
The lifecycle contract and platform limits are recorded in ADR-0219.
Tests, native check targets, timing/JUnit output and gate resource budgets are unchanged.

- Make the Store wide-usage and Review provenance fixtures deterministic with scoped unit-test
  time, retaining their original reservation and verification limits. Add real Store dispatch
  boundary, durable accounting and clock-isolation regressions
  ([ADR-0220](../docs/adr/0220-control-store-fixture-time-with-a-scoped-thread-clock.md)).
  Qualify the concurrency fixture's `Barrier::wait` calls to avoid false child-reap matches
  in the unchanged source guard, preserving simultaneous clock synchronization.

# Performance laboratory whole-source repairs

- Run every disabled dispatcher dry-run guard even with an uncertain retained intent; preserve
  the journal and never dispatch from preflight.
- Admit read latency only with the generator's complete corpus counts and content index.
  Version its raw protocol and registry to v2, retaining the existing statistical bounds.
- Bind measurement identity to the complete laboratory code/schema/fixture closure. Retain
  trusted authority trees, require matching installed verifier semantics and re-verify history;
  history v2 records the authority location and counts its bytes.
- Bound collection, Git, local verification, import, retention, history and cleanup by one
  absolute deadline. Collection v2 records unknown accounting honestly after interruption.
- Reconcile diagnostic executions with unique segment observations and metadata during capture
  and report admission; missing evidence is partial and contradictions are refused. The
  existing subreaper regression helper also recognizes Linux ESRCH on an opened process file
  as a vanished fixture process; permission and I/O uncertainty still fail.

No new cohort, hosted measurement, deployment or performance improvement is claimed. Existing
calibration bytes, verdict thresholds, containment and lifecycle repairs remain unchanged.

Fix telemetry collection interruption boundaries: revoke HTTP exchanges before terminal
accounting, synchronize socket ownership with cancellation, and close history lock descriptors
even when the deadline interrupts open or flock. A blocked cancelled resolver remains explicitly
unconfirmed and cannot perform later network operations.

Reconcile diagnostic retained counts, sealed/open execution summaries and both reader inventories
during capture and report admission. Contradictions are refused; observational differences stay
partial. Add production-path interruption and hash-consistent report regressions without changing
schemas, budgets, calibration evidence or statistical policy.

Refuse diagnostic execution/span identity reassignment and duplicate immutable spans in either
reader inventory, including offline report rederivation. Distinct inventory changes stay partial.
Defer Python's deadline handler only across descriptor ownership transfer/close, including with
cancelled resolver threads still alive. Preserve bounded cleanup uncertainty in Python 3.9+
exception messages and persisted cycle, run, artifact and shared archive refusals.

Integrate the Performance Lab producer with published GA Task continuation, gate reporting,
warm layers and writer recovery while retaining telemetry diagnostics, containment, collection
and calibration guards. Preserve the production 60,000 ms lease and current performance
goldens; move the seven unpublished lab decisions to ADR-0214–0220. Make the two reported
short-Attempt Store fixtures and adjacent paths deterministic with the existing scoped test
clock, prove exact deadline refusal without spend, and align the slow accounting fixture with
GA's observed-usage recovery contract. This adds no cohort, deployment or performance claim.

Reconcile the published 0.13.0-rc.1 release and trusted PR Performance report with the lab:
retain numeric verdicts, exact-plan rendering, advisory qualification and unavailable/exempt
states, plus both workflow regression sets. Control the two observed-usage recovery fixtures
with the scoped Store clock, including explicit lease expiry, cold replay and fencing checks;
keep their original budgets and the binary-copy behavior unchanged. Validation uses the current
Python runtime and latest-stable CI policy; complete native acceptance remains separate.

Refuse unchecked installed dispatcher bytecode and module authority; bind telemetry artifacts
to retained original-event plans for each Actions attempt; compile hosted telemetry sources with
owned writable state and a verified image toolchain; and bound and account for interrupted
history report staging. Add a minimal live Docker permission smoke to validation. No cohort,
deployment readiness or performance improvement is claimed.

- Separate locked telemetry dependency fetching from offline, network-disabled compilation;
  bound all writable build state on a verified 8 GiB filesystem within one deadline. Admit
  only confined bounded binary bytes, and keep tool provenance outside candidate mounts.
  Include collection status, position and incomplete records in locked peak-capacity
  reservations and final accounting. Extend the real Docker CI smoke with small-budget
  storage, network and output attacks. Hosted CI remains required before rollout
  ([ADR-0215](docs/adr/0215-contain-telemetry-experiments-and-collect-only-results-verified-under-their-authority.md)).

Reserve rounded history allocation before downloaded staging, authority expansion and retained/import
copies, including directories, parent growth and bindings. Validate bounded regular authority Git
objects and the complete path set before materializing. Keep interrupted owned bytes accounted until
locked cleanup, without evicting results. Preserve diagnostic reader uncertainty from launch through
namespace release and profile shutdown; retain scratch and writable evidence and prevent later
execution when any reader may remain live. Add actual-filesystem collection and every-reader
interruption regressions without changing persisted formats, caps, verdicts or release metadata.

Keep allocation fixtures in owned temporary scratch outside the source tree, including when
source is read-only. Preserve the shared authority-materialization interruption boundary after
the complete coexistence reservation, using the same bounded immutable Git-object admission.

Reuse the bounded immutable Git authority buffers within one collection operation after the full
coexistence reservation, preserving the materialization interruption entry point. Import captures
fresh confined authority bytes after verification, checks their complete digest/count/registry
identity, and retains those immutable buffers through the retention entry point. Unbound or
corrupted prepared evidence is refused; retained and imported output are still read back. No
preparation survives an operation. During each write-free locked allocation calculation, examine
each parent once and use its single filesystem allocation unit for unchanged rounding. All root
scans, simultaneous-copy reservations, terminal reserve and cleanup boundaries remain enforced.
These changes address measured redundant admission work before the unchanged one-second expiry
test's retention phase; hosted Python and committed whole-source verification remain required.

The guarded kernel experiment test now builds its disposable consumer from every crate file
and the complete protected performance instrument, including inventory-listed build inputs
wherever they live. Unrelated repository material no longer consumes its evaluator's existing
five-second Attempt wall twice through source and base preparation. A regression checks exact
crate, harness and staged-policy bytes and retains the evaluator's wall and read-source effect;
all original guarded-measure assertions and production timing, fencing and gate contracts stay
in force. This is fixture preparation repair, with no cohort, deployment or speedup claim.

Telemetry measurements now disable Docker daemon logging and writable shared memory and
reuse the build image's volume admission check before any candidate execution. Mandatory
Docker smoke coverage adds legitimate measurements, denied writes, output flooding,
aggregate and unlinked allocation, volume refusal and confirmed cleanup beside the existing
eight build/admission cases.

The telemetry workflow carries private, bounded scratch ownership through every execution
and final cleanup. Pending or uncertain launches and missing, corrupt or foreign evidence
retain the exact image, mount and control paths without unmounting or unlinking them. Clean
completion and a proved pre-launch refusal still clean up. Result JSON cannot authorize
cleanup, including after cancellation before publication.

Gate-owner test readiness markers publish atomically; production supervision and all
existing fixture assertions are unchanged. Calibration SHA acceptance is unchanged:
[GitHub documents case-insensitive string comparisons](https://docs.github.com/en/actions/reference/workflows-and-actions/expressions#operators).
The executable run-name test now models that rule without lowercasing the dispatch input.
These repairs make no performance, calibration readiness or rollout claim.

Telemetry experiments now report failed filesystem deletion as uncertain cleanup, retain recovery
paths and fence trusted hosted cleanup ownership before removing candidate scratch. Diagnostic
readers share the sample builder's disabled IPC/logging policy and pre-launch image-volume
admission. One entry-time deadline covers CLI build-record admission, bounded input hashing,
corpus copies, integrity checks, cleanup and terminal publication, with synchronous interruption
and the existing profile/reserve bounds. Add unprivileged cleanup and deadline regressions plus
mandatory hosted native-supervisor reader probes beside the existing Docker smoke cases.
Independent reviews, whole-source checks and live hosted validation remain separate obligations
([ADR-0222](docs/adr/0222-fence-filesystem-cleanup-and-bound-telemetry-admission-through-publication.md)).

Keep hosted telemetry scratch durably fenced throughout controller work, bound ownership
failure paths to the original deadline, and release machine locks across interrupted acquisition
handoffs. Retain decoder and renderer staging on uncertain diagnostic cleanup and refuse later
launches and bundle admission. Owner records use version 2 and refuse earlier state.

Settle fenced telemetry preparation refusals only with explicit evidence that no work began,
under the original deadline. Preserve typed launch ownership through shared supervisor pipe,
callback and removal exceptions so diagnostic readers, decoders and renderers retain writable
state and stop further launches when cleanup is uncertain. Keep owner version 2 and existing
Docker smoke assertions; live Docker and exact committed-source verification remain required.

PR validation now defaults to functional, e2e and CLI/TUI interaction tests, with performance checks
available through explicit class labels. A reviewed catalog partitions every Rust test and Python
check; stable validation gates require all selected work and report excluded classes as NOT RUN.
Local `make check` stays full. Release PR labels directly control validation without a metadata commit;
first tag creation captures the single exact merged release PR and freezes its provenance and plan
in SHA/version-bound annotated tags,
with unconfigured/manual tags defaulting full and packaged binary/archive/signature smoke always on.
Per-class JUnit and step reports distinguish execution time, summed test-seconds and runner cost,
and provide clearly labeled historical replay estimates. Separate Performance measurements follow
PR selection, report exemptions explicitly and remain asynchronous after release publication.
`make test-labels` previews idempotent label setup (explicit `APPLY=1`); release preparation checks the
profile label before mutations, and sealed AF gates keep Python evidence outside the source.
Fresh CI checkouts create the configured timing directory before recording; class runner costs and
shares count only class intervals, with base/container work reported separately.
Telemetry build/measurement smoke is selected with perf and required by both stable aggregations;
its cost stays separate from class runner shares. Native gates retain their ownership wrapper and
mandatory owner preflight. Advisory telemetry producers keep their existing scheduling and safety.

Measure the telemetry cleanup regression at the unused-work settlement boundary, including
deadline refusal and exceptional return, with the existing one-second bound. Prove boundary
entry and retain real deadline interruption, persisted fencing and cleanup refusal checks.
Delayed-preparation regressions reproduce the former total-setup false failure while checking
stalled and expired settlement independently. Production behavior and budgets are unchanged.

- Fix the CLI test fixture on hosts where the temporary directory and Cargo output use
  different filesystems; verify both shared hard links and the child-copy fallback.

- Provision bounded telemetry scratch of 16/20/24 GiB for PR, nightly/manual and release
  profiles, retaining AF's 10 GiB admission floor and the existing workload allowances plus
  filesystem headroom. Use deterministic serial corpus preparation under `--ipc none`.
  Add hosted regression coverage with actual release AF for admission and all three
  telemetry experiments; full hosted rollout and calibration remain separate obligations.
- Select the explicit first-run help oracle from `0.13.0-rc.1` (heading introduced by
  committed change `5ea4aedf`, contained in rc.1, rc.2 and stable), retaining the older
  `0.12.0-rc.3` surface and usage, required-command, version and exit assertions. This
  changes authority for a future cohort; frozen cohorts and historical evidence are unchanged.

GNU CPU diagnostics retain line-table debug information and frame pointers within the existing
build cap. Exact executables use bounded content-addressed XZ evidence; profiles bind each
retained archive's compressed hash/size to its capture and admission checks its decompressed
binary identity. Shared binaries pay once. Both perf capture paths avoid automatic build-ID
cache hard links while preserving IDs and the existing cleanup refusal rules. The ADR and
attributed prior executed-build report are retained for independent review. Actual perf
capture/render qualification remains coordinator-owned; no new speed claim is made.

Authority compatibility: profiles now write and read only `af.perf-profiles/6`, with required
nullable `symbols.archive`; versions 1–5 are refused. Other persisted contracts, ordinary
verdict/release recipes and all existing hard limits are unchanged.

- Rewrite the README for people and agents: what `af` does and why on the first screen, the same
  tested Quickstart, a Boundaries section (your checkout, publishing, credentials, environment,
  `trusted_local`) and a For agents section (planning, JSON schemas, exit codes, `next_action`,
  logins, one light review).

- Declare `quick_20261028_hosted_v7` from the actual accepted producer with six exact observed
  CPU/image groups, a fresh history anchor and the seven-day UTC schedule starting 2026-10-28.
  Preserve the invalid v6 cohort and every original; retain all statistical requirements and
  the separate parameter-sealing review before trials. Document Claw scheduling and reporting;
  no deployment, measurement success, readiness or enforcement is claimed
  ([ADR-0225](../docs/adr/0225-declare-the-replacement-hosted-cohort-from-the-accepted-producer.md)).

Give the guarded experiment's successful comparison a disposable 7,500 ms baseline sleep,
then restore a separate 1,500 ms consumer revision for refusal scenarios. A fast constructed
regression checks the observed loaded medians against the actual staged elapsed-time
objective and confirms three validations fit the existing sample timeout. The staged policy,
real builds, repetitions, budgets, guard, golden oracle and tampered assertions remain
unchanged. This repairs test reliability and makes no product speed or deployment claim.

Split the public v3 receipts into separate missing-inputs (0 ms), improvement (7,500 ms)
and refusal (1,500 ms) documents, preserving all eleven Task receipts and validating each
against the unchanged schema and reader. Phase-specific filenames prevent overwrites within
one run. A fast regression checks the exact partition and rejects missing, extra or reordered
Tasks, so one document no longer mislabels evidence from different source revisions.

Provider probes now distinguish a host permission denial from an incompatible CLI. Status and
detect report `probe_denied` with exit 8 and permission guidance; doctor shows agent sandbox
context, and `af help agents` explains what to tell a user running inside an agent.

Agent help now offers a repository pointer, explicit preview and installation of two agent
skills, and a map from launcher scripts to af commands. Provider detection and review timeout
help now state their actual effects and scope. The installer uses its declared temporary directory
for scratch files.

Classified first-run CLI refusals in `af/error@2` with stable causes, structured details and
copyable next commands. `af help errors` explains the causes. Review selector and Provider
suggestions now spell every flag and require unambiguous local authority and enabled Provider
registration; generated agent guidance follows the returned command.

Add a read-only fast/full release validation proof workflow using the existing Linux and
macOS release gates, with explicit dispatch or narrowly filtered PR label opt-in. Retain an
exact SHA/version/catalog plan, local production-resolver binding simulations and existing
selected-class timing artifacts. Fast reports perf NOT RUN; no release is published and
production profile defaults and `.af/` remain unchanged.

- Add an independent Claw VM performance backend beside GitHub Actions, using the existing
  contained builders and laboratory. Retain durable local requests, exact source objects,
  reports and local calibration originals; extend the common machine lock with crash fences.
  Include bounded operator/systemd examples and explicit advisory provenance. Deployment,
  real VM measurements and calibration qualification remain operator work
  ([ADR-0240](../docs/adr/0240-execute-performance-experiments-independently-on-a-locked-vm.md)).
- Preserve checked lease forwarding in the isolated runner CLI regression. Settle only the
  telemetry build smoke's own observed fence after reaping its CLI and proving Docker and
  exact scratch cleanup under the common flock; retain uncertain or foreign state. Hostile
  builds must reach their intended compilation or admission rejection before a case passes.

- Repair diagnostic build-container ownership uncertainty for owning and borrowed machine leases;
  stop further launches and retain named recovery evidence independently of diagnostic scratch.
  Freeze a validated image-local Zig/cargo-zigbuild directory in executor configuration v2 and
  pass it to shipping parent/diagnostic builders with unchanged version pins. Drain admitted
  originals before polling a bounded full queue. Recheck frozen calibration member days after
  capacity/source preparation and retain missed originals as explicit non-executed `expired`
  state/result v2 outcomes, off schedule in readiness with unchanged denominators. Obsolete
  configuration/state/result v1 shapes refuse; no migration or fallback is provided.

Selected native execution explicitly builds the entire plan's variants before its first
successful build receipt. Independent hosted class runs build only their own variants, so a
full plan no longer causes functional, e2e or uix jobs to compile perf-only features. Hosted
perf still completes both feature builds before its build receipt. Full-plan receipts,
inventory audits, required-build failure handling, process supervision and deadlines remain
unchanged. Focused fixtures cover full native execution, every hosted class and failed builds.
Hosted timing reports recognize only the four canonical outer class labels, preserving nested
`class-build` evidence without treating it as a fifth class or counting build time twice.
CLI report/aggregation fixtures cover zero durations, unknown missing outer timings, exact
selected receipts and refusal of missing, failed, duplicate or invalid outer timing evidence.

Measure telemetry ownership-fence and final-settlement stalls from their selected I/O
boundaries, preserving real 80 ms interruption, one-second assertions and retained recovery
checks. Independent read/write cases and delayed preparation/deletion regressions prevent
setup load and earlier failures from hiding the target. Production enforcement entry/order
guards and three bypass mutation controls prevent injected timers from masking missing
production enforcement. Uniform bootstrap observations now
use their mathematically identical median directly; general inputs keep all 10,000 seeded
resamples, with exact serialized-equivalence and RNG-draw regressions. Bounded local timings
show reduced uniform computation, without claiming a hosted or whole-suite speedup.

Selected native validation now records successful workspace and selected perf feature compilation
before running class suites, so the existing warm-check test allowance starts at a real build
boundary. Inventory JSON and compiler diagnostics are preserved; failed default or feature builds
stop before tests and cannot claim successful class completion. Focused native and hosted runner
regressions cover phase order and build failures without changing deadlines or coverage.

Telemetry experiments now refuse negative, nonfinite or unrepresentable physical measurements
before summarization, including individual samples hidden by a valid median. Result v2 retains
a bounded sanitized original numeric token, precise field path and source identities with a
side/round/experiment diagnostic, then publishes a typed partial or failed result using the
existing cleanup and verification boundaries. Signed derived differences remain valid. Offline
verification replays retained refusals; upload and collection still require verified bundles.
Older result shapes fail closed without fallback. Stand-in regressions cover admission,
finalization, tampering and incomplete-result retention; the original hosted failure's field
remains unknown.
Refused paths retain complete numeric property names such as `machine.load1` and resolve only
canonical bounded paths through the exact schema. Original nonzero tokens cannot underflow
silently to zero, including negative raw latencies and signed derived figures; signed zero
remains valid. Regressions cover mixed/all-refused load results, original-byte drain/read
underflow, masked samples, long exponents, schema parity and offline/upload tampering.
Online numeric projection now respects the verifier's declared array bounds. Oversized arrays
remain whole for schema admission and produce ordinary document refusals, preventing retained
out-of-bound evidence from breaking offline verification or upload. Controller regressions cover
oversized fixture/measurement arrays and nested intervals, mixed/all-refused results, and invalid
values at the last legal index.

`af onboard --tasks` now previews and adds pinned, model-driven Task authority. In a
new repository, `--apply` installs review and Task policy together. In a repository
already onboarded for review, it adds absent Task files and appends
`[task.defaults]` without changing existing review policy bytes. The command refuses
existing Task targets or a `[task]` table, and the missing-catalog error points to
onboarding.

Every GitHub pull request now defaults to functional, e2e and uix validation, including AF gate
and release PRs. `make release` and `scripts/release.sh` default to fast; explicit class labels
and `release:full` retain their precedence. An unlabeled matching merged release PR keeps its
fast selection in the immutable release tag binding. Local `make check`, ordinary main and
unbound releases remain full. Version 2 plan, receipt and release-binding schemas record the
new release PR context; earlier versions fail closed. Base gates and packaged-release smoke
remain mandatory, and excluded perf work is reported as NOT RUN or exempt.

- Kernel implementer Workers now run only filtered tests of changed code through the `ci`
  nextest profile. A bare nextest run uses four test threads and protects the same timing
  sensitive tests as the gate; the Task gate runs the fast profile (ADR-0251).

The guarded kernel experiment's perf test refuses every tampered harness as a baseline with
no build. Its judged comparison plus tampered-candidate refusals after a measured baseline are
now an ignored manual measurement. Run `make perf-kernel-measure` before a
pull request that changes the protected harness, experiment runtime or kernel receipts. The
measurement assertions and repetitions are unchanged.

Review ledger and finding inspection now have versioned JSON output; review report accepts
`--json`. Onboarding and review planning keep topology behind `--long`, and the generated
authority guide starts with a token-free quickstart. Reviewer prompts omit Git build options
while the canonical Change Set retains them. Afactory Task defaults supply common limits and
an implementation pipeline for short Task files. Reader guides now use plain terms and
the documentation index is organized by what the reader wants to do.

Performance Lab: a sample, diagnostic or telemetry container's removal is now confirmed only by
the typed outcome `supervise.remove_container` returns (`supervise.Removal`, removed and
confirmed absent, apart from its human detail), never by that detail's wording. A failed
launch whose removal raised an exception worded like `container was not removed` was certain,
and diagnostics removed the namespace a live process could reach; now it stays uncertain and
the namespace is kept (issue #388). A removal whose confirmation ends after its allowance,
however long its `docker` client took to start, is reported unconfirmed (issue #403, item 4).
Every deadline, the removal allowance and the existing messages are unchanged. The telemetry
cleanup docs now state what a failed ownership write keeps. A failure before `state.json` is
replaced keeps the earlier fenced record. A directory sync failing after the replacement leaves
the new record, which was already proved, and the next controller and the workflow's cleanup
are admitted. Regression tests pin both boundaries.

The repository's local Task gate now runs the reviewed fast validation profile (functional,
e2e and uix), records its selection plan in the private gate run directory, and records the
shared `test-build` step before class tests. Bare
`scripts/verify.sh`, `make check`, main validation and unbound releases continue to run the
full check; pull requests default to the fast selection (ADR-0239).

- Bare `af` opens its browser before any Task or Campaign Store is read: the Tasks, Campaigns and
  Workers panes read their Stores on a thread behind a spinner on their folder, Store by Store,
  so a machine whose registry names dozens of Stores no longer sits on the splash for as long as
  every event log takes to replay (40 s measured; issue #402,
  [ADR-0240](docs/adr/0240-open-the-browser-before-any-store-is-read.md)).
- The user scope's walk of the state root for every repository's Store is that read's too, and a
  Store registered or `:state`-added while a Task or Campaign is opened never shows the opened
  entry as gone: the listing stands until the read answers for its Store.

- The repository's catalog now pins its default Task pipeline, `default/implementation` and
  `default/verification`: one implementing iteration (`claude-opus-5-5`, high), two required
  reviewers (bugs on `gpt-5.6-terra`, correctness on `gpt-6-sol`, both high), one advisory
  reviewer for gate time and quality in the `uix` slot (`gpt-6-astra`, medium) and an evaluator
  (`gpt-6-astra`, high), so any checkout plans it; `docs/tasks.md` ("The default pipeline") says
  how to run it and bind its Providers
  ([ADR-0253](docs/adr/0253-the-default-task-pipeline-lives-in-the-catalog.md)).

Task plans and status now show the last comparable recorded Gate duration when one exists. Status
puts reserved, charged and remaining tokens together and marks unknown usage separately. Task
wait can ring a terminal bell or request an OS notification, while notification failures leave
the wait result intact. Pull request bodies open their Task report with an outcome, checks,
findings, tokens and active time receipt.

- An implementer stopped by its Attempt wall keeps its edits: the kernel seals the tree it left as
  a partial candidate (`af/TaskPartialCandidate@1`), never accepted, verified or delivered on its
  own, and records the end as `af/TaskAttemptWallEnd@1` with the wall and the time the Attempt
  took. The Task ends `exhausted/attempt_wall` (exit 4), not `malformed_reply`; `af task show`,
  its `--json` (`attempt_wall_ends`), `af task report` and the run's stderr say `implementer
  reached its Attempt wall of 30 min after 30 min`, and the next command, `af task run NEW_TASK_ID
  --from TASK`, starts a new Task from those edits. The Attempt is charged and counted as every
  failed Attempt is and spends no recovery allowance. `af task plan` prints each Worker slot's
  Attempt wall beside its model (`slots` under `--json`). Identity probes that proved nothing
  after their retries now read `provider_failed/provider_environment` with `af provider status`
  next, and an account that answered as another reads `identity_changed`. The Task state is
  `task-state-v2`, so the documents that print it are `af/task-inspection@14`, `af/task-list@4`,
  `af/task-list-entry@4`, `af/task-checks@3`, `af/task-collected-inspection@3`,
  `af/task-detached@3`, `af/task-findings@2`, `af/task-report@3`, `af/task-status@2` and
  `af/task-cancelled@2`
  ([ADR-0257](../docs/adr/0257-seal-the-implementers-tree-when-its-attempt-wall-ends-it.md)).
- A wall end survives its writer: the Attempt's reported usage is retained before the wall end is
  written, so a CAS failure then loses no counter, and the wall end's ID is kept in the Store
  before the rest of the Attempt is published, so a run interrupted after it is resumed with
  that Attempt settled `failed` at its wall, its partial tree kept for `--from` and no recovery
  allowance spent; `af task gc --apply` run before that resume keeps the wall end and the partial
  tree it names. `af task report` counts the Attempt under the new failure class
  `attempt_wall`, never `provider_failure`, and its row ends with `next: af task run NEW_TASK_ID
  --from TASK`. The plan preview lists every slot that runs with its Attempt wall, the ninth
  included, the walls beside each Worker in `--tree`, and an optimization experiment's baseline
  and candidate arms with the wall their Worker packages declare (`attempt_wall_ms` under
  `--json`) ([ADR-0257](../docs/adr/0257-seal-the-implementers-tree-when-its-attempt-wall-ends-it.md)).

- A check the kernel stops at its wall now says so everywhere: its check receipt,
  `af/TaskCheckReceipt@2`, records `stopped` with the cause `wall`, the wall and how long it ran;
  the Task ends `check_failed`, never `preflight_failed`; and `af task show`, `af task show
  --checks` (`af/task-checks@3`), `af task report` and the run's stderr say `check NAME stopped
  at its 90 min wall after 88 min` and what to do next. Only a check that did not start says
  `never started`. A Task file may state `acceptance`, typed items each judged by its ID: they
  are captured as `af/TaskAcceptance@1`, carried in the receipt and handed to the evaluator on
  the Pipeline's optional `acceptance` input, and a pass that leaves an item unjudged does not
  verify. `af/TaskCheckReceipt@1` is refused by name: rename it to `@2` in every package and
  re-pin with `af catalog digest`
  ([ADR-0255](docs/adr/0255-say-a-check-stopped-at-its-wall-and-carry-typed-acceptance-in-the-check-receipt.md)).
- Structured acceptance is refused unless an evaluator is held to it: a Pipeline that declares
  the `acceptance` port but hands it to no `verify` node is refused when the plan is compiled, a
  Task of a kind whose domain does not judge items by ID (a review, document, report or
  optimization Task) is refused at plan time, and a passing evaluation over a receipt with items is refused
  unless its evaluator received that record on its own `acceptance` input. Both plan-time
  refusals name the Task file and, under `--json`, are the typed `task_file_invalid` diagnosis
  of its `acceptance` field (`file`, `field`, the item IDs as `value`, and `next_action` the
  `af task plan --file` that plans it again), as `limits.wall_ms` is refused. The performance
  laboratory gives each side of a pair across ADR-0255 the check receipt its own source writes,
  read from that side's source, so `cli_task_start` and `cli_task_run_check` measure both sides
  ([ADR-0255](docs/adr/0255-say-a-check-stopped-at-its-wall-and-carry-typed-acceptance-in-the-check-receipt.md)).

- Test children no longer die of a sibling's stale kill under load (issue #331 item 7): no test
  guard or fixture signals a process or group it does not hold as its own unreaped child any
  more. Inspecting an ID and then signalling it is a race no start time closes (the process can
  be reaped and its ID reused in between), and on a shared gate host the ID space wraps within
  minutes while nextest makes every test a group leader, so such a kill could end a whole sibling
  test with its `af` and `git` children. A test ends what it holds; the fixture Workers, the
  parked `git` and the held Codex stand-in end their own child and themselves once the `af` that
  started them is gone; a laboratory fixture's escaped child ends when its test releases it
  (`tests.support.release_recorded`); the docker stand-in's init ends its container's group, not
  `docker rm`, and a group that ended before its signal is still reaped and reported ended; the
  laboratory's fake supervisor forwards a cancellation only while its child is unreaped (it reaps
  with SIGTERM blocked and forgets the child first); and the shared `Leftovers` guard only waits
  for and reports what a test recorded.
  The telemetry tail's real-time test joins the exclusive tests of `.config/nextest.toml`; no
  wall, retry bound, assertion or fixture budget changed. `make check` keeps running the stress
  proof's own tests (`scripts/test-stress-proof.py`) in its preflight, besides their e2e class.
- `make stress-proof` (`scripts/stress-proof.py`) is the repeatable concurrent-load proof of the
  recurring flakes (issue #331 item 8): it runs the named tests 20 times (`STRESS_RUNS`) through
  the gate's profile beside CPU-bound and process-spawning load workers it starts and kills by
  pid on every exit path, and prints each test's passes, load range and wall time
  (CONTRIBUTING.md).
- The stress proof counts a skipped case and a run nextest ended nonzero as not passed, and a
  load worker that ends on its own, printed with its status and stderr, ends the proof unproven;
  a spawn worker waits out a host's refusal to spawn instead of ending.
- The stress proof of issue #331 item 8, `scripts/stress-proof.py --runs 20 --test-threads 7` on
  a 14-CPU macOS gate host shared with other gates, 14 CPU and 2 spawn load workers beside it,
  run on the rebuilt #331 tree before its last close-out round (that round changed none of
  these tests): every test passed every run, over 42.4 min at a 1-minute load of 27.7 to 191.1.

  | Test | Passes | Load (1-min avg) | Wall s min/median/max |
  | --- | --- | --- | --- |
  | `task_starters::heavy_starter_repair_uses_current_evidence` | 20/20 | 27.7-178.9 | 30.2/39.1/91.3 |
  | `task_repair::interrupted_fix_verification_resumes_same_continuation_and_charges_the_lost_attempt` | 20/20 | 39.1-191.1 | 73.6/77.6/85.9 |
  | `campaign_loop::trusted_reuse_keeps_evidence_satisfaction_current_after_head_change` | 20/20 | 39.1-191.1 | 5.5/8.0/12.2 |
  | `campaign_loop::waiver_remains_current_when_the_subject_advances` | 20/20 | 39.1-191.1 | 5.6/7.8/12.9 |
  | `telemetry_runs::telemetry_tail_follows_a_live_run_within_the_flush_latency` | 20/20 | 39.1-191.1 | 3.9/4.5/6.8 |
  | `task_file::captured_task_kind_packages_keep_business_names_and_domain_acceptance` | 20/20 | 39.1-191.1 | 9.2/12.3/20.9 |
  | `review_plan_refusals::first_run_refusals_name_the_fix` | 20/20 | 39.1-191.1 | 2.4/2.5/3.7 |

- An interrupted command under `--json` prints `af/error@2` (version 1 is no longer printed):
  `cause: interrupted`, exit 130 or 143, `details` with the `signal` and the `task`, and
  `next_action` the command that continues it, `af task run TASK_ID` with the selectors given,
  so a script no longer parses the sentence
  ([ADR-0256](../docs/adr/0256-carry-the-recovery-of-an-interrupted-command-as-a-typed-field.md)).
  Every `af/error@2` document's `next_action` is null where no pasted line can carry it, a
  selector with a tab or a newline, or a line past 4096 characters, so each validates against
  `schemas/error-v2.json`; a `recovery_exhausted` document, whose fix the schema requires, then
  carries `af task start --file TASK_FILE` without the selectors, which its sentence completes.
- On a terminal the `next:` lines of `af task show`, `af task run` and `af task run --detach`,
  the run log's last line, and the log a detached run that ended at once replays, are held to
  its width; without one they stay whole for the script that reads them. In those lines, and in
  `af task list`, `af review plan` and `af doctor`, no line is wider than the terminal: an
  argument wider than a line, a long `--state` path or the program's own path, is cut into
  quoted pieces a shell joins, a label too wide for the command beside it (`Task ID: STATE;
  next:`) is wrapped first, and each runs as printed.
- `af doctor` offers a fix only where running it can clear its check: no `af self update --check`
  for an `AF_RELEASE_SOURCE` that is no directory, no `af onboard --repo` preview (the hint gives
  the preview and the `--apply`, the sentence's `;` outside the path), and no `af provider detect
  --apply` unless a CLI on PATH reports an unregistered login of the kind needed logged in (a
  marker alone, a logged-out context or an empty `~/.claude` registers nothing; the hint gives
  the setup to run at a private terminal). A project whose Worker needs a kind no login is found
  for names that kind (`no codex login was found to register`) and never denies a login of
  another kind that `af provider detect --apply` would register. With the default link absent, the PATH fix
  installs first, also when its directory is already on PATH, and an install with no `af` on
  PATH runs this binary by its path; a long PATH line, the elvish one included, is cut where its
  shell joins it again. A broken `.af/` is named where `af onboard` stops, by taking its steps in
  its order: a Worker package's pin before its typed `reviewer.toml`, a typed error with its file
  and line, a changed package with the `--refresh-lock` that re-pins it, a Worker package too
  large or unreadable named at that package rather than at the pipeline, and only paths below
  `.af/`. An install fix that runs this binary by a long path, alone or chained by `&&` to the
  PATH line, is broken where its shell joins it, its path in fish's quoting under fish, and
  pastes and runs as printed at 80 and 100 columns; its quoting is read as that of the shell it
  is printed for, so a path holding a backslash is never cut inside fish's `\\` escape.

- A Worker is told to read only rules and context its own tree holds. The "Read first" list names
  the Snapshot the Worker reads, and lists that tree's `.af/rules.md` and context files by digest,
  recorded as `af/TaskReadFirst@2` (`@1` is neither written nor read). A continued Task (`af task
  run NEW --from TASK`) lists its candidate's rules. Its plan prints `read first: from Snapshot
  S, …` on stderr, and a `rules:` line with both digests when they differ from what TASK read.
  Continuing a Task that an earlier release planned is refused, with the command that plans it
  again
  ([ADR-0254](../docs/adr/0254-name-the-tree-of-a-workers-rules-and-deliver-the-diff-subject-as-a-typed-input.md)).
- The Diff Subject is a typed input. A judging Worker declares the optional `diff_subject` port,
  and the kernel fills it with an `af/DiffSubject@1` whose payload is the `af/diff-subject@1`
  document. Its absence where a base exists, or any other value, fails before dispatch. No
  Pipeline may bind it. `../diff-subject.json` is no longer written; `../base` stays.
  `kernel/bugs` 1.6.0, `kernel/correctness` 1.4.0, `kernel/evaluator` 1.3.0, `kernel/ux` 1.2.0
  and `kernel/uix` 1.6.0 read the port and are re-pinned.
- Judging Attempts still materialize their tree and base. Cloning a warm template with the fresh
  stamps (#266) and integrity check a safe clone needs measured slower than materializing on
  this repository. `attach_base_template` now stamps the cloned base with the current time, and
  `cargo run -p review-pipeline --example judge-materialization` repeats the measurement.
- A Worker port named `diff_subject` of any type but `af/DiffSubject@1` is refused when the plan
  is compiled, bound or not, instead of planning and then failing before its Worker starts
  ([ADR-0254](../docs/adr/0254-name-the-tree-of-a-workers-rules-and-deliver-the-diff-subject-as-a-typed-input.md)).

- Review-Campaign reviewers now reply under a native schema, as typed Task Workers already did:
  Claude Code receives the Reviewer Result schema as `--json-schema`, Codex its strict projection
  as `--output-schema`, so the CLI refuses a structurally wrong reply inside the call instead of
  the kernel failing a paid Attempt on it. The flat result contract and the kernel's parsing and
  admission are unchanged. A reviewer whose CLI ends without a constrained reply now fails as
  `provider_failure`, charged the usage that call reported, rather than `invalid_output_contract`
  ([ADR-0246](docs/adr/0246-carry-the-reviewer-result-schema-natively-on-the-campaign-path.md)).
- `af review render` prints the reply schema a model Worker's dispatch passes on its own `reply`
  header line; `--json` now prints `af/review-render@2` (`schemas/review-render-v2.json`) with a
  `reply_schema` field, `null` for a command Worker.

- A Task-file refusal that ends naming the command to run next (`acceptance` on a review,
  document, report or optimization Task, or a Pipeline without the `acceptance` input, and
  `limits.wall_ms=0`) no longer prints that command inside one over-wide line on a terminal: the
  sentence is wrapped to the terminal and ends `then run this command again:`, and the command
  follows on a `next:` line of its own, continued between its arguments, so it runs as pasted.
  Piped stderr and the `--json` document are unchanged. The performance laboratory and its tests
  run again on Python 3.9 (macOS's `/usr/bin/python3`), as ADR-0162 requires: the container
  init regression no longer needs `os.waitid`, and the supervisor's and telemetry run's added
  error notes no longer need `BaseException.add_note`.

- `af help --json` prints a machine-readable catalog of every command, and `af help COMMAND
  --json` (`af help task run --json`) of one command and those below it: one
  `af/command-catalog@1` document (`schemas/command-catalog-v1.json`) generated from the command
  definitions the binary parses, with each command's usage, subcommands and arguments, and each
  runnable command's effects: whether it may charge tokens, the state it may write (`store`,
  `files`, `machine`, `remote`) and what it needs from a person (`login`, `approval`,
  `terminal`), each with the flag that turns it on. A command without an effects entry fails the
  build's tests ([ADR-0247](docs/adr/0247-generate-a-machine-readable-command-catalog.md)).
- `af schema show ID` prints the JSON Schema of any document `af help json` lists, as `schemas/`
  holds it (`af schema show af/task-status@2`); `af schema show` lists the IDs.

- `af task findings --json` prints `af/task-findings@2` and `af review report --json` prints
  `af/review-report@5`: each finding carries an `agent_prompt` (its location on the reviewed
  Snapshot, the evidence, the requested change and how verification will judge the fix, and all
  of it as one prompt), and each round says what it covered per reviewer (`reviewed`, or
  `not_run` with why) and per path (`reviewed`, `partially_reviewed`, `not_reviewed`,
  `outside_round`). A reviewer that did not run now reads `not reviewed`, never `no findings`.
  "Verified fixed" appears only with the verification record and Snapshot that prove it.
  `--format sarif` on both commands prints the findings as one SARIF 2.1.0 log for
  code-scanning tools
  ([ADR-0248](docs/adr/0248-hand-review-findings-to-the-agent-that-will-fix-them.md)).

- An auth directory below `HOME` reads `~/…` in the doctor, onboarding and `af provider status`
  lists of directories found but not registered also when `HOME` is reached through a symbolic
  link (macOS's `/var`), as it does on Linux. The doctor tests that prove the cut forms of a
  PATH line now use a directory long enough to be cut whatever the temporary root is, so they
  hold on Linux CI's short `/tmp` as on macOS, and the test of a long install path gives `af
  doctor` a PATH it controls, so it proves both forms of the binary's fix (the `af` on PATH, and
  this binary by its path when none is) whatever the host's PATH holds. In `af doctor`'s text
  form a chained fix (`af onboard … --refresh-lock --af VERSION && af self install VERSION`)
  keeps its `&&` or `||` at the end of the first command's line; before, an operator that did
  not fit there started the next line, which a shell rejects (#331).

- `af task list --attention` groups the Tasks of a Store by what they wait for: approval, a failed
  gate, findings to answer, verified and ready to deliver, and stopped (a Provider, an interrupt,
  a limit or the wall clock ended it). Each Task in those groups is printed with its one next
  command; running Tasks are named on one line and the Tasks that need nothing are counted.
  `--json` prints `af/task-attention@1` (`schemas/task-attention-v1.json`) with all seven groups,
  `needs_you` on each. The groups are read from each Task's state alone and never recorded; `af
  help task-states` and `docs/tasks.md` list them. The browser's Tasks folder opens on the same
  groups under `NEEDS YOU`, with each Task's next command.

- `af task events TASK_ID` follows a Task as JSON lines: one `af/task-event@1` document
  (`schemas/task-event-v1.json`) per event of the Task's log, numbered with the Store's own
  gap-free `sequence`, naming what changed, the node and Attempt and a settled Attempt's result
  and exact charge. Once the Task settles it prints exactly one terminal record with the Task's
  `state` (outcome and `next_action`) and exits with the Task's exit code, as `af task wait`
  does. `--after N` prints only the events after sequence N, so a follower that reconnects gets
  the rest; `--timeout S` stops with 124 and names the `--after` that continues. The command
  takes no `--json`, every other command's `--json` stays one document, and its refusals are
  one `af/error@2` line
  ([ADR-0250](docs/adr/0250-follow-a-task-as-a-stream-of-its-store-events.md)).

- The browser's Providers pane always shows every login's quota windows: discovery hands the rows
  over, then the usage probe `af provider status --usage` runs follows on its own, so `R` no longer
  stands for a separate probe. Each window is a bar of twenty cells drawn in the brand's colour
  for how much is used (green under half, blue from half, pink from four fifths) on a grey track
  with a hairline between two bars, with the percentage used and when the window resets; the
  `models` section stands one blank row under them
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- The Claude usage probe reads the five-hour session window from the `/usage` screen as well as
  the weekly ones: `af provider status --usage` and the browser list `Claude session 5h` first
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- The Providers pane keeps the model catalogs off screen until asked for: its last row says
  `<m> to show models`, and `m` shows every listed Provider's `models` section (one row per exact
  ID) until `m` hides them again
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- With Caps Lock on the browser still answers: an upper-case letter with no binding of its own
  (`Q`, `J`, `K`, ...) is read as its lower-case key; `G`, `N`, `R` and `Z` keep their own, and so
  does `D` in the Tasks pane, where it prefills `task deliver`; to every other pane `D` is `d`, so
  `D` on a Provider removes it as `d` does. The key completing a sequence folds the same way:
  `zO` opens a fold and `gF` opens the file
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- `af provider status` and the Providers pane leave out a CLI default context whose login a
  registered Provider of its kind already holds (the Claude CLI's `~/.claude` reading the same
  Keychain login as a registered directory), as they leave out a logged-out one; the row is not
  named as a logged-in sibling either, and the names-only scan still names the directory
  ([ADR-0260](docs/adr/0260-leave-out-a-default-context-that-holds-a-registered-login.md)).
- The Workers folder lists its Workers from the start in a project scope, read on its thread
  after the first frame like the other folders; it no longer waits for an open
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- `Enter` on a Worker or a Pipeline listed in the main pane of its folder opens it, as the bar
  does; the folders' legends say so
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- The Pipelines pane never prints a declaration's TOML: a review Pipeline under
  `.af/pipelines/` is drawn from its committed declaration (Subject, convergence, budgets,
  checks, one row per node with its package or runner, gate and Demands, and the `af review
  plan` command), and a package the kernel has not planned yet, or refuses to plan, is drawn the
  same way (one row per node with its slot's Worker and Attempt bounds, a `call` expanded into
  the pinned callee with the call's bindings applied, the contract, then the refusal). The
  status line names the highlighted row's Worker
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- The project scope is the Project's view, not the directory's: the bar's root and the panes'
  titles carry the `[project] name` the repository's committed `.af/af.toml` declares (read
  again on `R` at the root), and `tasks/` lists, after this checkout's own Store, the default
  Store of every other worktree of the Project that holds
  Tasks (its registered Workspaces by label, then the worktrees git lists by name), each a bar
  group of its own; a registry Store a Workspace uses is listed once, with the Workspaces beside
  it. A Worker's STATE counts the Attempts of every checkout's Store. The checkouts are
  discovered on the read's thread, never at startup
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- `f` in the Tasks pane lists the running Tasks only: those of every Store in one list, the one
  started last first, each with its progress, how long it has run, its charge so far and where
  it runs; the Stores that cannot be read follow, so an empty list never hides one. `f` again
  lists every Task grouped by state. `Enter` on a Task row of the Tasks folder opens it, as the
  bar does ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- A duration of a day or more in the Tasks pane prints as `6d 02h`, where it printed hours
  alone (`142h 22m`)
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).

Bare `af` now has a pending 200 ms first-browser-frame performance target in the continuous
laboratory. A 100x30 pseudo-terminal case measures an empty private registry and 20 privately
registered copies of the 10,000-event Task seed, recording launch-to-browser (ending when the
status line reaches the terminal) and launch-to-first-output intervals for each sample. Its
driver ends the browser's whole session on success, failure and cancellation, finds a process
that left the session by the run's token in its environment (one that replaced its environment
before `exec` is beyond it, as beyond any process-group sample), and refuses a sample whose
browser left a process running or whose cleanup it cannot confirm (on Linux a process that has
exited and awaits its parent's reap runs nothing and is skipped); the shard and a diagnostic
child run keep that sample's scratch for recovery, as they do when their supervisor had to end
the driver. The case was added for the
splash delay diagnosed
in [issue #402](https://github.com/PakhomovAlexander/afactory/issues/402) and measures whatever
binary it is given, the repaired browser included; it does not activate a shipping budget
([ADR-0263](docs/adr/0263-bound-bare-afs-first-browser-frame.md)).

- A Task's gate evidence comes from its check report
  ([ADR-0258](docs/adr/0258-task-gate-evidence-comes-from-the-check-report.md), #346). A check
  writes one `tests` record per test run, and af folds them into one, so a gate that runs one
  nextest selection per test class no longer has its `af/TaskCheckReport@1` refused (`line 26 is
  a second tests record`). The gate's failed and flaky tests and the step that stopped a check
  come from its report when the check writes one, and from the text scan of its output only for
  a check without one (absent or refused): the failing fixtures a preflight prints are no longer
  reported as the Task's failing or flaky tests, a passed gate names only its real flaky tests,
  and `af task show --checks` ends each check line that prints this evidence with
  `evidence: report` or `evidence: output`.

- The agent pointer `af setup agents` installs in `AGENTS.md` and both skills is now four rules:
  run `af help agents` first, add `--json`, run `state.next_action`, and never add `--login`
  in an agent session. `af help agents` starts the loop at step 1 with no readiness check, and
  a command it describes needs no `effects` lookup first
  ([ADR-0259](docs/adr/0259-point-agents-at-the-loop-in-four-rules-and-own-skills-by-their-bytes.md)).
- The generated `SKILL.md` files open with YAML frontmatter, so Codex and Claude Code load
  them. `af setup agents --write` replaces only a skill whose bytes are one af wrote, the
  previous version included; any other file, one under a recomputed digest line included, is
  refused as foreign.
- With no Provider of a reviewer's kind registered, the `review_binding_missing` refusal of
  `af review plan` and `af review run` names `af provider detect` as the next command and says
  which kind each reviewer needs; with a choice, it lists `NODE: ID, ID` in words, never as a
  debug map. `af review plan` also prints the plan it computed: its reviewers and their models,
  gates, convergence and budgets before the refusal, and the whole document in
  `details.plan` under `--json`.
- `af provider detect` no longer says every authenticated login is registered when the only auth
  directory it found is logged out: it says no authenticated login was found and points to the
  private login step.
- `af help agents --json`, and `af help TOPIC --json` for every page `af help` prints, now prints
  the page as one `af/help-topic@1` document (`schemas/help-topic-v1.json`): its name, description
  and text; `af help errors CAUSE --json` too. Words that are a command still print the command
  catalog, and words that name neither still exit 2. `af help errors CAUSE` with a cause af does
  not know now exits 2 (usage), as an unknown topic does, in both forms.
- On the default branch with an uncommitted change and reviewers that still need a Provider,
  `af review plan --json` no longer says the default branch cannot be determined: its refusal's
  `next_action` is the command with the selectors af would choose, and the message names each
  reviewer that still needs a Provider. Running that command reaches the binding refusal, which
  names `af provider detect` or the choice.
- `af review plan --uncommitted --policy-rev REV` without `--base` is refused like `af review run`:
  `review_selectors_missing`, with the command that adds `--base REV`, instead of an internal
  capture error. Both give that refusal, instead of "no route covers the 0 changed path(s)",
  when the project's routing refuses unmatched paths and every route is on a diff pipeline.

- The performance lab's scratch enforcement fill syncs its file once every 64 MiB written and
  once more after the filesystem refuses a write, instead of after every MiB. A sync after every
  MiB left an 8-GiB build scratch on nested ext4 short of its capacity when the 120 s check
  ended. The refusal rules, byte bound, deadlines and 30 s cleanup reserve are unchanged, and a
  delayed ENOSPC, EDQUOT or EIO from the last sync is still the fill's
  ([ADR-0167](docs/adr/0167-bound-every-supervisor-wait-and-verify-only-what-was-observed.md)).

- The local executor verification note now states the long-`TMPDIR` results per length: the
  complete 17-test selection passed at length 104 and with `TMPDIR` unset, the three targeted
  regressions passed at 104, 144 and 224, and the separate
  `task_outcomes::a_next_line_fits_the_terminal_and_runs_as_printed` failed at 144 and 224.

- A Provider capability probe that fails with `os error 11` or `os error 111` (such as
  `Connection refused`) is no longer reported as a host permission denial (`probe_denied`): errno
  values are matched on word boundaries, and a CLI the host refuses to start is judged by its typed
  error kind ([#393](https://github.com/PakhomovAlexander/afactory/issues/393)).

- A finding's `agent_prompt.text` in `af task findings --json`, `af review report --json` and
  their `--format sarif` keeps its location, requested change and verification even when the
  evidence or the title is too long to fit: it cuts the evidence first and then the title, each
  marked `[… truncated to fit the prompt]`. Only a requested change too long to fit even then is
  cut, with the same marker, while the location and verification stay whole; the structured
  `requested_change` field always holds the complete bounded value
  ([ADR-0248](docs/adr/0248-hand-review-findings-to-the-agent-that-will-fix-them.md)).
  `docs/agents.md` now says that a `not_run` reviewer has no admitted review result for the
  round, rather than that it never looked.

- `af task events TASK_ID` refuses a planned Task with no run in progress (exit 1, one
  `af/error@2` line) at every poll, not only the first: a run that was starting when the follower
  began and ended before it took the Task no longer yields a terminal record of the planned
  state with exit 0
  ([ADR-0250](docs/adr/0250-follow-a-task-as-a-stream-of-its-store-events.md)).

- `af help --json` declares the effects four commands left out: `af provider models` may write
  machine state for a Codex Provider (`codex app-server` caches its model list in
  `$CODEX_HOME/models_cache.json`), `af review plan --file` writes the Task Store,
  `af review run --file` (and `af review --file`) may write remote state with a pipeline that
  declares remote checks, and `af task deliver` writes the Task Store as well as files
  ([ADR-0247](docs/adr/0247-generate-a-machine-readable-command-catalog.md)).

- `af review plan` lists each changed path only for a Subject of at most three; a larger one
  takes one `change` line with the first paths that fit, how many more and `--long lists each`,
  so a 25-file plan fits one 80x24 screen. `--long` lists every path, and the `--json` Subject
  still carries them all (#425).
- The `af onboard` text preview keeps its lines within 80 columns and a source-build preview
  with no Provider registered within 24 rows: the `profile` reason is shorter, a `providers`
  line too wide puts its `←` fix on a line of its own, a next step's long command continues
  between its arguments, the file count joins the `pipeline` line, the Round maximum joins
  `budget`, and a build without an install receipt says so in one line. `--long` adds the
  `convergence` line, every file and the whole warning; `--json` is unchanged apart from
  shorter next-step wording (#425).
- The software starter's `implementation-reviewed.json` no longer names `strategy`,
  `verification` or `facts.standard`: the `implement` kind's defaults supply review verification
  and `facts.standard = true`, with strategy `reviewed` in place of the `fast` it named, which
  selected nothing because the file names its pipeline (#425).

- The repository's catalog now also pins a light default Task pipeline for small, local changes,
  `default-light/implementation` and `default-light/verification`: one implementing iteration
  (`claude-opus-5-5`, medium; 1,000,000 tokens, 2 h), the correctness reviewer (`gpt-6-sol`,
  high; 300,000 tokens, 30 min) and the evaluator (`gpt-6-astra`, high; 200,000 tokens, 20 min),
  with no bug-bounty or gate-time reviewer: the correctness reviewer also owns concrete
  functional defects on the changed paths. The catalog's `[review.reviewers]` now marks `bugs`
  advisory so that pipeline may leave it unbound; a pipeline that binds it counts its findings
  and its missing result as before, but its benchmark Demands no longer block convergence.
  `docs/tasks.md` ("The light default pipeline") gives its limits and Provider bindings
  ([ADR-0262](docs/adr/0262-offer-a-light-default-pipeline-for-small-changes.md)).

- This repository's `.af/af.toml` `[task.defaults]` names `default/implementation`, the default
  pipeline every Afactory change runs through, instead of `kernel/implementation-reviewed`: a
  Task file of this repository that names no pipeline plans on it, with limits sized for it
  (tokens 8000000, max_attempts 16, wall_ms 43200000; verification tokens 2000000, attempts 9,
  wall_ms 18000000), the limits of the default-pipeline example in `docs/tasks.md`, whose
  verification Attempts rise from 8 to 9: the planner reserves one verification Attempt for each
  judging node (checks, bugs, correctness, uix, evaluator) and one for each of the four Provider
  admissions it protects, and 8 refused every plan of `default/implementation`
  ([ADR-0253](docs/adr/0253-the-default-task-pipeline-lives-in-the-catalog.md)).

Full `make check` now runs the Python performance laboratory suite as a mandatory tests-stage
step after the genuine Rust no-run build and ordinary Rust suite, preserving both runner
modes, standalone `make test`, all coverage and the existing build/test deadlines. Lightweight
preflight, catalog/schema and source-integrity checks remain mandatory; selected class wiring,
native process ownership and `.af/` authority are unchanged. A focused regression exercises
the real native wrapper, Make graph and step receipts, including a build failure that prevents
suite execution.

- Integrate exact main `bdf17c2b` with the accepted independent performance executor. Retain
  upstream fast defaults for every PR, release provenance and selection v2, and the owned
  fixture-copy permission repair. Preserve upstream ADR-0239 and move only the executor
  decision to ADR-0240. Regenerate descriptions and protected pins with unchanged generators;
  record focused non-root ordinary/read-only checks in the executor verification note.

Repair macOS performance-laboratory fixture setup by canonicalizing newly created temporary
roots, using explicit Python no-op executables, modeling the Linux mount host explicitly and
exercising log preallocation with real dense allocation where the platform lacks
`posix_fallocate`. Preserve strict symlink refusal, allocation-before-launch, log overflow,
absolute deadlines and cleanup ownership assertions; production behavior and test-class
selection remain unchanged. Anchor the terminal stall fixture’s simulated clock jump to its
recorded start so setup duration cannot consume its declared boundary interval twice.

Make the telemetry collection interruption fixture reach verification, authority expansion,
authority retention, pruning and history rendering independently of preparation cost. A scoped
test clock separates real preparation from the unchanged one-second work/cleanup deadline;
real timer interruption, incomplete terminal accounting, partial-import checks and lock release
remain asserted. Production deadlines, telemetry semantics and test-class selection are unchanged.

Integrate upstream telemetry runtime compatibility and cross-filesystem private AF fixtures
with class-selected validation. The real AF contained runtime smoke now runs for selected perf,
is mandatory for full validation and fails both stable aggregates on absent or unsuccessful
selected work; fast without perf records an intentional skip. Preserve upstream serial corpus
preparation, versioned CLI oracles, all contained security and timeout limits, and the existing
portability, Make build ordering and five-boundary interruption repairs. Retain runtime smoke
costs separately from class shares and regenerate protected harness identities.

- Reconcile four-class validation with bounded upstream through `91f75edb`, retaining profile
  v6 and symbol archive protections, the replacement calibration cohort, guarded experiment
  fixtures and README changes. Preserve label/release selection, both perf smoke dependencies,
  macOS and interruption repairs, and Rust-build-before-Python ordering. Renumber only unmerged
  feature ADRs and regenerate protected harness hashes. Focused local checks prepare an
  unverified candidate for operator publication and GitHub CI; no full local validation or
  independent review pass is claimed.

## [0.13.0] - 2026-10-26

### Authority compatibility

Committed .af/ policy valid under 0.13.0-rc.2 needs no hand edit. Run af onboard --refresh-lock to pin 0.13.0 and its signed bytes. State written by other releases remains unsupported under ADR-0145.

### Changes

- release: v0.13.0-rc.2 (#337)
- chore(deps): bump actions/setup-python from 7.0.0 to 7.1.0 in the actions-minor-and-patch group (#339)

## [0.13.0-rc.2] - 2026-10-25

`0.13.0-rc.1` was tagged but never published. Its release notes are carried forward into
`0.13.0-rc.2` below. The latest published signed release remains `0.12.0-rc.5`, so `.af/af.lock`
continues to pin it until `0.13.0-rc.2` is published, as required by
[ADR-0138](docs/adr/0138-the-repository-pins-its-newest-release.md).

### Authority compatibility

Existing committed .af/ policy that validates under 0.12.0-rc.5 needs no hand edit. Run af onboard --refresh-lock to pin 0.13.0-rc.2 and its signed bytes; a pipeline opting into recovery_attempts uses af.pipeline/3 and needs its refreshed lock.

### Changes

- release: v0.13.0-rc.1 (#335)
- fix(perf): show verdicts and reports in PR checks (#330)
- Isolate CLI usage test from release pin (#336)

Show actual performance verdicts in the PR check list and render each planned comparison's
coverage and rounded case table in its Details summary, including bootstrap reports when the
full primary is unavailable. Link the retained HTML report artifact and annotate verdicts
without changing advisory enforcement or granting a workflow write token.
Valid `no-perf` exemptions show `EXEMPT (no-perf; not measured)`; failed scope decisions
remain unavailable and cannot appear as an exemption or a measured pass.

- Isolate the CLI usage-error fixture from the repository release pin so prerelease source
  checks preserve exact JSON and stderr assertions while the signed pin awaits publication
  ([ADR-0213](../docs/adr/0213-isolate-cli-usage-tests-from-the-repository-release-pin.md)).

### Changes carried forward from unpublished 0.13.0-rc.1

- release: v0.12.0-rc.5 (#300)
- Fix every finding of the 2026-10-20 GA and security audit (#299)
- Fix clock-skewed warm Cargo targets after PR297 light review (#301)
- Integrate the continuous performance laboratory into the SDLC (#304)
- fix: sanitize missing review keys in terminal errors (#306)
- Fix hosted performance artifact layouts and offline CLI fixtures (#308)
- perf: complete hosted suite coverage and calibrate mixed CPU shards (#309)
- perf: predeclare hosted calibration for 23–29 October (#311)
- Telemetry P2–P5: recording, spans, storage kind, accounting, ledger, bench (#294)
- State the lines a pull request adds per directory, and check them in CI (#312)
- Telemetry P6–P7: harness streams, turns and tools, content blobs (#313)
- Give the af task report tests' body the lines block the check now requires (#314)
- Count a capped stream's gaps by the observer's runs, not by one chunking (#315)
- fix: supersede stale unavailable optimization gaps (#318)
- build(deps): bump the actions-minor-and-patch group across 1 directory with 2 updates (#253)
- perf(store): avoid redundant plan dependency decoding (#326)
- feat: skip PR performance measurements with no-perf label (#325)
- ci: use latest Python and model-based Task independence (#329)
- feat: disable Providers temporarily with automatic expiry (#334)
- GA polish: provider detection, first run, one outcome model, agent loop and kernel resilience (#332)

- Telemetry, second package: the commands that run a kernel run (`af task run`,
  `af task start --execute`, `af review run`, `af provider doctor`, `af self optimize --execute`)
  now record `af.command > af.run > af.authority.load, af.plan` (one `af/TelemetryNode@1` record
  per plan node), `af.node > af.attempt` with the kernel's exact accounting, and the run's
  `af.sweep`; every other command, and every command at `off`, opens no telemetry directory. From
  the first `af.run` until the seal one consumer thread is the segment's only writer and makes
  every record visible within `flush_latency_ms`; the command's end and the seal are one request
  to it, bounded at 2 s, after which it is abandoned and publishes nothing, leaving the segment
  for the next reader to recover. Every lease that begins kernel execution now commits an
  execution marker to its Task's log, the event `TaskExecutionStarted@1`
  (`af/TaskExecutionStarted@1`) beside `TaskTransition@5`, at every telemetry level. `af telemetry runs`, `show`, `tail` and `gc`
  (`--json`: `af/telemetry-runs@1`, `af/telemetry-show@1`, `af/telemetry-tail@1`,
  `af/telemetry-gc@1`) report a run's executions — numbered by those markers, active time as
  their union, execution time as their sum, elapsed time, history `prefix:k` or `partial`, never
  complete, the open ones named beside the sealed ones in every label and total, the outcome the
  latest execution's own, by ordinal — its span tree and its live tail, which ends only at a
  published seal or a recovery (at once for a run whose every execution was already sealed,
  its sealed records printed), recovers a crashed writer's segment itself, fails naming a
  sealed segment it cannot read, a live one with interior corruption or a followed one gone
  without a seal (the `.corrupt` file another reader's recovery left), exits 1 for an unknown
  run before following anything (naming the unreadable segment instead when one could hold
  it), refuses naming a `.corrupt` segment that held one of the run's executions or exists
  while the run cannot be found, also one renamed after it started, before it ever saw it
  (checked at every look and before it exits 0), and follows a run in one Store, refusing with `--store`
  guidance, once the followed Store's pending records are printed, a second Store that starts
  the same run id while it is followed, even in a segment sealed during its startup or between
  two of its looks.
  Telemetry is the kind `telemetry` of the Storage Budget (`af/storage@4`,
  `af/storage-prune@4`, the Project Store, Survey and claim kinds' `@3` documents extended, both
  planned by the one sweep plan): every registered directory counted once, one entry per segment by what
  its eviction frees, a sealed one last used at its seal time, evicted under ADR-0144's rules
  past `[telemetry] max_bytes` or the whole budget, every file of an entry, its manifest
  included, checked again before either is unlinked and the whole entry left in place, reported
  `changed`, when one is no longer what the inventory found, with temporaries, unreferenced blobs
  and free index pages reclaimed by every applied sweep and an overlapping directory reported
  `overlapping`, never evicted as telemetry.
- Telemetry, third package: every phase of a run is a span of the crate that owns it —
  `af.admission`, `af.context.compose`, `af.harness`, `af.output.parse` and `af.usage.account`
  under an Attempt, `af.gate` with its `af.check` spans and `af.warm.restore`/`af.warm.save`
  under a check node, and `af.snapshot.capture`, `af.cas.materialize`, `af.store.append`,
  `af.store.read`, `af.git`, `af.remote_check` and `af.process` wherever the run does them — each
  named in the catalogue in `docs/architecture.md`. Every supervised process now carries its
  process id and its CPU time and peak memory through every `review-process` result, a
  timed-out, killed or cancelled one included, and its span records them (`af.rusage.*`, absent
  when the platform did not deliver a figure, never zero), read safely from the children totals
  around each reap because the workspace forbids the `unsafe` a direct `wait4` needs — every
  child reap of `af` now goes through `review-process`'s one reap lock, so a figure is never
  another child's. A check
  records `af.check.status` and its comparability key `af.check.fingerprint`
  (`af/check-fingerprint@1`, the environment only as a digest with a runtime path a placeholder
  only where a path equals it or lies under it, a relative `PATH` entry resolved against the
  check's directory, `unknown` when a part is unavailable); a kernel measurement is a run of its own (`af.run.kind = measure`,
  `af.measurement.id`). At `spans` every run an execution works on has its own configuration
  record (`af/TelemetryConfig@2`, as many 16 KiB parts as it needs, each naming its run, an
  environment-origin value as `<env>`, a file under the home as `~/…`), its `configuration` and
  `machine` sections written when the run opens and its `plan` section when the plan resolves,
  with `af.config.text` naming the pipeline's `pipeline.toml`, the Task file and `.af/af.toml`
  by the SHA-256 of their own bytes and their size;
  `af telemetry config RUN [--execution ID]` assembles it, saying per section `complete`,
  `incomplete` or `unknown` (`--json`: `af/TelemetryConfig@2`).
- Telemetry, fourth package: every recording command installs one post-commit observer on its
  Stores, told of each committed transaction once, after its commit, in committed order — a
  generic append, a Task transition, a Round publication, an atomic integration — and never of a
  rollback; from `spans` on each committed event is `af.store.event` (type, sequence, id,
  causation, artifact references, the payload's digest and size), every Report a Campaign admits
  and every change of a summarized Finding field is an `af/TelemetryFinding@2` record (with the
  admitting Worker's bound model when the kernel recorded it) and a status change an
  `af.finding.disposition` event, and its Demands are `af/TelemetryDemand@2` records keyed by the
  Demand id, naming the Demand's own Subject and its Snapshot, with `af.demand.opened`,
  `af.demand.evidence_satisfied` (an admitted reuse that satisfies a stale Demand again
  included), `af.demand.stale` and `af.demand.waived`; every summary carries the committed
  sequence of the event that caused it. From `metrics` on every committed accounting transition records the
  Attempt's charge as the Task projection holds it (`af.usage.state`, by the committed event's
  sequence) and every sidecar write its merged row (`af.usage.wall`, ordered within one
  execution only); `af telemetry runs` now sums the Attempts' latest charges. A mirror failure is
  one problem, never a Store error, and makes the run `af.ledger.coverage = incomplete`. The new
  `af telemetry ledger RUN` (`--json`: `af/telemetry-ledger@1`) prints a run's committed events,
  Finding and Demand summaries and each Attempt's charge beside its sidecar observations, from
  the telemetry directory alone, each Store log in its committed sequence, with the coverage and
  history markers.
- Telemetry, fifth package: the performance contract of design §12 is measurable.
  `make telemetry-gate` builds a release `af` and runs `scripts/telemetry-overhead.py`: `off`
  against `spans` on three pinned, credential-free bench fixtures under
  `fixtures/telemetry-bench/` (four concurrent Attempts, one Attempt streaming 50 MiB, a Review
  appending Store events), 20 randomized pairs each with private homes, the paired overhead's
  median and 95% bootstrap interval per fixture and the dense fixture's drain latency, failing
  (exit 3) past a 2% median, a 3% upper bound or a 10 ms drain difference (`--json`:
  `af/telemetry-overhead@1`); `make check` runs only its one-pair smoke.
  `scripts/telemetry-corpus.py` generates the deterministic read-latency corpus
  `telemetry-corpus-10k` into a directory the operator names and measures `af telemetry status`
  (fresh and unchanged index) and `show` on it, warm and, where the page cache can be dropped,
  cold (`--json`: `af/telemetry-read-latency@1`); an unchanged `status` now opens each blob
  prefix directory once instead of once per object. `docs/telemetry.md` gains a `Performance`
  section with the protocol and this machine's first numbers.
- Telemetry, sixth package: every Claude and Codex Attempt's streams are tapped as they arrive.
  The supervisor's drain offers each chunk to a bounded queue (`[telemetry]
  observer_queue_bytes`) without ever waiting — a full queue drops it and counts it — so the
  process, its bytes, status, deadline, held output and accounting are unchanged, and from
  `spans` on the consumer records validated structure only, timed by arrival: each Claude
  `assistant` message and each Codex turn is `af.turn` with the harness's own per-turn usage and
  stop reason, a Claude message's turn ending as soon as its line is read (a further line of the
  same message is `af.turn.continued` on that turn, which `turns` folds into its row), each `tool_use` block or Codex command and MCP item is `af.tool` with its input and
  output bytes, the other lines are events, every harness identifier is a correlation id
  (`af.corr`) and every untrusted string only a size. A frame too long to judge is withheld
  whole, a dropped chunk is `af.stream.gap` and makes the stream incomplete, and once the
  process has ended Telemetry takes its own release decision over the complete streams and a
  Codex Attempt's `-o` final message, judged for every observed process whatever its ending —
  login-challenge material, a final message that cannot be read whole, a replaced capture or a
  stream the output cap truncated withholds everything pending (`af.stream.withheld`), a
  successful reply's included — before
  `af.harness.ids` lists the originals that pass the credential rules, continued in
  `af.harness.ids.part` events past what one record holds; an identifier past the 4096 a
  process pends or longer than 256 bytes has no mapping and is counted in
  `af.harness.ids.omitted`, with `af.harness.ids.analysis = incomplete`. A Claude Attempt asks for `--output-format
  stream-json --verbose` at `spans` and `content` and keeps `json` at `off` and `metrics`, the
  invocation-cost gate (`scripts/telemetry-invocation-cost.py`, `af/telemetry-invocation-cost@1`)
  having failed; `claude --help` must declare `--verbose`. The Store now keeps of a Claude
  Attempt's stdout only its `result` line (under `stream-json` the only envelope there is), and of a Codex Attempt's its turn and error lines
  beside the `-o` final message — never the native text around them, which classification
  reads in memory — so accounting, classification and raw evidence are the same at every
  level. `af telemetry turns` and `tools` (`--json`: `af/telemetry-turns@1`,
  `af/telemetry-tools@1`) print one row per turn or tool call with its timing, usage, outcome
  and sizes.
- Telemetry, seventh package: at `[telemetry] level = "content"`, and only there, Telemetry
  keeps content as blobs, each redacted whole before any truncation with its source's digest
  and size intact and named by one event: the composed Worker input of `af.context.compose`
  (`gen_ai.content.input`, `gen_ai.content.system`); once Telemetry's release decision withheld
  nothing, a harness's assistant text and Codex agent messages (`gen_ai.content.output`), its
  thinking blocks and reasoning items (`af.content.reasoning`, with `content_reasoning`) and its
  tool arguments and results (`af.content.tool_input`, `af.content.tool_output`, with
  `content_tool_io`); a check's stdout and stderr (`af.check.output`); every mirrored Store
  event's payload (published without waiting for the consumer: a busy consumer delays no Store
  append and no lease renewal; a released stream published under the seal renews the 2 s
  shutdown bound with each content, so a release that outlasts it completes); an admitted Finding's body (`af.finding.body`); and `.af/af.toml`, the
  pipeline's `pipeline.toml` and the Task file (`af.config.text`). A document `af` composed
  treats its identities and counters as typed only in the positions its kind declares — in a Worker
  request's input payloads only where the schema of an artifact type `af` owns declares one, the
  Task's requirements always judged whole. Past `content_attempt_max_bytes` for an Attempt or
  `content_process_max_bytes` outside one, further content is dropped and counted with one
  `budget` problem (a piece publication drops is never charged), and nothing about the Worker
  changes. The index (now `af/telemetry-index@3`; an earlier one is refused until `af telemetry
  reindex`) keeps the FTS5 table `content_fts` at ingest when `content_index` is on and empties
  it when it is off. `af telemetry context RUN ATTEMPT
  [--raw]`, `reasoning RUN ATTEMPT` and `search TEXT [--since D] [--run RUN]` (`--json`:
  `af/telemetry-context@1`, `af/telemetry-reasoning@1`, `af/telemetry-search@1`) read it back;
  `search` reads a bounded number of matches and shows an excerpt only of a blob it verified,
  listing a missing or corrupt one as unreadable; below `content` the first two say the content
  was not captured and `search` is refused. No
  prompt, Warm Set, Worker Notes or optimization source reads any of it.
- Telemetry measures its bytes the way the Storage Budget measures every other kind (ADR-0159
  clause 13): the allocated bytes of regular files, a hard-linked file once, directories and
  symbolic links nothing. `af telemetry status`, `af telemetry gc`, `af storage` and
  `af storage prune` now report the same telemetry numbers on ext4 as on APFS; on Linux they
  are smaller than before by 4096 bytes for every directory under the telemetry root, which
  they no longer count.
- Telemetry merged with the 2026-10-20 security audit: `[telemetry] max_bytes` is enforced, not
  advisory — the Storage Budget's sweep evicts sealed segments past it, and `af telemetry status`
  over the cap warns that the next storage sweep evicts them (its help, `af help telemetry`,
  `docs/telemetry.md` and `schemas/telemetry-status-v1.json` say the same, and name no overhead
  gate). `af/storage-prune@4` carries the audit's `left` (what a removal left in place in a Store
  not proven af's) beside `telemetry`; `af/storage-prune@3` stays superseded and ships no
  schema. Every `review-process` result carries the capture cap's `truncation` beside the
  leader's process id and resource use, a check, an Attempt, a container command or a gate call
  past the cap fails naming it inside its `af.check`, `af.harness`, `af.process` or `af.git`
  span, which ends after that judgement: a capture rejected for its cap ends its process span as
  failed (`error.type = "failed"`) with the cap reason, whatever the exit code, its stream sizes
  and resource use still recorded, and the Linux subreaper's reaps of escaped descendants take the same reap lock. At
  `metrics` a recording command's segment header keeps `argv[0]` and the subcommand chain the
  argument parser matched, which `af task run` and the other recording commands now pass in.
- A blob published again is renamed over its existing file, so the file's age restarts with the
  publication: a sweep that runs while the new reference is still buffered — a detached Store
  mirror publication's, or an attached one's past the bounded wait — and evicts the segment that
  referenced the file before no longer collects it as an orphan before the reference is written.

- Security audit 2026-10-20, state, storage and policy parsing (H2, M6, M5, L10, L14, L4, I7). An
  explicit `--state` directory must be absent, empty or already an af Store: one holding anything
  else is refused before anything is written or registered, and a new Store carries the owner
  marker `.af-store`. The sweep, `af storage prune --apply`, `af review gc --state-root` and
  `af self uninstall --purge` remove a Store outside af's roots whole only while it is marked and
  holds nothing but af's entries, each known by its exact name (`notes.lock`, `backup.lock` or
  `events.sqlite.notes` is never af's), and a removal claim only when its name
  (`.af-removing-<pid>-<device>-<inode>`, as removals now name it) binds the very directory it
  holds, so a user's `.af-removing-1-2-3` is kept, reported and not measured, and the sweep
  finishes only such proven claims; otherwise they remove af's entries alone and name what they
  left (`left` in `af/storage-prune@4`), and such a Store counts only af's entries against the
  budget. af's state and telemetry directories are created `0700` and its files there `0600`
  (the SQLite `-wal` and `-shm` included) whatever the umask, and an open af-owned root is
  narrowed to `0700`; the checkout keeps the user's umask. Route patterns match in
  O(pattern × path) and are capped at 1024 bytes and 64 wildcards. Authority files are read
  by the path as named, never canonicalised first, without following a final link and within
  1 MiB each: a linked configuration layer is refused in one line, and a project's `.af/af.lock`
  that dispatch or the remembered pins read is refused, in one line on stderr, when it is a link
  or over 1 MiB, Worker packages within 256 files and 8 MiB,
  and a parse error quotes at most 200 bytes of its line. A Task cancelled or abandoned after its
  gate published is listed and cleaned up like a finished one. A Campaign label is at most 128
  bytes and refuses format, bidi-control and separator characters
  ([ADR-0144](docs/adr/0144-hold-afs-disk-use-to-a-machine-budget.md),
  [ADR-0035](docs/adr/0035-address-campaign-state-by-opaque-id.md)). Outside this package's
  files, minimal edits: the `--state` help in `crates/af/src/cli.rs`, the Task Store creation
  and `--state` admission in `crates/af/src/task_execution.rs`, the private `git-home/` in
  `crates/af/src/task.rs`, and the `nix` features of `review-store` and `review-telemetry`.

- Security audit package D (process capture, containers, Worker environments, the remote gate).
  Each captured stdout and stderr of a check, a Worker or a container command is kept to 64 MiB
  (`review_process::MAX_CAPTURED_STREAM_BYTES`); the rest is read and discarded so the child never
  stalls, the total is recorded, and a check whose output passed the cap fails with a reason
  naming it, as does a Worker Attempt. On Linux af is a child subreaper, and a shell Attempt or a
  local check ends every descendant that left its process group (`setsid`, or a `set -m` job in
  another group of the same session) but kept the `AF_PROCESS_TREE` value it inherited from its
  leader, before its tree is sealed: that value alone, never how many runs are live, tells it
  from any concurrent run's, so a run never ends a process another live run started; when the
  subreaper cannot be set, af warns once on stderr and every check (`reason`) and Attempt
  (stored stderr) records that descendant cleanup fell back to the process-group kill. A
  descendant that clears or replaces that variable and leaves its process group can still
  outlive the run under `trusted_local`, on Linux as on macOS, where any such descendant can;
  `trusted_local` is no security boundary, and untrusted code belongs in a container
  ([ADR-0118](docs/adr/0118-let-review-workers-execute-checks-in-an-ephemeral-clone.md),
  [ADR-0120](docs/adr/0120-give-a-source-writing-worker-a-shell.md), amended). Container checks
  run with `--pids-limit 4096`, `--cap-drop=ALL`, `--security-opt=no-new-privileges` and
  `--init`, and with `--memory`/`--memory-swap` and `--cpus` when the `[gate]` binding sets the
  new optional `memory` and `cpus` keys. Attempts and local checks get a `PATH` without empty or
  relative entries; a Codex Attempt's `HOME` is a private empty directory, not the candidate
  tree; a Claude Worker's `CLAUDE_CONFIG_DIR` is redacted from stored output like `CODEX_HOME`,
  in its canonical spelling too. The Remote Check gate streams each blob into `git fast-import`,
  runs `git` and `gh` from an allowlisted environment (no `GIT_CONFIG_COUNT`, `GH_HOST` or
  `GH_TOKEN`), calls `gh api --hostname github.com`, redacts any URL's user information, also
  refuses a candidate that changes `.github` itself, `.gitmodules`, `.cargo/` or a root
  `action.yml`/`action.yaml`, and cleans up through the mapping entry of the Task's
  `repository_id`; the docs now say the workflow *file* is protected and everything it runs is
  candidate code
  ([ADR-0140](docs/adr/0140-run-a-declared-check-through-a-gate-pull-request.md), amended). Two
  edits outside this package were unavoidable: the `[gate]` keys in `review-config` (with the
  struct literals in `af/src/onboard.rs` and the Integration tests, the two provider sites in
  `review-pipeline/src/review_domain` and the container Gate argv test beside them), and the
  cleanup lookup in `af/src/storage/gate.rs`.

- Self-management, the installer and the release tooling are hardened after the 2026-10-20
  security audit. An install a project's `.af/af.lock` asks for now needs the archive to match
  both the lock's digest and the release's signed `SHA256SUMS` (receipt
  `verified_by = "lock+signature"`); a release without signature material is refused under a lock
  as outside one. `[self] install_pins` is `"ask"` (the new default), `"always"` or `"never"`
  (`true` and `false` still mean `"always"` and `"never"`): under `"ask"` a pin that is not
  installed is installed only after a terminal confirmation naming the version, the lock, and
  whether the release is older than the running `af` or a pre-release; without a terminal nothing
  is installed and an older pin's command runs under the current `af` after a notice naming
  `af self install <version>`. `af review plan` never installs. `af self update` without
  `--version` refuses a listed newest that is older than the running release. Releases of a public
  repository are listed and downloaded anonymously over HTTPS with `curl` (pinned to HTTPS and
  TLS 1.2), so updates work without `gh`, which is used only when that is refused, and judged by
  the call it makes rather than by `gh auth status`. `af self uninstall` leaves
  `$XDG_BIN_HOME/af` in place unless it is af's link into its versions, and `af self remove` and
  `prune` delete through the same descriptor-based removal as uninstall, never following a link.
  `install.sh` pins `curl` to HTTPS, refuses without `minisign` unless
  `AF_INSTALL_ALLOW_UNSIGNED=1` (saying so on stderr), and verifies an installed binary again on a
  re-run. The PR-report check exempts a `release/` branch only from this repository itself, never
  a fork's. The release workflow installs `cargo-zigbuild` and `ziglang` from
  `scripts/release-toolchain.txt` with `--require-hashes --only-binary=:all:` (hashes of every
  published wheel, taken from PyPI), and a weekly, on-demand `cargo deny check advisories bans
  sources` workflow with `deny.toml` gates advisories, duplicate crates and sources.
  `SECURITY.md` states what the update check sends and the pinned projects `self.toml` keeps,
  and the release docs the `pin` job's blast radius
  ([ADR-0044](docs/adr/0044-af-manages-itself-and-dispatches-to-the-pinned-release.md),
  [ADR-0045](docs/adr/0045-one-release-train-and-a-pin-that-binds-bytes.md),
  [ADR-0142](docs/adr/0142-carry-the-af-task-report-in-every-pull-request.md),
  [ADR-0152](docs/adr/0152-push-the-release-pin-with-a-deploy-key-that-bypasses-mains-ruleset.md),
  amended in place). Outside this package's own files, the `af help self` text in
  `crates/af/src/topics.rs`, the `af self` and `af onboard` help in `crates/af/src/cli.rs`, and the
  TUI settings golden (`install_pins = "ask"`) change with the new default.

- Bare `af` no longer runs `git diff` against the working tree or `git hash-object` to mark
  drifted Pipelines and Workers. Both could run an fsmonitor or a clean filter that a checkout's
  own `.git/config` names. The browser now takes the committed blob ids from `ls-tree`, hashes
  each working-tree file in-process as git hashes a blob (`blob <len>\0`, then the bytes,
  streamed through one 64 KiB buffer), and compares the ids, so its drift check never holds a
  whole blob or a whole working-tree file. Every
  git call the browser makes, and the Task file advice's `git check-ignore`, now runs with
  capture's cleared environment and full `-c` override set
  (`review_source_git::git::isolated_command`). That advice still reads the global excludes file
  at git's default place
  ([ADR-0122](docs/adr/0122-show-committed-workers-and-their-recorded-attempts.md),
  [ADR-0115](docs/adr/0115-declare-the-af-layout-and-keep-task-files-out-of-git.md), amended).
- A tree that names a path git itself refuses is refused, with an error naming the path, at
  capture, materialization and Task binding. One shared predicate (`git_refuses_path`) covers
  `.git` in any ASCII case, its NTFS and HFS+ spellings (`.git.`, `.git` with a trailing space,
  `git~1`, HFS-ignorable code points) and a symlinked `.gitmodules`. On a sandbox volume that
  folds case or Unicode normalisation (probed once per root), materialization refuses two paths
  that would name one file, naming both. Committed capture streams `cat-file --batch` bodies
  through one 64 KiB buffer and no longer allocates a blob whole. Stored bytes and digests are
  unchanged
  ([ADR-0020](docs/adr/0020-stream-cas-materialization-and-clone-duplicates.md), amended).
  `review-source-git` gains the `unicode-normalization` dependency, already in `Cargo.lock`.
- A tree entry with a legacy regular-file mode such as `100664` is captured and reviewed as a
  file, executable when its owner bit is set, instead of failing the review.
- Text output of `af review plan`, `ledger`, `show`, `report` (text and Markdown), `render` and
  `run` shows every control character in a changed path, finding title or other repository- or
  model-authored string (pipeline node IDs, edges, reservation, input and provider lines of
  `plan`, Proposal paths and report JSON lines of `show`, the selectors and node of `render`) as
  a visible `\u{…}` escape. ESC, BEL and newlines no longer reach the
  terminal raw. `--json` output is unchanged.

- Security audit fixes (redaction, provider login, telemetry): the shared credential heuristic now
  also withholds a URL carrying a password or an `http(s)` user, `password <value>` after whitespace
  (`--password x`, `.netrc` lines), `mysql -u … -p…`, AWS access key ids, device codes beside
  `code`, `/login/device`, one-time codes and `#code=`, `Bearer: …`, JSON Web Tokens with a `+` or a
  short signature, hex runs of 20 or more mixing letters and digits, lower-case runs of 40 or more,
  the account a login names, and two long words that are one secret split by a space; a `sha256:`
  digest and a 40- or 64-hex git object id are no longer withheld. JSON values are judged whole
  whatever their member's name (`pw` now counts as a secret name), and a granted path is replaced in
  its home-relative, canonical, JSON-escaped, percent-encoded and non-normalised spellings, the
  last wherever the path starts (after `=`, `:`, a quote, a bracket or whitespace); blobs
  record `af-redact/2`. Provider login processes disable core dumps (non-dumpable on Linux) and zero
  the challenge and the returned code when dropped (new dependency: `zeroize`); every buffer of
  the private-host transport that holds a challenge or a code (the serialized frame, the read
  buffer and the pending input, each allocated once at its bound) is zeroed on every exit path. The terminal-only
  login is documented as protection against accidental capture, with a pseudo-terminal caller out of
  scope and a test pinning it ([ADR-0112](docs/adr/0112-refuse-agent-mediated-provider-logins.md),
  amended). At telemetry level `metrics` the segment header keeps only `argv[0]` and the subcommand
  path its caller's argument parser matched (`Start::command`; never guessed from leading
  lower-case words, so `af provider setup alice` records `provider setup`; `argv[0]` alone when
  none is passed in) and no working directory; `-p` and `--pw` values are withheld at `spans`. `sink = "otlp"`,
  `sink_endpoint` and `sink_headers_file` are refused at load as not implemented; `[telemetry]
  max_bytes` is documented as the cap the storage sweep enforces and `af telemetry status` warns
  when over it that the next sweep evicts; its `--json`
  paths (and `reindex --json`'s) are `~`-prefixed like the text output; no help or status text names
  an overhead gate; `docs/security/containment-probes.md` records the green `container-probes` job
  and closes live Gate routing. Fixtures outside this package's files changed only where they pinned
  a 40-hex run or a bare digest as a secret (`review-runner` CLI tests, the Claude and Codex
  auth-failure tests, one `record.rs` test), plus the `af telemetry` text output, the `af telemetry
  status` help in `cli.rs` and the `nix` features in `crates/af/Cargo.toml`.

- Native Task checks and measurements now refresh the mtimes of regular files in their private
  source trees before running. Reusing a warm Cargo target across Snapshots, including an
  A→B→A reversion, rebuilds workspace crates from the current Snapshot while keeping registry
  dependencies warm. Sources are ordered after the held target's actual mtimes without a future
  lead; future-dated targets run cold while Cargo home stays warm. Repeated Cargo invocations
  within a check reuse the units just built. Warm diagnostics still report available target bytes rather than a
  compiler cache hit or a previously built Snapshot (#266,
  [ADR-0161](docs/adr/0161-refresh-native-check-source-mtimes-before-reusing-cargo-target.md)).

- Performance laboratory P10 repair. The kernel receipts reader (`af.perf-kernel-receipts/3`,
  shape and schema unchanged) now refuses a Task result whose `evidence` or
  `missing_obligations` is not strictly ascending in the kernel's string order, as `unique_set`
  does when the kernel deserializes it. Before, it accepted the root's reversed evidence. An
  output's `artifact_ids` stays valid in any order. Four generated fixtures cover an unsorted
  and a duplicated entry in each set. A Rust test also checks eleven variants built at test
  time, and the laboratory's reader and `review_core`'s own deserialization agree on every one.
  The retained evidence is byte-exact. `checks/p10-repair-reconciliation.json` reconciles the
  current source with the measured commit through the earlier repair's record, using the new
  `fingerprint.py rerepair`
  ([ADR-0189](docs/adr/0189-read-a-task-result-s-sets-in-the-kernel-s-order-and-reconcile-a-second-repair-through-the-first.md)).

- Activation closeout of the performance laboratory. A new `Performance activation` workflow
  verifies a proposed activation with the base revision's own code and a read-only token
  scoped to that one step. When the manifest or a record it names changed, it runs `cli.py
  activate --verify-proposed`, which recomputes readiness and a bridge's evidence from the
  retained Actions evidence. An unchanged activation is verified locally, and `--fetch` is gone.
  Active plans carry their calibrated CPU/image groups. A run measured on any other host cannot
  satisfy the required check (`uncalibrated_cpu_group`). An equivalent-adapter bridge measures
  as active only with verified executions: a golden check of both product sources and a
  post-merge A/A, recorded by the new `cli.py equivalence-evidence`. Its protected diff now
  covers lock files, Cargo manifests, build scripts, the toolchain and Cargo configuration.
  Readiness binds each original's head commit and refuses another producer. It names
  history gaps from the listed numbers alone. Capped or inconsistent listings are refused.
  `cost-report` reads each run's cache from its build manifest, keeps failed attempts and needs
  five distinct completed comparisons per group. `shipping-report` refuses repeated runs and
  other policies, and keeps failed or missing samples visible. No real cohort, activation,
  bridge or demonstration has run (`af.perf-run/6`, `af.perf-readiness/5`,
  `af.perf-activation/3`, `af.perf-equivalence/1`, `af.perf-activation-verification/2`,
  `af.perf-cost/2`, `af.perf-shipping/2`;
  [ADR-0184](docs/adr/0184-verify-proposed-activations-in-trusted-ci-and-bind-bridges-groups-and-costs-to-retained-executions.md)).

- Calibration readiness now detects a deleted original. A cohort records the performance
  workflow's run number when it is declared, and readiness requires every later run number in
  the history it reads, so a deleted first dispatch refuses rather than letting a later one
  stand in. Readiness also counts only successful original attempts. It recomputes A0, loop costs
  and counts from the A/A and preparation originals and refuses other sealed parameters. It
  invalidates a member with two artifacts of one expected name and requires its measuring jobs on
  fresh hosted runners. An impossible timestamp is invalid evidence, not a traceback. New
  `cli.py activate --verify` verifies an activation manifest: every active plan rehashes its
  cohort, parameter and readiness records and recomputes the decision, and `--fetch` recomputes
  readiness from the cohort's retained Actions evidence. A full plan outside the calibrated
  environment and recipe is unavailable. An equivalent-adapter admission may change only its
  named adapter files and registry entries, and owner approval, golden checks and a post-merge
  A/A stay pending. New `cli.py cost-report` and `shipping-report` compute the cost and
  shipping-budget demonstrations from retained runs. No real cohort, activation or demonstration
  has run yet (`af.perf-cohort/4`, `af.perf-readiness/4`, `af.perf-activation/2`,
  `af.perf-activation-verification/1`, `af.perf-cost/1`, `af.perf-shipping/1`;
  [ADR-0183](docs/adr/0183-detect-deleted-originals-recompute-parameters-and-verify-activations-from-retained-evidence.md)).

- Calibration readiness now binds each cohort member to its first dispatch rather than to
  whichever of its artifacts can still be read. The performance workflow names a calibration
  run for its cohort's digest and member as the run is created. `cli.py readiness` and
  `calibration-prepare` read the complete workflow-run history and that run's attempt-1 record,
  so an original that failed, expired or became unreadable keeps its place and no later
  dispatch replaces it. Each member's plan must also be exactly the plan its authority freezes
  for it again (`--repo`). Also fixed: the authority probe gives each setup write a lease of its
  own and times its region through a fresh, cold authority. Three real-time tests run alone,
  and `make lint` now builds and runs the calibration builds of the probe and `af`
  (`af.perf-cohort/3`, `af.perf-readiness/3`;
  [ADR-0182](docs/adr/0182-bind-each-calibration-member-to-its-first-dispatch-and-its-authoritys-plan.md)).

- Calibration cohorts can now be declared, run and judged. `cli.py cohort-declare` fixes a
  cohort's logical members, seeds, days and CPU/image groups before any member runs; GitHub's
  run IDs are recorded beside it as provenance, so its bytes never change. Preparation and
  slowdown members build both sides with the default-off `perf-calibration` feature, and only
  the candidate runs a deterministic CPU loop inside its timed operation. Shipped builds refuse
  the loop's control. `cli.py calibration-prepare` seals A0 and the loop counts before any
  trial. `cli.py readiness` recomputes readiness from each member's original retained run. It
  refuses reruns and replacements and requires all 20 A/A members within tolerance and 9 of 10
  power trials valid in every critical case of the same trial. No real cohort has run yet.
  Also fixed: an unreadable or inconsistent claim from the trusted release handoff now stops a
  release measurement instead of allowing a second one. The authority probe's timed region takes
  a fresh lease of its own after setup. The supervisor kills again a process that a single group kill
  missed while it was being forked (`af.perf-run/5`, `af.perf-cohort/2`,
  `af.perf-calibration-parameters/2`, `af.perf-readiness/2`;
  [ADR-0181](docs/adr/0181-declare-cohorts-before-they-run-inject-only-the-candidate-and-count-whole-power-trials.md)).

- Published releases are now measured. `performance-release.yml` runs when a release run
  completes. Its read-only handoff checks that the run is this repository's `release.yml`,
  that the release is published under its exact tag and SHA, and that `sign and publish`
  succeeded in some attempt, so a later failed pin retry still counts. It claims the release's
  upstream run, tag and SHA once, before any sample, and measures only the first valid claim.
  A later run of the same release links that claim's result, and a claim without a result is
  reported incomplete and never measured again on its own; only a `remeasure_of` dispatch
  measures again, keeping the parent claim. The measurement calls the token-free
  `performance.yml` and stays advisory. A CLI trace that outgrows its evidence room is now
  always `trace_over_cap`, also when the watch cancelled the command while it was still
  running (`af.perf-schedule/2`, `af.perf-release-claim/1`, `af.perf-release-handoff/1`;
  [ADR-0180](docs/adr/0180-hand-off-published-releases-read-only-and-measure-each-claimed-identity-once.md)).

- Guarded kernel performance experiments. The new `cli.py measure --case ID --metric KEY` is
  one repetition of a kernel `measure`: it builds the native probe offline into the kernel's
  Cargo target, runs one golden-judged nonspawning library sample and prints one
  `af.measure-report/1` line, or exits nonzero with no report. Before importing the driver or
  building anything, the literal isolated bootstrap of the staged policy verifies the whole
  protected harness against the digest the captured policy holds. The harness is the driver,
  fixtures, probe, adapters, trace and calibration helpers, and every manifest, lock, build
  script, toolchain pin and Cargo configuration, with additions. The bootstrap refuses links and
  stray bytecode. `fixtures/performance/kernel-measure/` stages the measures for a reviewed
  human installation; `cli.py harness-digest --check` runs in `make check` and release
  preparation, `--write` re-pins. Objectives compare `elapsed_ms` until the guard is accepted. A
  real kernel integration proves tampered candidates never reach an improved receipt. A warm
  target never measures another revision's product. Costs count every retained execution, fail
  any with a failed sample, timeout or unhealthy sentinel, and observe real cold or primed warm
  compiled caches. Shipping keeps one target per binary and host group, with median and p95, and
  never demonstrates from unhealthy runs. A bridge's protected diff covers the instrument helpers,
  and fetched bridge runs are bound to the Performance workflow, attempt and hosted jobs
  (`af.perf-builds/6`, `af.perf-cost/3`, `af.perf-shipping/3`,
  `af.perf-activation-verification/3`, `af.perf-kernel-inventory/1`;
  [ADR-0185](docs/adr/0185-measure-guarded-kernel-experiments-through-a-literal-bootstrap-over-a-protected-harness.md)).

- Performance laboratory p8 closeout. The kernel receipts reader (`af.perf-kernel-receipts/3`)
  now binds each retained baseline, candidate and comparison to the artifact, type and Snapshot
  its Task's result names, and refuses a retained output the result does not name or a named one
  that is missing, as a failed Task leaves it. It recomputes every envelope's content and
  artifact IDs and every whole stream's ID with the kernel's canonical JSON and domains, so a
  changed run under its original IDs is refused. A stream cut to its first and last 32 KiB says
  so, and its omitted middle is never claimed rehashed; `/2` is refused. Reports list only failed
  or incomplete cases in their first view, keep passing improvements and non-regressions in the
  Cases table, and say every case passed, by status, instead of calling an improvement within
  tolerance. A measured run with a missing or failed slot states how many planned records it
  holds and what is missing or failed, never that every pair ran. ADR-0186 no longer asserts an
  unmeasured hashing time. `docs/performance-evidence/p8-local/` retains one real same-source run
  of the documented workflow on a macOS arm64 host, from the frozen plan and offline build to the
  report, its linked phase-trace child, archive, history, exact reproduction and link checks; it
  is functional local evidence, not a product speed claim
  ([ADR-0187](docs/adr/0187-bind-kernel-receipts-to-their-task-results-word-reports-by-what-was-admitted-and-retain-a-real-local-run.md)).

- Performance laboratory acceptance repairs. The guarded kernel measure's bootstrap now protects
  case variants of build inputs, such as `BUILD.RS` and `.CaRgO/config.toml`, that a
  case-insensitive filesystem hands to Cargo. A warm Cargo target is keyed by the content of the
  whole measured Snapshot, so a source below a directory named `target` or a file a package
  includes from outside its member is never measured stale. The probe's output is bounded while
  it is read: one that outgrows its bound or timeout is killed, a held stream fails, and the
  build log and failure tail are capped. A fetched bridge run must be exactly the plan the
  bridged authority freezes for its dispatch, so a widened tolerance or another changed frozen
  field is refused. The native kernel integration adds a case-variant tampered candidate. It
  exports schema-checked `af.perf-kernel-receipts/2` receipts with each output's envelope and
  every repetition's bounded public streams, and runs alone because it asserts real elapsed
  time. Test helpers keep an `af` process's real exit status and signal instead of `-1`.
  Reports lead with the verdict and the affected cases' baseline, candidate, absolute and
  relative change, keep exact statistics and provenance in expandable details, and word
  synthetic, measured, mixed and empty evidence by what it is. `docs/performance-acceptance.md`
  records what is implemented, unavailable and still pending
  ([ADR-0186](docs/adr/0186-fold-protected-paths-key-warm-targets-by-the-whole-snapshot-bound-measure-output-and-replan-bridge-runs.md)).

- Performance laboratory P9 repairs. The kernel receipts reader (`af.perf-kernel-receipts/3`,
  shape and schema unchanged) now refuses what the kernel itself refuses. That covers a blank
  producer run, node or operation (blank as Rust's `trim` defines it), a blank domain
  conclusion, satisfied acceptance without completed execution, evidence and outputs, an empty
  or over-cardinality output binding, and a retained stream part longer than 32 KiB once
  decoded, in either encoding. Seven self-consistent invalid fixtures each break one rule, and
  a Rust test deserializes them into `review_core`'s own types to confirm the refusals. The
  retained receipts are still admitted. The p8 local workflow's snapshot stage no longer
  inherits a checkout's `.git`, and every stage stops at its first failure. The retained
  history now holds the original run's page, regenerated byte for byte from its retained
  bundle. Its links are checked in the exported directory itself, and the original measured
  fingerprints are unchanged
  ([ADR-0188](docs/adr/0188-validate-kernel-receipts-as-the-kernel-does-and-restore-the-retained-history.md)).

- The performance laboratory's first package: a reviewed registry of the design's initial cases
  (`fixtures/performance/suite.toml`), closed versioned evidence contracts with generated
  schemas and golden fixtures, and `python3 scripts/performance/cli.py` `list`, `plan` and
  `compare`. `plan` resolves baselines and the measuring authority and freezes the seeded paired
  schedule; `compare` recomputes an exact `paired_median_tolerance_v2` verdict from retained
  evidence and refuses missing, duplicate or tampered records. Nothing executes a workload yet,
  and full plans stay unavailable until a calibrated activation exists
  ([ADR-0162](docs/adr/0162-freeze-performance-plans-and-judge-them-by-exact-paired-verdicts.md)).

- The performance laboratory's native probe: `crates/review-pipeline/examples/perf-probe.rs`
  times each library case's real production operation (canonical serialize and validate, CAS raw
  and typed reads, Git source capture of 100 and 10,000 files, cold and prewarmed projection of
  exactly 10,000 Task events, the current-plan check and first `TaskRuntime::new` at both issue
  #255 package sizes, and an advisory persistence round trip) on fixtures it generates through the
  product's own writers, with setup timed apart. Independent golden entries
  (`fixtures/performance/golden/native.json`) come from a reference generator that never runs the
  product, and `python3 scripts/performance/cli.py probe` and `judge` check each side against
  them. The registry is now `af.perf-suite/2`; its `/1` is refused. Nothing runs paired
  comparisons yet
  ([ADR-0163](docs/adr/0163-measure-native-workloads-through-a-public-api-probe-and-independent-golden-oracles.md)).

- The performance laboratory's `compare` now refuses evidence it used to pass: a missing advisory
  or sentinel record, a shard whose terminal record says failed or interrupted, records out of
  planned order, and builds whose recorded recipe settings or Cargo.lock differ from the frozen
  recipe and the digests `plan` now reads from each source's Git objects. A missed enabled budget
  fails a complete healthy case even when its interval is inconclusive. The plan, build and
  verdict contracts are now `af.perf-run/2`, `af.perf-builds/2` and `af.perf-verdict/2`, and their
  `/1` documents are refused. The required `lint` check now fails unless the laboratory's Python
  3.9 and 3.11+ legs succeed
  ([ADR-0162](docs/adr/0162-freeze-performance-plans-and-judge-them-by-exact-paired-verdicts.md)).

- The performance laboratory now writes the source capture and state projection fixtures once
  per side and shard through each side's own probe. Each writer's check is judged against its
  own golden entry before the fixture is sealed, and every sample copies it privately as
  untimed setup. How a recipe builds, its image included, is frozen in the plan's recipe
  identity (`cli.py plan --build-image` or `--trusted-host-build`), and a build never chooses
  another image. A `name@sha256:` image is compared by its full reference. The `review ledger`
  and `review report` oracles check every declared Finding and a findings summary consistent
  with them. The supervisor's host backstop covers a sample's setup as well as its child. The
  whole run is bounded by one execution start and total deadline, recorded when its build
  begins. The plan, build, shard, probe request, golden, CLI and registry contracts are now
  `af.perf-run/3`, `af.perf-builds/5`, `af.perf-shard/4`, `af.perf-probe-request/2`,
  `af.perf-golden/2`, `af.perf-cli/3` and `af.perf-suite/3`, and earlier versions are refused
  ([ADR-0170](docs/adr/0170-seed-native-fixtures-freeze-the-build-image-and-bound-the-run-by-one-deadline.md)).

- The performance laboratory no longer kills only a sample's supervisor when its backstop or
  the run's total deadline expires. The supervisor is sent SIGTERM and cancels the sample
  through the same supervision: a running child or setup is ended and verified gone, and a
  child not yet launched never is. A launch the deadline could not cancel in time is not made.
  When nothing confirms that a sample ended, the shard stops (`cleanup_uncertain`). It then
  keeps the sample's private scratch and the shard's seeds for recovery and names them in
  `shard.json`. A diagnostic `cli.py probe` keeps its scratch the same way. The run's total
  deadline now also bounds a shard's preflight: each Git read of the authority, the container
  runtime query and the scratch enforcement fill ends 30 s before it. A step the deadline ended
  refuses the shard as `run_deadline`, and no later step starts. The `task show` oracle now
  requires the Task history to be numbered exactly 0..9999, each once and in order, over the
  whole output, which the supervisor observes past any retained log. The supervisor request,
  supervisor result and CLI registry contracts are now `af.perf-supervisor-request/3`,
  `af.perf-supervisor-result/4` and `af.perf-cli/4`, and earlier versions are refused
  ([ADR-0171](docs/adr/0171-cancel-samples-through-their-supervisor-carry-the-deadline-through-preflight-and-observe-history-order.md)).

- The performance laboratory's native probe driver now refuses what it used to pass or damage:
  `cli.py judge` refuses a recorded request whose case, contract, parameters or repetitions
  differ from the registry, `probe` and `judge` refuse a golden document the reference generator
  does not derive, a persistence sample without exact `logical_bytes` fails, and an explicit
  `probe --timeout` outside 1 to 120 seconds launches nothing. The driver bounds a probe's
  stdout and stderr while reading them and kills and reaps the direct child on overflow or
  timeout; the probe removes `probe-work` only while it is the directory it created, and the
  state persistence sample refreshes its production lease immediately before its region
  ([ADR-0164](docs/adr/0164-judge-only-declared-probe-requests-against-a-reference-checked-golden.md)).

- The native performance probe no longer removes anything: it creates `probe-work` in the
  scratch it is given and leaves it for its caller. `cli.py probe` runs each sample in a private
  0700 namespace that it holds by a directory handle, below a `--scratch` parent owned by you or
  root and not writable by others unless sticky. It removes the namespace only through that
  handle, confirms the removal, and fails the sample when cleanup is not confirmed; it no longer
  ignores cleanup errors. A lease-bound sample is `lease_expired` only when the Store reports the
  lease expired or fenced. A SQLite, decoding or clock error keeps the operation's own failure,
  with both diagnostics. The state persistence sample finishes its Store reads before the lease
  refresh that starts its region
  ([ADR-0165](docs/adr/0165-own-probe-scratch-in-the-driver-and-take-lease-expiry-only-from-the-store.md)).

- The performance supervisor bounds every wait. If it cannot observe a child's exit without
  reaping it, it still enforces the deadline and cancellation and reports the exit as
  `unobserved` or reaped by polling, with no timed interval. A failed process enumeration, reap
  or kill makes the cleanup uncertain and keeps the original error; it is never `clean`. The
  shared `review_process::sample` refuses PID-namespace containment unless it runs as a
  namespace's PID 1. The supervisor result is now `af.perf-supervisor-result/2` and the
  capability report `af.perf-capabilities/2`. `cli.py capabilities --verify-scratch-enforcement`
  verifies a scratch bound only by the filesystem's own ENOSPC or EDQUOT after progress, with
  the filesystem then reporting itself full; I/O and permission errors, no progress, the fill's
  time bound and a failed cleanup refuse it. `cargo test --test perf_supervisor` builds the
  example it tests. On macOS, cache metadata normalization no longer goes through `/dev/fd`,
  whose lookup could fail intermittently when many processes ran at once
  ([ADR-0167](docs/adr/0167-bound-every-supervisor-wait-and-verify-only-what-was-observed.md)).

- The performance laboratory builds and supervises its samples. `cli.py build --plan run.json`
  builds a frozen plan's two products and one authority-built `perf-supervisor` from detached
  worktrees under `target/performance/<run>/`, using a credential-free private repository,
  locked offline builds and an allowlisted environment. It verifies each AF binary's embedded
  commit, overlays the authority probe from `fixtures/performance/adapters.toml` in fixed
  priority order, and writes `builds.json` (`af.perf-builds/3`) beside hashed, read-only
  artifacts. The supervisor times each child from launch to its observed exit, drains capped
  output, forwards termination signals and establishes descendant lifecycle. In a Linux sample
  container it is PID 1 and kills and verifies every escapee. `cli.py capabilities` reports what
  the host can run and why. `cli.py probe` now needs `--trusted-local`, runs under the
  supervisor, refuses spawning cases and removes its scratch only through verified tombstones.
  A failed sample keeps its original diagnostic ahead of a bounded cleanup note
  ([ADR-0166](docs/adr/0166-build-frozen-plans-and-supervise-samples-with-one-authority-instrument.md)).

- The performance laboratory's shards and scratch enforcement fills take one machine lock,
  `/tmp/af-perf-machine.lock`, before touching anything shared. It does not depend on `TMPDIR`
  or `HOME`, and is held until the shard's terminal record. `review ledger` and
  `review report` are now measured over a 10,000-event Campaign: each side's own
  `af review run` writes it, and that side's Findings Ledger writer tops it up. Large Task and
  Campaign state is written once per side and shard as a sealed seed. Every sample copies the
  seed privately, and a changed seed stops the shard. Linux recipes build in a pinned build
  image (`cli.py build --build-image`): no network, only the run's directories mounted, and the
  authority closure read-only. Host builds need an explicit `--trusted-host-build`. Every
  recipe, environment and case is executable where its host provides it; elsewhere the run
  refuses with the host's reason. The CLI registry, shard and build records are now
  `af.perf-cli/2`, `af.perf-shard/3` and `af.perf-builds/4`, and earlier versions are refused
  ([ADR-0169](docs/adr/0169-coordinate-a-machine-seed-each-side-once-and-build-linux-recipes-in-a-pinned-image.md)).

- The performance laboratory runs frozen plans. `cli.py run` executes exactly one shard of a
  plan from its verified build, in planned order with its start and end sentinels. Nothing is
  rebuilt, retried or reordered. Each binary is rehashed before each launch. Whatever happens,
  the shard writes a terminal `shard.json`; slots past a deadline stay unstarted, and a cleanup
  that is not confirmed stops it. `cli.py aggregate` imports shard bundles from other hosts and
  recomputes the verdict. `fixtures/performance/cli.toml` declares whole-CLI commands and
  independent oracles. They check each side's package version from its own Cargo.lock, help
  tokens, a captured Task, a `task run` whose check(`true`) is proved to have run, and exact
  counts over `task show --json` on 10,000 Task events. Each side writes its own fixtures in an
  untimed setup step. `review ledger` and `review report` stay unavailable until a Campaign
  fixture writer exists. Samples and shards are now `af.perf-sample/2` and `af.perf-shard/2`,
  recording identity, metric scopes, execution and host observations with reasons for every
  unknown. The supervisor's request and result are `/2` and `/3`, adding the setup step and
  whole-stream pattern counts. Earlier versions are refused. The local macOS environment runs
  discovery and nonspawning library cases as `partial_scope`; Linux environments stay
  unavailable until validated on a host with a verified scratch. A container sample now binds
  only its own `sample/` directory, never the scratch filesystem's root. A build refuses when
  the overlaid probe closure changes during a compile, and `cli.py build --zigbuild-bin` builds
  the musl shipping recipe with exactly its pinned cargo-zigbuild and Zig.
  `cargo test --test perf_supervisor` finds its target from cargo's own layout, so a precreated
  target directory works
  ([ADR-0168](docs/adr/0168-run-frozen-shards-with-whole-cli-oracles-and-only-a-per-sample-scratch.md)).

- The performance laboratory now bounds its work by absolute deadlines that a slow step cannot
  stretch. A scratch filesystem check fixes one deadline before its first step. A capacity
  refusal that arrives after the fill's bound no longer verifies the scratch. A fill still
  blocked at the deadline is abandoned, and its file is kept and named. A sample container is
  removed once, within one 60 s allowance that also ends by the run's deadline. A seed's
  directories are refused while they are read, as soon as they exceed the entry bound. Each
  HTTPS request of the archive collector, its redirects included, is refused at its deadline
  however slowly the server sends, with TLS verification unchanged. A cancellation admitted
  before a sample's launch now always prevents the spawn; one that arrives during it cancels the
  launched child. The supervisor's integration test runs its own copy of the example, so a
  parallel test's rebuild can no longer remove it mid-run
  ([ADR-0173](docs/adr/0173-own-the-supervisor-a-test-runs-and-bound-admission-collection-scratch-and-cancellation-by-absolute-deadlines.md)).

- The performance laboratory admits files a measured side wrote only through held, non-following
  descriptors, within declared entry, depth, logical-byte and deadline bounds checked before
  anything is read. A seed with a symlink is refused before any mode changes, and a sparse
  oversized file before it is read. A build output that is a symlink, lies under a symlinked
  directory or exceeds the artifact budget is never copied. An AF binary without its own commit
  is never published. A shard also fixes its run's deadline before it rehashes any artifact. The
  `task show` oracle now judges the whole output as one complete JSON document: the Task's
  identity, every history entry numbered 0..9999 in order and 9,997 lease renewals. Fractions,
  exponents, misplaced entries and unclosed documents fail, and equivalent whitespace and member
  order pass (`af.perf-supervisor-request/4`, `af.perf-supervisor-result/5`, `af.perf-cli/5`).
  `cli.py report` writes a run's self-contained offline report, its JSON and a job-summary table.
  `cli.py archive` keeps run evidence in a content-addressed filesystem archive with retention,
  pins, capacity health and a read-only Actions collector. `cli.py history` shows compatible
  history across runs
  ([ADR-0172](docs/adr/0172-admit-candidate-files-through-bounded-descriptors-and-report-archive-and-trend-runs-offline.md)).

- The performance laboratory's diagnostic child runs (`cli.py profile`) now run in the contained
  Linux mode: `--mode linux_container --image … --scratch-fs …` takes a shard's preflight, then
  runs every execution in its own sample container, with only the run's own directory of the
  verified disk-bounded scratch filesystem writable. Outputs staged there are admitted into the
  evidence. CLI cases are diagnosed through each side's own AF binary, each execution in a
  private HOME and XDG. A state read uses that side's own seed, and every command's whole
  output is judged by its oracle. Musl shipping diagnostic recipes sit beside the GNU and macOS
  ones. A supervised run whose stdout was truncated fails before any oracle, even when its
  retained prefix parses. A step whose files together outgrow the evidence room is cancelled
  and refused, with every byte it wrote kept and counted. A seed tree is admitted and sealed
  before its confined copy into each repetition. Each trace states its region's coverage, so a
  setup-only trace is never complete (`af.perf-profiles/4`,
  [ADR-0176](docs/adr/0176-diagnose-cli-cases-and-contained-linux-runs-and-bound-every-step-and-seed-before-it-is-used.md)).
  The `af` browser now flushes typed keys before it shows a finished command's exit line, so an
  Enter pressed as the line appears is never discarded.

- The performance laboratory's diagnostic child runs (`cli.py profile`) now run every tool and
  candidate as a request to the build's authority supervisor under the machine lock. The
  execution mode is explicit (`--mode`), recorded beside what the plan's environment declares,
  and the contained Linux mode is refused as not implemented. Each repetition of a profiled
  probe is a fresh execution of its image within one recording, and its message is judged as it
  arrives. Every file a tool or candidate wrote is admitted as a confined, bounded regular file
  before it is read. The byte usage recorded is the evidence's actual size, with an
  administrative reserve inside the cap. Lost events and potentially lost samples are decoded
  apart from the recording itself, and are unknown, never zero, when it cannot be decoded.
  Sanitized flamegraphs keep only the SVG namespace and a parsed safe CSS subset. Explicit zero
  bounds are refused (`af.perf-profiles/3`,
  [ADR-0175](docs/adr/0175-supervise-every-diagnostic-execution-admit-what-it-wrote-and-record-what-it-used.md)).

- `report --diagnostic` now admits a child run's views against the trusted renderer, not their
  hashes. The child page must be exactly the page drawn from its admitted profiles and traces.
  Each sanitized flamegraph must be what the sanitizer makes of its original, and each Perfetto
  export must be its trace's. The frozen child plan must carry the parent's sources, selectors
  and authority, and the profiles must name that plan's sources, recipe, capture bounds and
  cases. An inconsistent child, or one with active HTML or SVG, is refused, and no child bundle
  is rewritten. A dispatched `refs/heads/NAME` now resolves where the workflow's checkout
  fetched it, `refs/remotes/origin/NAME`, in the control step and the scheduler alike, and is
  refused as ambiguous when a local branch of that name disagrees
  ([ADR-0178](docs/adr/0178-admit-linked-diagnostic-views-by-their-renderer-and-read-dispatched-branches-where-the-checkout-fetched-them.md)).

- A diagnostic child's page now links and embeds only sanitized flamegraphs. inferno's original
  SVG, which may hold a script, stays archived byte for byte in the child bundle, named by path
  and hash but never linked. `report --diagnostic` now refuses a child whose selected cases are
  not the parent's workloads, whatever IDs they share. Each case must keep the parent case's
  parameters, seed, timing, cache, oracle and AF environment; only its pairs, rank, alpha,
  orders and shard may differ. The report also refuses a child whose profiles claim another
  recipe kind than its frozen plan states, or whose evidence is not of that kind. Children are
  classified by the plan's kind, never by a recipe ID. Diagnostic plans now freeze the recipe's
  kind, base recipe and tools (`af.perf-run/4`), and earlier plans are refused. The comparison
  fixtures commit only their golden verdicts and generate their run bundles at test time
  ([ADR-0179](docs/adr/0179-link-only-sanitized-views-and-bind-a-diagnostic-child-to-its-parent-workloads-and-frozen-recipe-kind.md)).

- The performance laboratory now runs continuously through `.github/workflows/performance.yml`:
  pull requests to `main`, pushes to `main`, a nightly schedule and explicit dispatch all use one
  planner, runner, comparator and report, every script running from the trusted control
  revision. Gate branches are skipped, pull requests run only on ephemeral hosted runners with
  read-only permissions and no secrets, and enforcement stays advisory; before an activation
  exists the full check is unavailable and an advisory control bootstrap runs, and a policy
  change adds both proposed-policy bootstraps (`af.perf-schedule/1`). One credential-free job
  builds each run once and packs it with its modes for verified shards (`af.perf-build-pack/1`);
  shards upload their evidence failed or not, and the aggregation keeps missing shards
  incomplete. The report links validated diagnostic child runs and its rerun command keeps the
  frozen configuration (`af.perf-report/2`). Diagnostic runs now keep the scratch of an execution
  whose end was not confirmed, and count a raw trace against the evidence room while it is
  written
  ([ADR-0177](docs/adr/0177-schedule-continuous-runs-from-trusted-control-and-move-builds-once-to-bounded-shards.md)).

### Added

- Integrate the performance laboratory with advisory PR, main, nightly and release comparisons,
  a stable final check, verified toolchain image provisioning and bounded continuous evidence
  collection. Enforcement remains disabled until hosted calibration establishes readiness.

- `af review show` escapes control characters in a missing Finding key before printing its error,
  so the key cannot split or restyle the stderr line (#298).

### Fixed

- Admit the pinned artifact action's flat single-download layout for performance shards and
  reports, retaining run/plan identity checks and missing-shard failures. Run benchmark CLI
  setup, samples and diagnostics with `AF_SELF_OFFLINE=1`, preventing a detached update check
  from invalidating otherwise completed fixture setup.

Reduce repeated performance-fixture setup by writing admitted authority Tasks once per side
and shard, then restoring a private copy with a cold validator and a fresh production lease
for every sample. Distribute the complete quick suite over sixteen schedules, four at a time,
with up to 25 sampling minutes per shard inside the unchanged 60-minute total limit.
Keep the pair counts, coverage, performance thresholds and per-sample limits unchanged. Require complete full-suite
bootstrap evidence before calibration; enforcement remains advisory.

Authority compatibility: committed `.af/` policy is unchanged. The performance authority,
authority probe contracts and staged protected-harness digest change; earlier calibration
evidence does not activate this harness.

Seed readers construct the cold production validator with a non-executing host; the full
execution host is captured by the seed writer. This removes unused per-sample setup while
keeping the timed Store checks and runtime construction unchanged. Reader contracts advance
to version 3, and earlier contracts are refused.

Make diagnostic seed-reuse and flamegraph tests request exactly three fresh executions, so
FIFO scheduling delays cannot change the exercised repetition count. Production capture
bounds and the separate recording-cap tests are unchanged.

- Calibrate each performance case against the CPU group observed by its paired shard,
  allowing a hosted trial to use different allowed CPUs across shards. Keep whole-trial
  acceptance and all thresholds unchanged; unknown hosts still invalidate the trial.
  Readiness records advance to version 6 and refuse earlier versions
  ([ADR-0194](docs/adr/0194-bind-calibration-to-each-cases-observed-shard-cpu-group.md)).

- Predeclare a seven-day hosted performance calibration cohort for 23–29 October 2026,
  with 20 A/A comparisons, 16 preparation runs and 40 slowdown trials. Freeze its authority,
  cases, pairs and six observed CPU/image groups before execution. Retain every original;
  trials require separately sealed parameters and enforcement remains advisory.

- Every pull request description states the lines the pull request adds per directory, in a
  `Lines added` block that `python3 scripts/pr-lines.py` prints: a title with the total and a
  table with one row per directory — its figure, and its share as a bar and a percentage (a
  file under a `tests` directory counts as tests, a root file under its own name, figures
  rounded past 1000) — and the `PR report` check renders the block again from the pull
  request's commits and refuses a description whose block is missing or differs
  ([ADR-0195](docs/adr/0195-state-the-lines-a-pull-request-adds-per-directory-and-check-them.md)).

Fix self-optimization economics projections so a later complete capture clears earlier
unavailable gaps for the same declared source. Still-unavailable sources, incomplete
recaptures, and newer failures remain visible; retained capture history and
execution accounting are preserved.

- A pull request to `main` labeled exactly `no-perf` is not measured by the `Performance`
  workflow: adding the label cancels the running measurement, removing it measures again, and
  the stable `Performance check` reports the exemption as `exempt (label no-perf), not
  measured`, never as a measured pass. Pushes to `main`, the nightly schedule, dispatches,
  calibration, release handoffs and reruns ignore the label. Agents apply it only to a pull
  request whose whole diff is plainly prose with no performance impact. A Remote Check
  mapping entry may list `no_perf_paths`; a gate pull request whose whole change from the
  Task's source lies in them gets the label, which af removes before a head update that no
  longer qualifies and never creates in the repository
  ([ADR-0196](docs/adr/0196-skip-pull-request-measurements-only-for-an-explicit-no-perf-label.md)).

- `af provider detect` finds the `claude` and `codex` CLIs (on `PATH`, then in the usual install
  directories) and every auth directory on the machine — the CLI variables, `~/.claude` and
  `~/.codex`, their sibling names, the registry and `--dir` — asks each logged-in-looking
  directory's CLI what `af provider status` asks, and prints the one command that registers it;
  `--apply` registers every logged-in directory through `af provider add`'s publication, `--json`
  prints `af/provider-detect@1`, and with nothing found it prints the install commands. A directory
  without a login marker is never handed to its CLI, in `detect` or `status`, so neither creates
  `~/.codex` any more. `af provider add` refuses a directory that is not logged in (exit 6) unless
  `--unverified`; `af provider setup` creates a missing `--auth-dir` only with `--login`; its lock
  moved to `$XDG_STATE_HOME/af/locks`; each status row names one `setup … --login` fix
  (`--verbose` adds the CLI's own login command); `af help providers` explains the order of first
  use; and the Codex private-host login accepts the device prompt of codex-cli releases after
  0.159.2 (#305) ([ADR-0206](docs/adr/0206-detect-provider-logins-and-register-them-through-add.md)).

- Task plan dependency reference checks reuse the validated typed CAS envelope, avoiding a
  repeated decode and identity validation while preserving lock checks, reference sets and
  existing errors ([ADR-0076](docs/adr/0076-decode-and-verify-typed-cas-reads-once.md)).

- An Attempt survives one failed call inside its own wall and token reservation
  ([ADR-0208](docs/adr/0208-recover-an-attempt-from-a-transient-provider-failure-and-a-reply-that-does-not-parse.md)):
  a typed reply that does not parse is read tolerantly and then re-asked once, with both replies
  kept and named (#283); Codex typed calls pass their reply contract as `--output-schema` (#33);
  a network, capacity, timeout or silent-exit failure is waited out and retried up to three
  times, each wait recorded as a `TaskProviderEvent@1` that `af task show` names while it lasts
  (#295); an identity probe that times out or answers malformed is re-probed instead of read as
  an account change, and a Provider CLI update with the same account passes (#289); a refused
  admission probe is sent once more in other words, and every admission is decided before any
  gate starts; a report draft is validated for shape before sealing and the author is
  re-dispatched with the validation (#167). The shipped reviewer and evaluator slots allow two
  Attempts, and `af task show` prints each failed node's cause and first line. A sealed report
  whose verification missed the plan deadline still cannot be verified later (#166).

- The first run says what to do next
  ([ADR-0210](docs/adr/0210-make-the-first-run-say-what-to-do-next.md)).
  `install.sh` ends with the next steps and the PATH line for your shell, takes
  `AF_INSTALL_CHANNEL=rc` (or `--rc`) for the newest pre-release, and names the `minisign` install
  line for your package manager. Bare `af` without a terminal prints first-run hints before its
  usage; at a terminal with no Provider registered it opens on the Providers pane, and the
  Settings pane says `absent  <- af onboard` for a repository without `.af/`. `af onboard` picks
  the runner from the Provider kinds you registered, prints a `providers` block with the
  `--provider` binding for every model Worker (`--probe` adds their login state), and says what
  happened when it cannot fetch a release's checksums. New `af doctor [--json]` (`af/doctor@1`)
  checks the binary, PATH, shell, updates, tools, Providers, storage and the repository,
  token-free. `af help start` and `af help concepts` are new topics, `af --help` lists the
  commands once in first-use order, every flag of every command is documented, and
  `af task output` writes to stdout without `--output`. `af self setup-shell` gains `--remove`
  and says what is left to do; `af self uninstall` names the completion files and man pages it
  leaves, and `--purge` removes them. The README Quickstart is four steps with a first Task.

- One Task state behind every rendering: `af task list`, `af task show`, `af task report` and the
  last line of `af task run` print the same `phase/outcome/cause` (`finished/gate_failed/check_failed`),
  in text and under `--json` (`state`), with the one command to run next (`next: af task run ID
  --retry-gate`); `af help task-states` and [docs/tasks.md](docs/tasks.md#task-states) list every
  phase, outcome and cause. A failed gate is `gate_failed`, never `changes_requested`, and
  `af task run`, `af task start --execute` and `af task explain` exit 5 for it, 6 for a Provider
  failure and 7 for an expired deadline, while `af task show`, `list`, `report`, `status` and
  `findings` exit 0 (`af help exit-codes`). The documents are new versions:
  `af/task-inspection@13`, `af/task-list@3` of `af/task-list-entry@3`, `af/task-report@2`,
  `af/task-collected-inspection@2` and `af/task-selection@3`; the previous ones are refused.
- `af task show ID --checks` (`af/task-checks@2` under `--json`) prints the gate's last check
  receipt with each check's failing and flaky tests and the end of its stderr; `af task list
  --json` rows carry `gate`, the run log names the failing tests after a failed gate, and
  `af task show` names each failed node's cause, first error line and diagnostic artifact.
- `af task list` prints a header row with ID, state, round, tokens, age and the next command's
  verb, and shows a delivery only once there is one.
- A refused plan prints its reason on stderr, and an infeasible one the smallest limits that fit,
  computed from the reserve each verification slot protects (issue #250); `af task plan --fit`
  plans with them and prints what it raised, and `--json` carries
  `planning_diagnostic.required_limits`.
- A Task file without `strategy` takes its kind's (`implement`: `reviewed`, `review`,
  `facts.standard`); planning the same file again shows the existing plan, and `af task plan
  --replace` re-plans an unstarted Task in place.
- A Task's wall clock starts at its first `af task run`, not at `plan`: the preview says
  `deadline starts when you run`, and the run prints the deadline in local time
  (`deadline 17:41:18 +03 (2 h 59 min from now)`).
- Not-found errors name the Task, the Store's path and the Tasks it holds; an unreadable Task file,
  a missing output port and Task state inside the repository name the path, the ports or the
  repository root.
- A non-zero exit always says why on stderr: `af task explain`, `af task refresh`, a plan
  decision and `af task run` of a Task that already finished end with the run log's last line,
  `Task ID: phase/outcome/cause; next: …`, on stderr beside their result on stdout.

- A review Pipeline may leave an `advisory` reviewer of the captured review policy unbound.
  `[review.reviewers]` can name a reviewer only some Pipelines run (the `uix` reviewer of
  `kernel/review-light`) while the stock `kernel/review-code` Pipelines still plan on the same
  policy; before, the review reduce refused any Pipeline that did not bind every reviewer. A
  bound reviewer with no runtime result is still missing evidence and the round stays
  incomplete; a `required` reviewer must still be bound. Compiled signatures carry the allowed
  names in a new optional `unbound_allowed` field that older plans read as empty.

- The gate reruns the tests that failed in its parallel run once each, alone, before it fails: a
  test that passes alone is printed `flaky:` and no longer fails the step; one that fails twice
  does, as before. More than 25 failed tests, or 15 minutes of reruns, fail with the whole list
  ([ADR-0205](docs/adr/0205-rerun-a-failed-test-alone-report-the-gate-and-retry-a-failed-gate-on-the-same-candidate.md)).
  `make test-repeat` (`scripts/nextest-gate.py --repeat N`) runs a selection N times in a row to
  prove a flake fixed.
- Every Task check records `af/TaskCheckReport@1` beside its receipt: the host's 1-minute load
  average at the check's start and end, its CPUs, and what the check wrote to `AF_CHECK_REPORT`
  (`af.check-report/1` lines: its steps by stage, its test threads, its failed and flaky tests).
  `af task show` and `af task run` print where a gate stopped, with its fix (a stale
  protected-harness pin names `python3 -I scripts/performance/cli.py harness-digest --write`),
  the flaky tests and the tests that failed twice, each on its own line.
- `af task run ID --retry-gate` runs a finished Task's failed gate again on the same sealed
  candidate, in a new check Attempt of the same round, under the confirmed plan, with nothing
  re-planned or re-admitted; the Task continues into review when the check passes. A Task that
  ended at a failed gate prints `next: af task run ID --retry-gate`. The Task log gains the event
  `TaskGateRetried@1`; a release before this one cannot read a log that holds it.
- A Task whose source Snapshot pins a Rust toolchain that its `[warm]` checks cannot resolve on
  this machine is refused before its plan is admitted, before any implementer Attempt, naming the
  code policy file and the field, instead of building cold after the implementer ran.
- Two `af task plan` into one Store no longer collide in the content store: every object is
  staged under a temporary name of its own and a lost race is retried (#159).
- The heavy review, starter repair, interrupted repair, source capture, concurrent pipes and TUI
  tests no longer fail under load, the gate's fixtures pass under umask 0077 and with Rust
  coreutils, the real Rust build finds `cc` through `CC` and `~/.local/bin`, the protected
  harness pin made in a Worker sandbox equals a checkout's, and `cargo test` without nextest no
  longer meets `Text file busy` (#121); the reflink cache materialization of #149 has its
  regression test.

- New Worker package `kernel/implementer-long` 1.0.0: `kernel/implementer` with a six-hour attempt wall
  (the stock package keeps three hours, which the stock Task limits are sized for). The GA polish
  campaign's `kernel/implementation-ux` 1.1.0 runs its implementer slot on it after two packages
  were killed at the three-hour wall under host load with nothing sealed.

- Help that runs and refusals that name the fix. Every example in every `--help` page and
  `af help <topic>` now runs as written in an onboarded repository, or says what it needs first
  with `# needs: …`; a test runs them all. `af review plan` refuses, at exit 1, a
  `--provider NODE=ID` whose Provider this machine has not registered or whose node is no model
  Worker of the pipeline, in the words `af review run` uses, and ends with a `next` line, the
  exact `af review run` command once the plan is ready. A repository without `.af/`, an unknown
  `--policy-rev`, a missing binding and an empty diff each say what to do next. Things af cannot
  find are said in one shape, `<Thing> <name> not found in <where>; <fix>`, and a malformed
  Task file names the field path and the shape it takes. `docs/README.md` gains a
  troubleshooting table of the messages people meet most.
- A Store names the af release that wrote it. Every Store records each release that opens it
  for writing (`af/store-writer@1`, beside the event log), so a release that cannot replay
  another release's Store, which it still refuses (ADR-0145), says `Store … was written by af
  <v> (this is af <w>); fix: AF_VERSION=<v> af …`; `af task list`, `af review campaigns`, the
  browser and `af storage` list such Stores in one row by release. A Store from before this
  release is named as an older release's.
- The canonical Document text escapes only what is Markdown markup where it stands, so paths and
  ordinary punctuation read as written; its payload is now `af.document/2`, and a Document of
  `af.document/1` is not read. `af task output --format markdown` writes the readable
  presentation of a Document or report; `--format canonical-markdown` writes the recorded bytes.
- `af provider list` and `af providers …` work, `af review tui` points at the browser, `af
  provider doctor` shows only its own flags and requires `--campaign`, `af self --help` leads
  with `status` and `update`, `af review --help` groups its commands by a Campaign's life, and
  `af onboard --help` reads as four steps for a person. The roots af keeps state under are
  resolved once, so a home under a symlink (macOS `/tmp`) no longer prints `skipped … is a
  symlink`, and a path `af storage` cannot measure is a note of the new `af storage --verbose`
  rather than a warning after every run. Human text shows sizes, durations and local times in
  units; `--json` keeps the raw figures.

- The release check cannot stall: `curl` runs with `--connect-timeout 5 --max-time 30` (its
  retries within the limit), `gh` under a 30 s deadline, and af ends a client that outlives
  them. The cache is `af/self-check@3`: it records the last attempt and why it failed, which
  `af self status` prints (`last check: <time> · last attempt <time> failed: <reason>`); a check
  that has held `latest.lock` for over an hour counts as none and is reported with `rm <lock>`.
  `af self status` schedules a due check itself, `af self status --refresh` runs it at once, and
  `--json` prints `af/self-status@1`. A pre-release af asks for pre-releases whatever the channel
  and hears `latest stable X · latest pre-release Y (you run R: af self update --rc)`; a release
  older than the running one is never shown as simply `latest`. `af self update --check` honours
  `AF_SELF_OFFLINE` (`offline: check skipped`, exit 0) and says the anonymous route's HTTP status
  or curl error before any `gh auth login` hint; neither it nor `install.sh` needs `gh` for a
  public repository ([docs/self-management.md](../docs/self-management.md)).
- The Storage Budget keeps what it ate before: the newest warm key of a project checked in the
  last 7 days is never evicted, nor a key a check holds; caches go before telemetry and records;
  a finished Task younger than `[storage] keep_days` or behind a delivered Workspace stays; a
  Store with a live writer lease is judged in use before the sweep plans; a Project Store another
  release wrote is never collected, with or without Workspaces (#302). The default `[storage]
  max_bytes` is `"auto"`: 20 GiB, or 40 GiB when the cache volume has more than 100 GiB free, and
  `af storage` and the run say once when a warm key does not fit the budget, with the budget that
  does.
- A Task command prints one sweep line, `storage: freed 1.2 GiB (2 warm keys)`, instead of every
  `af storage:` removal; `af task run --verbose` and `af task start --execute --verbose` print
  them all. `insufficient_disk` names the volume by mount point and type with both numbers and
  the way out (`/tmp (tmpfs): 1.9 GiB … free, floor 10.0 GiB …; the sandbox root is /tmp; set
  TMPDIR or [storage] sandbox_root to a volume with room`); `[storage] sandbox_root` and
  `build_cache_root` move sandboxes and warm keys, and the floor measures those volumes.
- The Rust toolchain mapping is configured: `[task] rust_toolchain_policy` in the user's
  configuration or a project's `.af/af.toml`, with `AF_TASK_RUST_TOOLCHAIN_POLICY_FILE` as the
  override, and `af config show --origin` names where it came from. `af self optimize --execute`
  runs under the interrupt watcher like every Task-running command (#158). `make prereqs` checks a
  contributor's machine, one line per tool with its install command, and changes nothing (#161).

- A Task whose `af task run` died mid-Attempt (a lid closed on battery, SIGTERM, a reboot, a
  killed process, a lost lease) resumes with `af task run TASK_ID`: the lost Attempt is settled
  as abandoned, charged only the usage its Provider reported (never its reservation, which goes
  back to the Task's budget), and its node is dispatched again without using one of its
  Attempts, once per node unless its slot declares `recovery_attempts` (`af.pipeline/3`, compiled
  as `af.compiled-task/2`). SIGINT and SIGTERM settle the interrupted Attempt the same way, so the
  advertised resume now runs the node again. The resume's first line says which node runs again,
  or `cannot continue Task X: node N lost its writer 2 times (recovery allowance 1); fix: …`
  with exit 1; a dead run's lease is waited out with a line saying so instead of refused
  ([ADR-0209](docs/adr/0209-recover-a-lost-or-interrupted-attempt-without-charging-its-reservation.md)).
- The writer lease is 60 s, renewed by its own heartbeat thread once a third has passed; a writer
  that loses it stops its Workers at once and says `lost its writer lease (writer_lost: …)` with
  the Store's observed time and the lease's expiry. On macOS `af task run` and
  `af task start --execute` hold a power assertion (`caffeinate -i -s -w`) while they run, and
  `af task run --verbose` says so.
- An interrupted Task's state reads `abandoned/cancelled/writer_lost` (or `host_cancelled`) with
  `next: af task run TASK_ID`, or `af task start --file TASK_FILE` once a node used up its
  recovery allowance, and `af task show` adds `why: node N stopped at T`. The refusal `Task reservation is not
  dispatchable` names the guard that fired and its numbers (`reservation 40 > remaining 129`).
- The performance laboratory follows the production lease: its `product_constraints` and golden
  constants say `task_lease_ms = 60000`, its probe takes the 60 s lease, and the protected
  harness digest is re-pinned.

- A Task that ended at a failed gate is `finished/gate_failed/CAUSE`: `af task run` (gate retry
  included), `af task start --execute` and `af task explain` exit 5 for it, while `af task show`,
  `list`, `report`, `status` and `findings` exit 0 once they read the Store (`af help exit-codes`),
  and its one `next:` line is `af task run ID --retry-gate` only when the kernel would run that
  gate again on the same candidate; otherwise it is `af task show ID --checks` (ADR-0205).
  `af task show ID --json` carries the gate's check reports (`gate.check_reports`, each check node's latest `af/TaskCheckReport@1`) and its retries
  (`gate.retries`, every `TaskGateRetried@1`) in `af/task-inspection@13`; `af task report` counts
  each `af task run ID --retry-gate` as a run of the Task and an `af task refresh` as none. The
  run log's last line on stderr, the Task's state and next command, now comes after what the
  stale sandbox sweep and the Storage Budget sweep that end a run print.

Validation runs the performance laboratory once on the latest stable Python, with required
Tomli/tomllib parity and fail-closed propagation to the required lint check. Other validation
jobs select the same latest-stable policy before running Python and Make commands.

This repository opts into distinct-model Worker independence under the owner-approved policy,
allowing one Codex Personal account with explicit Sol implementation, Astra correctness/evaluation
and Terra bug review bindings. Engine defaults and generic catalog Provider defaults are unchanged;
fresh sessions, distinct packages, role-scoped inputs, required checks/review and exact-plan
approval remain required. Different models do not establish different accounts (ADR-0198).

`af provider disable ID --for 2d` now suspends a registered Provider on this machine until an exact
wall-clock expiry; `af provider enable ID` restores it immediately. Positive durations accept
seconds, minutes, hours and days up to 30 days. Separate private versioned state leaves registry
v1 unchanged, survives restarts and refuses new probes and Task private sends while disabled.
Status JSON advances to `af/provider-status@3` with expiry and the enable action. Already-running
calls may finish, and older pinned releases do not honor suspension. Provider documentation also
explains how the trusted independence knobs retain model diversity or restore strict principal
separation without a general bypass.

- `af self --help` lists `status` with a one-line summary that no longer wraps into a phantom
  `release` command; `af self status --help` keeps the full description. The examples of `af
  provider add --help`, `af task run --help` and `af task show --help` that need existing state
  now say what they need (`# needs: …`), as every other help example does. `af provider doctor`
  keeps `--campaign NAME` required: its admission evidence belongs to a Campaign that `af review
  run --campaign` reuses.

Provider disable now preserves long registered IDs within the existing 64 KiB aggregate state
bound, and `af provider enable ID` can clear a retained disable entry after registry removal to
recover the 32-entry capacity. Unknown IDs and unsafe or malformed state still fail closed;
updates remain private, atomic and token-free. Command, status and refusal diagnostics show the
exact expiry as a UTC calendar date and time while persisted Unix seconds and nanoseconds stay
unchanged. Provider documentation now correctly describes `distinct_providers` as comparing
adapter families, preserving the existing trusted principal/model policy knobs.

Admit an explicitly configured 90-minute check Attempt without changing the 60-minute explicit
per-process cap, aggregate multiplication or configured defaults. Provider disable schema fixtures
now resolve through the runtime workspace root. Copied Command-worker catalogs explicitly use
package independence while asserting production model diversity, and Project collection failures
report raw exit status and Unix signal. Telemetry corpus facts query a private database/WAL copy
to avoid native SQLite's sidecar-free read-only failure while preserving the measured index and
committed WAL rows. Provider suspension retains its private sidecar, exact
expiry, long-ID lifecycle, orphan cleanup and pre-invocation checks.

- A warm check layer is reused by content, not by modification times: af records, per Cargo
  package, the content each layer last built and stamps only the packages whose content differs,
  so a crate another Snapshot compiled can no longer be linked (#266) and unchanged crates keep
  their build. A check that finds the layer busy waits for it until its deadline, saying every
  minute whom it waits for (`[warm] clone_when_busy = true` builds in a private copy instead), and
  the Storage Budget never evicts a layer a check holds. `[warm] build_wall_ms` gives the build
  its own deadline, ending at the gate's `build` step, so `check_wall_ms` is left for the tests; a
  layer evicted under a running build is named as the cause. An `offline = true` check runs with
  no network from the layer's `npm_cache`, its `npm_packages` pinned by SHA-256 and filled once;
  this repository's markdownlint gate now runs that way (#230). Sealing leaves out the generated
  files a Worker's runs left (`__pycache__/`, `*.pyc`, `/target/`, `node_modules/`, the tree's
  `.gitignore` and `.af/seal-ignore`), lists them in `af.seal-ignored/1`, and refuses a FIFO,
  socket or device file by name (#160). `af task show --checks` prints each check's layer, lock,
  source groups and phases; its `--json` document is now `af/task-checks@2`
  ([ADR-0207](docs/adr/0207-key-warm-check-layers-by-content-lock-them-split-the-build-deadline-and-seal-without-generated-files.md)).

- The agent loop's verbs
  ([ADR-0211](docs/adr/0211-give-the-agent-loop-its-verbs-and-deliver-a-verified-verification.md)):
  `af task findings ID [--round N]` prints a review round's findings per reviewer — severity,
  confidence, title, location, evidence and fix, grouped by severity — and the evaluator's
  verdict (`af/task-findings@1`); `af task diff ID` prints a candidate's change as `git diff`
  would, touching no worktree (`af/task-diff@1`); `af task materialize ID --into DIR` makes a
  worktree's tracked tree equal to a Task's candidate, refusing uncommitted changes without
  `--force` and an unverified candidate without `--allow-unverified` (`af/task-materialized@1`).
- `af task run ID --detach` runs a Task in the background and returns with its pid and its log,
  `runs/<ID>-<N>.log` in the Store (`af/task-detached@1`); `af task wait ID [--timeout S]` blocks
  until the Task settles and exits with its code (124 on timeout); `af task status ID` prints one
  line now (`af/task-status@1`); `af task cancel ID` stops the run its lease names, by its
  recorded pid only, records who asked, and leaves the Task `abandoned/cancelled/host_cancelled`
  and resumable (`af/task-cancelled@1`). Every run records its process in `runs/<ID>-<N>.json`.
- `af task run` prints one stderr line per node that starts or finishes, with the elapsed time
  and the tokens so far, and a heartbeat every 5 minutes (`--quiet` keeps them back); its last
  stderr line is the state and the next command in text as under `--json`.
- A verified `verification` Task of another Task's candidate delivers that candidate on the
  other Task's source, recording both Tasks (`af/TaskDeliveryProof@1`); `af task deliver`'s
  other refusals name `af task materialize`.
- `af task report IDS… --pr-body [--template FILE]` prints the whole pull request description,
  the report and the `scripts/pr-lines.py` block spliced into the template, which
  `scripts/check-pr-report.py` accepts; `af help agents` and [docs/agents.md](docs/agents.md)
  walk the loop.

Stabilize Provider disable verification fixtures by waiting for the complete project TUI frame,
scheduling heavy starter repair exclusively under the existing realtime nextest policy, and
using the existing debug-only fixed Store clock for the telemetry-level invariance comparison.
Guarded measurement failures now include bounded nested Task inspection, invocation, feedback
and diagnostic artifacts with their exact scenario. Preserve all Provider behavior, checks,
assertions, internal budgets and the approved 90-minute whole-check allowance.

Run targeted starter repair exclusively within the existing CI nextest realtime override,
alongside heavy starter repair, preserving its five-second Workers, 15-second lease and every
assertion. Captured-admission accounting/replay failures now report debug and raw Unix exit
status and the Unix signal while retaining exact exit-code checks; the earlier signal remains
unknown. Preserve Provider disable behavior, the authorized gate-local four-thread command,
all gate targets and internal budgets, and the approved 90-minute whole-check allowance.

- `af task run NEW --from TASK [--round N] [--goal FILE]` plans a new Task from a finished
  reviewed Task: its source is the candidate that review round reviewed, not the base, and its
  implementer receives the round's reviewer findings and the evaluator's verdict as one
  `af/TaskReviewFindings@1` document on the Pipeline's optional `findings` input; nothing is pasted into
  the goal. A Task file says the same with `"continue": {"task": "TASK", "round": N}`. `af task
  show` prints `continues TASK round N`, `--json` carries `continues` in
  `af/task-inspection@13` (`@12` is no longer written or read), and `af task report` names it in
  the row
  ([ADR-0212](docs/adr/0212-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md)).
- The kernel's `kernel/implementer` and `kernel/implementer-long` take the optional `findings`
  input and list each finding by ID as `fixed`, `disputed` or `out of scope`;
  `kernel/implementation-reviewed`, `kernel/implementation-ux` and `kernel/review-light` declare
  it, re-pinned in `.af/task-catalog.toml`.
- Every reviewer, evaluator and verifier gets the tree before the change read-only at `../base`
  and `../diff-subject.json` (`af/diff-subject@1`, each changed path with its digest on each
  side). The base is a sealed candidate's parent or a bound tree's; a Task file may name it with
  `"base": {"commit": "SHA"}`.
- A Task file needs only `task_id`, `kind` and `goal`: `[task.defaults]` of `.af/af.toml` supplies
  `limits`, `pipeline` and `fallback`, and `af task plan` prints what came from it. `context`
  names files the Worker reads first and `.af/rules.md` holds standing rules; both are listed at
  the end of the Worker's requirements with the digest of the bytes the Task captured. `af help
  task-file` describes the shape.

- The browser shows review Campaigns and every Task Store. Bare `af` gains a `campaigns/` folder
  that lists every review Campaign on the machine: the default Campaign root, the Store a plain
  `af review run` keeps for each repository under `review/local/`, and every `--state` Store
  and `--state-root` directory `stores.toml` records. Each row shows the Campaign's newest
  Round, its state, its verdict, what it spent, when its Store was last written, and its
  reviewer nodes as chips. The rows update once a second while a Round is open. Opening a
  Campaign shows its Ledger by reviewer. Enter on a row puts the matching `af review report`,
  `ledger`, `show` or `run --restart-round` command on the `:` line. The `tasks/` folder also
  lists every `--state` Task Store the registry records, each under its path, and `:state DIR`
  adds one for the session; `r` and `D` name such a Store with `--state`. `?` opens a help
  overlay naming the panes, the keys and `: <af command>`, so backward search is now `/` then
  `N`. While no key comes, the status line takes turns with `? help - : run a command - q
  quit`. An ambient Provider says how it got there and how to make it go: through
  `CLAUDE_CONFIG_DIR` or `CODEX_HOME`, or the CLI's default directory, with the
  `af provider add` command that registers it. Stores other releases wrote are one row in both
  panes. `af review campaigns` text is now a table in 100 columns (Campaign, Round, state,
  verdict, tokens, wall, last update); `--format json` is unchanged.

- The documentation index names what the merged packages added: the `docs/README.md` row for
  `tasks.md` lists Task states, resuming an interrupted run, retrying a failed gate, continuing a
  reviewed Task and warm checks (`af help task-states`, `af help task-file`), and
  `docs/task-execution.md` gains a contracts row for continuing a reviewed Task
  ([ADR-0212](docs/adr/0212-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md))
  and links `af/task-collected-inspection@2`, the version af writes. The release notes of the
  one-outcome state model and the gate evidence name `af/task-inspection@13` and
  `af/task-checks@2`, the versions this release prints.

- The Storage Budget keeps every Task a delivered Workspace was made from. The delivery records the
  Task ID as data in a new Project Store event, `WorkspaceDelivered@1`, so the protection holds
  when the label was suffixed because the Task ID was taken. Every Project Store is read for this
  without the discovery ceiling. While one cannot be read, a sweep collects no finished Task and
  names that Project Store. A campaign whose writer leases cannot be read is in use for the sweep
  and `af review gc`
  ([ADR-0158](../docs/adr/0158-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)).
- `af storage --json` prints `af/storage@5`. Its `volumes` name each root the free-disk floor
  measures (`sandbox`, `cache`, `build_cache`) with its volume, `free_bytes`, `below_floor` and
  `next_action`. Its `unreadable_store` entries carry the `writer_release` and the `next_action`
  from the Store's provenance. A version-1 `stores.toml` is replaced by the first write that
  meets it, said once, instead of a warning on every run.
- `af self update --check --rc` on the stable channel records the pre-release only as
  `latest_prerelease`, and status keeps naming `af self update --rc`. A failed explicit check is
  recorded as the last attempt with its cause. `af self status --json` prints `af/self-status@2`,
  with `check_lock` and `next_action` (`rm <lock>` for a stalled check). The update row of
  `af doctor` warns of a failed or stalled check with the same fix. On macOS its storage row warns
  when Spotlight indexes the warm build cache, the warm Workspaces or the sandbox root, and
  [docs/README.md](../docs/README.md#macos-spotlight-and-gatekeeper) gives the exclusion.
- `af provider detect` prints `af/provider-detect@2`. Each auth probe waits 10 seconds at most,
  and an unknown login carries `auth_cause` and `auth_detail`. A directory that cannot be listed
  is probed by its marker names. An unreadable registry makes every row's `registration`
  `unknown`, and `--apply` refuses it with exit 1. `--apply` checks a registered row again under
  the registry lock, and leaves a login whose CLI is off `PATH` unregistered with the PATH line
  (exit 4). A conflict names `conflict_with` and suggests the retry with a free ID. The closing
  `next:` line follows the outcome, and `--kind claude` gives no Codex advice
  ([ADR-0206](../docs/adr/0206-detect-provider-logins-and-register-them-through-add.md)).
- `af provider status --json` prints `af/provider-status@3`, with `found_unregistered` and
  `next_action`. `af doctor` and `af onboard` list the same directories from the same scan. A
  quota window without a reset time says `Claude does not report when this window resets`
  (`reset_reported: false`). The browser's ambient row now says that unsetting `CODEX_HOME` lists
  `~/.codex` instead, rather than that it stops the listing.
- A Claude reply that is the account's usage-limit refusal now says `the Claude account's usage
  limit is reached; it resets at <time>` (cause `provider_capacity`). It no longer reports the
  internal model-bound error. `make prereqs` reports rust missing when the pinned toolchain lacks
  `clippy` or `rustfmt`, with the `rustup component add` command. Its report wraps at 100 columns
  and spells `$HOME` and `$TMPDIR` short.

- A failed gate names `af task run ID --retry-gate` only when the kernel would run it: a Task
  whose budget leaves no Attempt for a new check, whose plan approval expired or whose deadline
  passed names `af task show ID --checks` instead, and `--retry-gate` refuses it with the reason
  and `af task plan --file TASK_FILE --fit`. A failed paid Worker that ran beside the gate, with
  no edge to it, refuses the retry too (`a paid node ran beside its failed gate`).
- While a gate retry is pending, `af task show --json` keeps the `gate` object, with the checks
  of the receipt the retry superseded and the pending retry under `retries`.
- An Attempt abandoned by SIGTERM or by a lost writer is charged the usage its Provider reported,
  incomplete counters included, never its reservation; the rest returns to the Task's budget
  ([ADR-0209](docs/adr/0209-recover-a-lost-or-interrupted-attempt-without-charging-its-reservation.md)).
  An Attempt its own answer or failure ended still keeps the reservation floor.
- An interrupted `af task run` whose node used up its recovery allowance names `af task start
  --file TASK_FILE`, the next command `af task show` names, instead of a resume that refuses.
  Under `af task run --verbose` the resume line comes first and the power assertion line after
  it, each wrapped at 100 columns.
- A verification Task delivers another Task's candidate only when its output is the very
  Snapshot it verified; another Snapshot with the same tree is refused
  ([ADR-0211](docs/adr/0211-give-the-agent-loop-its-verbs-and-deliver-a-verified-verification.md)).
- A judging Worker that restores write permission on `../base` or `../diff-subject.json` and
  changes them has its Attempt refused at seal, naming the path
  ([ADR-0212](docs/adr/0212-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md)).
- A Task Worker that misses its wall is named by its slot (`implementer did not answer within
  …`), not as a reviewer. A check line whose warm directory was evicted ends `; the next check
  runs cold`. The deadline refusal lists each node on the longest path as `1 × wall`, the
  verification reserve, their total and the Task's wall.
- Concurrent content-store writers never replace an object another one already published; the
  existing object is verified and kept. A sandboxed macOS check whose `sysctl` lookup is refused
  reads the load average through `uptime`. The gate's own preflight gives its child builds a
  private `TMPDIR`.

- A usage error under `--json` is the one `af/error@1` document `af help json` promises, with
  clap's message, `exit_code` 2 and `cause: usage`, beside clap's own lines on stderr; `--help`
  and `--version` stay what they are. `af/error@1` now has a schema (`schemas/error-v1.json`), and
  a refusal af classifies adds typed fields a script branches on: `cause`, `next_action` and
  `details`. A node past its recovery allowance is `recovery_exhausted` with the node, how often
  it was abandoned and its allowance; a zero `limits.wall_ms` is `task_file_invalid` naming the
  file and field instead of `--timeout-secs`; an unreadable `--goal` file is `input_unreadable`
  and says how to fix it.
- Every next command af prints for a Task carries the `--repo`, `--state` or `--workspace` the
  command was given, as typed: the `next:` lines of `af task show`, `run` and `list`, the
  `state.next_action` of every `--json` document but `af task report`'s, the preview's approve and
  `Details` commands (no more "use the same selectors" line), a refused plan's `--fit` command and
  a recovery refusal's fix. Run as printed, each acts on the same Store; under the default Store
  nothing is added.
- A refused plan is `af/task-selection@3` (version 2 is refused): it adds `error`, `exit_code` and
  `next_action`, the `--fit` command for an infeasible plan. A refused `af task refresh` says why
  on stderr, and under `--json` is one `af/error@1` with `cause: refresh_refused`, exit 4.
- `af review plan --json` carries `next_action`, the command its text prints, or null; its text
  wraps at the terminal's width, prose under its label and a long command continued with a
  backslash between its arguments. `af task list` moves each row's next command to an indented
  line of its own when the table is wider than the terminal.
- Every `--help` page and `af help` topic fits 100 columns, and every example fits 80: a note
  that did not now sits on its own `#` line above its command. `af task --help` names `run
  --from`, and `af task report --help` counts a gate retry's lease as active time.
- The human Markdown of a Document or report escapes a line's first word only where it could
  open a block, so `-foo`, `+1` and `!wow` read as written; the canonical `af.document/2` bytes
  are unchanged.

- `af task run --detach` no longer claims a run it did not see start: its document is now
  `af/task-detached@2` with `admitted` and `next_action`, and a run that neither took the Task
  nor admitted its plan within 2 minutes exits 4 with `admitted: false` and `af task status ID`
  next
  ([ADR-0211](docs/adr/0211-give-the-agent-loop-its-verbs-and-deliver-a-verified-verification.md)).
- A second `af task cancel` of one run reports the first request, its requester and time, and
  waits for the same stop instead of signalling again. `af task status` names the run that holds
  the Task's lease and prints the next command on its own indented lines, broken between its
  arguments, within 100 columns (the terminal's width when narrower). `af task run` prints a
  `started` line for every node, the kernel's own (`seal`, `check`, `review.reduce`, `accept`)
  included, before its `finished` or `failed` line.
- `af task report --pr-body` exits 1, naming `--base REF`, when the checkout's
  `scripts/pr-lines.py` cannot print its lines added block, instead of printing a body the pull
  request check refuses; the new `--base REF` is passed to the script. `af task report` warns on
  stderr when two Tasks' candidates add the same `docs/adr/NNNN-*.md` number, naming both.
- `af task findings --round N` and a continued Task's `af/TaskReviewFindings@1` are built by one
  function and carry the same findings; a refused `--round` of a Task without a review round
  ends with the Task's next command.
- A Store gets this release's writer marker in the transaction of its first appended event, never
  at an open: a Store another release wrote and this one refused stays named by that release, and
  a marker that cannot be written fails the append. The summary of Stores several releases wrote
  names one `AF_VERSION=…` command per release instead of the newest for all, within 100 columns.
- The browser's Campaigns pane reads the Stores every 5 seconds while no Round is open, so a new
  Campaign appears without a key; shows a Campaign Store it cannot look at as an error naming the
  path; shows an open Round's own reviewer chips, none before its first Attempt, and a node's
  latest Attempt by its start, not its ID. Two unavailable registered Task Stores are two rows.
  `af review campaigns --state-root R` with no Campaign names
  `af review run --campaign NAME --state R/NAME`, within 100 columns.

- A failed gate's next command is the one the retry eligibility chooses, with the Store
  selectors the command was given: `af task show ID --checks --state DIR` when the kernel would
  refuse `--retry-gate` (no Attempt left, deadline passed, a failed paid node beside the gate),
  `af task run ID --retry-gate --state DIR` when it would run it. The message of a run stopped by
  SIGINT or SIGTERM and of one whose writer lease was lost now name their `af task run ID` or `af
  task start --file TASK_FILE` with the `--repo`, `--state` or `--workspace` it was given, the
  same command `af task show` names. `af help json` lists `af/task-detached@2`, the version af
  prints, among the documents that carry the Task's `state`. A Store path wider than the
  terminal keeps a line of its own in a printed command, alone or with its `--state`; it is never
  broken.

- `af provider detect` recognizes the default Claude context `af provider status` accepts, a
  `~/.claude` with no marker inside it and a regular `~/.claude.json` beside it: both commands ask
  one predicate, so detect lists it (marker `~/.claude.json`), probes it and `--apply` registers
  it as `claude-main`, and status probes the registered Provider instead of calling it
  `not_probed`. `af provider detect --json` prints `af/provider-detect@3`
  (`schemas/provider-detect-v3.json`; versions 1 and 2 are neither printed nor read), whose next
  actions are typed fields: `install`, one `{kind, command, docs}` per kind whose CLI was not
  found, and `next_action`, the scan to run again with its own `--kind`, `--dir`, `--id` and
  `--all` when no CLI was found
  ([ADR-0206](docs/adr/0206-detect-provider-logins-and-register-them-through-add.md),
  amended).
- `af task findings ID --round N` shows round N's own evaluator verdict, the one the Task log
  recorded after that round, instead of the last one the current execution holds, and a
  continuation (`af task run NEW --from TASK --round N`) hands its implementer that verdict, not
  the verdict of another round that reviewed the same Snapshot.
- `af task report --pr-body` refuses a lines added block that `scripts/pr-lines.py` printed
  and `scripts/check-pr-report.py` would refuse, with exit 1 and a message naming what is
  missing. A whole block is the begin marker, then the title `**Lines added: <figure>**`, the
  header `| Directory | Added | Share |`, its separator and at least one directory row, each
  directory once, or `**Lines added: none**` alone, then the end marker; `af task report --help`
  and `docs/tasks.md` say the same.
- `af help start` binds its review command to the Providers registered on this machine, as
  `af onboard` binds them, and prints `PROVIDER` with the reason when none is registered; the
  README shows the placeholder form and says that `af onboard` prints the exact command.
  `af help agents` teaches `af task run NEW_ID --from TASK_ID [--round N]` for another round,
  keeping the hand-made source binding only as the form that carries no findings.
- `af doctor`'s rows fit 100 columns, or the terminal's width when narrower (at least 60): a long
  detail or path continues on indented lines, broken after a `/` and never cut short, and each
  fix command stays whole on its line or is broken between its arguments.
- `af task report --pr-body` accepts only a lines added block that `scripts/pr-lines.py`'s own
  parser reads back: the title, then the table header, its separator and at least one row, each
  directory once, or `**Lines added: none**` alone. A title without its table is refused with
  exit 1, naming what follows the title, instead of printing a body `scripts/check-pr-report.py`
  refuses.
- A continuation from an earlier review round (`af task run NEW --from TASK --round N`) records
  the provenance of that round's own candidate: its source origin names the `af/TaskOutput@1`
  record that published the candidate, not TASK's result when the result holds a later Snapshot.
  A round whose candidate no recorded output holds is refused with `af/error@1`, and binding a
  source refuses any reference whose output holds a Snapshot other than the bound one
  ([ADR-0212](docs/adr/0212-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md),
  amended).
- `af doctor`'s fixes are commands a shell runs as given, and what a command cannot say moves to a
  new hint: `af doctor --json` prints `af/doctor@2` (`schemas/doctor-v2.json`; version 1 is
  neither printed nor read), each check with `fix` and `hint`, and the text prints a `hint:` line
  under the fix. The shell check names the shell `$SHELL` names (`af self setup-shell --shell zsh
  --write`), or `--shell SHELL` with the choices in its hint; the project check's fix is `af
  onboard --repo PATH`, its hint says it is a token-free preview; the PATH fix is the line for
  the shell `$SHELL` names, its hint where it belongs. A problem no single command fixes (no
  known package manager, Spotlight indexing, an unreadable lock) has a null `fix` and a hint that
  says what to do ([ADR-0210](docs/adr/0210-make-the-first-run-say-what-to-do-next.md), amended).
- `af doctor`'s PATH fix runs in the user's own shell and keeps the directory literal: `export
  PATH='DIR':"$PATH"` for bash, zsh and sh (a `'` written `'\''`), `fish_add_path 'DIR'` for
  fish, `set paths = ['DIR' $@paths]` for elvish and `$env:PATH = 'DIR' +
  [IO.Path]::PathSeparator + $env:PATH` for pwsh, each in that shell's single quotes, so a `$`,
  `"`, backquote or `$(…)` in the directory is never expanded when the line is pasted. Another
  shell, or a directory with a `:` or a control character, gets a null `fix` and a hint naming
  the directory. `af self setup-shell --write` writes completions and man pages and leaves PATH
  alone, so it is never the PATH fix.
- `af doctor` offers a fix only when running it can clear the check, never when the error the
  check reported would stop it. A `.af/` that does not validate (`af onboard` would stop on it),
  a machine `config.toml` that does not load (`af config show`, `af self` and `af storage` would
  stop on it) and an unreadable Provider registry have a null `fix` and a hint naming the file,
  the parse error's line and `af doctor` (with `--repo PATH` for the project) to check again;
  another check's fix that would run one of those commands moves into its hint, after the file
  to correct first. A `.af/` that does not validate is shown by its error's first line.
- Binding a continuation's source checks the referenced output's artifact, not only its port: the
  `af/SourceTree@1` the port names must hold the Snapshot the port names, and the bound one, or
  the binding is refused with `af/error@1` and nothing is recorded.
- The pseudo-terminal test helpers pass the test process's `TMPDIR` (and, for `af provider
  detect`'s, the fixture's home and XDG directories) to the child they start after clearing its
  environment, so the tests pass in a sandbox whose `TMPDIR` is private.

Reconcile the Provider disable feature with current repository authority: preserve upstream
ADRs 0196–0198, renumber the feature decisions to 0199–0203 and update their references. Keep
the experiment, report and remote-twin planning fixtures under distinct-model policy, retaining
isolated Provider probes, exact Worker identities and admission reservations; cover the
Command-only policy helper separately on a disposable catalog (ADR-0204). Provider disable,
status and registry contracts, the approved ninety-minute check allowance, all internal budgets
and required checks remain unchanged.

### Fixed

- `install.sh` no longer stops with `SHELL: parameter not set` where `$SHELL` is unset (containers,
  CI runners, cron). It prints the generic PATH line instead of a shell-specific one.

## [0.12.0-rc.5] - 2026-10-21

### Authority compatibility

Committed .af/ policy keeps working as is relative to 0.12.0-rc.4; no authority migration is required. Every existing pipeline stays `af.pipeline/1`; only a pipeline that declares a Project Brief root port is `af.pipeline/2`, pinned like any new pipeline by `af onboard --refresh-lock`. Machine state needs no hand edit: `$XDG_STATE_HOME/af/stores.toml` is now version 2, and a version-1 file is refused with its way out (delete it; Stores register again as commands use them); `af storage --json` is `af/storage@3` and a Survey record is `af/ProjectSurvey@2`, their superseded versions refused; telemetry records under `$XDG_STATE_HOME/af/telemetry/` at level `spans` unless the machine's `[telemetry]` table sets `level = "off"`, and nothing leaves the machine unless an OTLP sink is configured.

### Changes

- release: v0.12.0-rc.4 (#269)
- fix: reject explicit zero unknown usage in collected tasks (#273)
- fix: reject PR report content hidden by HTML, comments and code (#275)
- fix: preserve copyable preview commands for maximum-length task IDs (#274)
- fix(tui): preserve error reasons for paths with spaces (#276)
- docs: align setup help with current CLI behavior (#277)
- ci: push the release pin with a deploy key that bypasses main's ruleset (#281)
- docs: record the 0.12.0-rc.2 Task startup profile from issue #255 (#282)
- Telemetry P1: the review-telemetry crate, levels, index and status (#286)
- fix: check for updates in headless and JSON commands (#285)
- Project: identity, Workspaces, Surveys and their collection (#291)

- A `task_collected` tombstone (`af/TaskCollected@1`) that spells `unknown_usage_attempts: 0`
  is now refused on admission, event validation and replay, as its schema already refused it;
  a tombstone without the field still reads as no unknown usage, and written bytes are
  unchanged ([#194](https://github.com/PakhomovAlexander/afactory/issues/194)).

- `af task start` and `af task explain` no longer wrap a suggested `af task run` action or
  `Details` hint inside a Task ID. With a 128-character Task ID, the copied command keeps every
  shell token, the exact `--confirm-plan` ID and a valid `\` continuation. Prose rows still wrap at
  96 columns, and command rows are still sanitized
  ([ADR-0149](docs/adr/0149-never-wrap-a-suggested-command-inside-a-shell-token.md)).

- The `PR report` check no longer accepts an `af task report` block GitHub does not render as
  the report: a block wrapped in a raw HTML block (`<pre>`, `<script>`, `<style>` or
  `<textarea>`, indented or not, closed or left open), whose markers are its text, and a
  pipeline line or round table, between visible markers, that sits in a code block, inside an
  HTML block, which a fence inside it does not end before its blank line, inside an HTML
  comment, or inside the quoted attribute value of a tag an HTML block leaves open, or a
  pipeline line that starts inside a code span opened on a paragraph line above it. A comment
  stays hidden from its `<!--`, in an HTML block or spanning the lines of a paragraph, across
  blank lines to the first `-->` of raw HTML. A raw block ends at the closing tag of any of the
  four names, a `<pre>` nested in another HTML block opens none, and a `<!--` in an attribute
  value, a code span or escaped hides nothing, as GitHub renders them; so do steps or a Task cell
  in a code span. The block-level tags that interrupt a paragraph are GitHub's: `<source>` is
  one, `<search>` is not (issue #193,
  [ADR-0142](docs/adr/0142-carry-the-af-task-report-in-every-pull-request.md)).

- `af` browser: `:scope project` from a long start outside any repository and `:cd` into a
  repository whose configuration file does not parse now keep their reason visible on the
  status line at 80 columns. Before, a path with spaces could hide the reason. The error carries
  the exact span of the path it names, so a path with spaces is shortened from the left as one
  path. The error text is unchanged, and a message that fits is unchanged (#232,
  [ADR-0150](docs/adr/0150-name-the-path-a-status-line-error-carries.md)).

- Setup help and guides now describe what the CLI does. `af onboard --help` and the generated
  `.af/README.md` say that `--refresh-lock` re-pins every route and oversized-policy pipeline as
  well as the selected one, with their Workers, and drops stale pins. The Task prerequisites say
  that `install.sh` checks the release signature only when `minisign` is on `PATH`, apart from a
  checksum-only install. The preflight guide separates a successful empty-Diff `af review plan`
  from the `af review run` refusal and the helper's `empty_intended_diff` conflict. The
  shared-catalog guide lists all eight installed Task-kind profiles. `af catalog --help` and its
  `--destination` flags say the destination must be an absent, safe project-relative path. No
  behaviour changes.

- `af project`: a repository now has one Project identity, derived from its root commits and never
  from a path or a remote URL, with a Project Store under `$XDG_STATE_HOME/af/project/`. Its
  checkouts are Workspaces, each with its own Task Store, Task files and bindings file. `af
  project show` and `af project workspaces` print them (`--json`: `af/project@1`,
  `af/project-workspaces@1`). `af project workspace new LABEL` makes a worktree, a branch and a
  Workspace directory without a commit, filling the worktree the way `af task deliver` does, so no
  checkout, filter or hook of the repository runs; it refuses a `--path` inside the checkout,
  another Workspace or af's state (exit 2) and a `--from` commit of another history before it
  makes anything, and undoes what it made when a later step fails, deleting only what it can
  prove it made: the worktree directory its own `create_dir` made, only while it keeps that
  identity, every entry in it, an empty directory as much as a file, is a path of the captured
  commit equal in kind, mode and content or a directory above one holding nothing else, its
  `.git` is absent or names this add's entry and the index the add wrote is unchanged, otherwise
  keeping the worktree and naming the first differing path; only the parent directories it
  created itself, each only while it is still that directory by device and inode, a replacement
  at its path being left and reported; administrative entries whose `gitdir` names its
  worktree; the branch only while
  it still holds the commit it was created at and no kept worktree is on it. Every other new
  entry is reported as left behind with one fixed recovery line, also when the undo's final check
  fails, and `git worktree prune` never runs. `workspace add PATH` adopts an existing checkout,
  and `workspace remove LABEL` unregisters one and never removes a Store; `--delete-worktree`
  removes a worktree only when its index and its files both equal its commit, the index through
  delivery's cached-index comparison and the files through the committed capture rather than
  `git worktree remove`, so no clean filter runs and a staged or unmerged entry is refused. It
  goes by identity, never by name: before it moves anything it writes a machine-local removal
  record in the Project Store (`removals/<workspace id>.toml`) holding the checkout path, the
  device and inode of the checkout and of its administrative entry, the container it makes for
  each and the step each reached; it claims each directory into a container it creates
  exclusively beside it (mode 0700, through that container's own descriptor, never over a taken
  name), judges the checkout there through descriptors without following a link, deletes only
  what it claimed and then the container with `rmdir`. A replacement at the checkout's path is
  kept and reported, a container holding anything else keeps it and is reported with the first
  such path, and a directory merely named like a container is reported and never touched. An
  interrupted or failed removal says where what is left remains, keeps the Workspace registered
  and is finished by the same command from its record, which unregisters the Workspace exactly
  once. The rerun never takes a recorded step as proof: whatever the step, it first checks the
  administrative entry now, opened without following a link and read only while its device and
  inode are the ones the record holds, so another directory put at its path is a replacement,
  reported and never read; while the entry is replaced or locked or its `gitdir` names another
  checkout it touches nothing, a checkout already in its container staying there, reported by
  its claim name, and a failed check never deletes the removal record, so the next run goes by
  the recorded identities rather than inspecting afresh; then it checks that step's
  precondition on the file system, so a checkout moved back out of its container is claimed and
  judged again and a replacement at its path is kept, and an entry is claimed or deleted only
  while its `gitdir` names the checkout and it holds no `locked`, whatever the step; an entry that
  is locked, names another checkout or lacks its `gitdir` stays where it is, reported, and so does
  the Workspace's registration, as while an entry's `gitdir` cannot be read; a plain `workspace
  remove` is refused meanwhile. Automatic Workspace labels follow one stated rule within 128 bytes. `af
  onboard --apply`, `af task plan|start`, `af review run` and `af project` register the Project
  and the checkout lazily (`af task plan|start` before resolving `--workspace`), refuse a shallow
  clone with `git fetch --unshallow`, and never change a command's result; every git process they
  start runs supervised under a deadline. `af task deliver` registers the worktree it makes.
  `--workspace LABEL` on every `af task` command that takes `--state` selects a Workspace's Store
  and bindings. `af storage --json` is now `af/storage@2`, with the entry kind `project_store`,
  and `$XDG_STATE_HOME/af/stores.toml` is version 2: a version-1 file is refused until it is
  deleted, and Stores register again as commands use them
  ([ADR-0158](docs/adr/0158-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)).
- PJ1b close-out: this branch did not have it yet, so it is added here. Every `*-moved` step of
  `af project workspace remove --delete-worktree` checks its original path: stopped after
  `entry-moved` (or `checkout-moved`), a directory found at the entry's (or checkout's) original
  path is a replacement that stops the removal, the recorded directory staying in its container,
  nothing deleted and the Workspace registered, reported as `a new administrative entry appeared
  at <path> since this removal moved the recorded one; clear it or move the recorded entry back,
  then run the command again`; cleared or moved back, the same command finishes
  (`a_directory_at_the_original_path_after_any_moved_step_stops_the_removal`,
  [ADR-0158](docs/adr/0158-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)
  clause 5).
- Second close-out: a rerun of `af project workspace remove --delete-worktree` checks the
  administrative entry as its recorded step needs. Identity, `gitdir` and lock are checked before
  a live checkout or entry is claimed or deleted; an entry whose deletion is recorded resumes in
  its container without the `gitdir` that deletion may have removed; and one gone from its
  container and its path, or recorded deleted, advances to done once its recorded container is
  verified. So a crash right after the entry's unlink, part way through its deletion or after its
  container's removal finishes on the rerun and unregisters the Workspace once, also beside a new
  entry another add made at its path, which is kept
  (`a_rerun_after_the_entrys_unlink_or_part_of_its_deletion_finishes_beside_a_new_entry`, and the
  stop points `entry-unlinked`, `entry-partial`, `entry-rmdir`, `checkout-unlinked` and
  `checkout-rmdir` of
  `an_interrupted_removal_is_recorded_by_identity_and_a_rerun_finishes_it_unregistering_once`).
  Every identity the removal records carries the directory's birth time beside its device and
  inode, since ext4 hands a freed inode number to the next directory at once: the removal record
  is `version = 2` and a version-1 record is refused, an identity at the same inode born later is
  refused, and a file system that reports no birth time refuses the removal with nothing changed
  (`a_file_system_without_birth_time_refuses_a_removal_and_changes_nothing`); the undo of
  `workspace new` compares the same identities. The Project ADR is renumbered ADR-0158, after
  the numbers main took
  ([ADR-0158](docs/adr/0158-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)
  clause 5).
- Third close-out, Workspace removal: a completed `--delete-worktree` removal record now outlives
  the Workspace's unregistration and is deleted only after `WorkspaceRemoved@1` is appended and
  its paths dropped. A crash before the unregistration, or after it and before the record's
  deletion, is finished by the rerun from the record, never by inspecting the path again, so a
  clean worktree created at the same path since is left untouched and exactly one
  `WorkspaceRemoved@1` is recorded; registering that path again settles the finished record first
  (`a_completed_removal_record_outlives_the_unregistration_and_a_replacement_survives_the_retry`)
  ([ADR-0158](docs/adr/0158-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)
  clause 5).

- `af task output --format human-markdown` writes a readable presentation of a Document or
  report for a person: its sections with lists and examples, sources and citations, ordinary
  punctuation and non-ASCII text as written, the Task's result in plain words
  (`accepted`, `changes requested`, `incomplete`) and the reviewer's summary when an exactly
  matching receipt exists, recorded under the Task revision's own plan, verifier policy and
  source Manifest, and whose verifier received any captured sources; stale evidence refuses the
  export. Its closing note names only the checks and review that actually judged the canonical
  report. It is derived from the draft that renders exactly to the recorded report and is never
  recorded or reviewed itself. Authored Markdown, HTML and links stay text; an authored example
  keeps a fence line indented four columns, or padded by a no-break or other non-ASCII space,
  as content. Only ASCII spaces and tabs are layout: a no-break, em or ideographic space stays
  as written in prose, lists, headings and after the indentation of an indented example. A
  captured source's link opens exactly its captured URL: a query such as `?q=&copy;` is never
  decoded to `©`.
  `--format markdown` and `--format json` are unchanged
  ([ADR-0151](docs/adr/0151-export-a-human-presentation-of-a-report-beside-its-canonical-markdown.md)).

- Telemetry, first package: a new leaf crate, `review-telemetry`, records what `af` does in its
  own store under `$XDG_STATE_HOME/af/telemetry/`, never in a Task or Campaign Store, and the
  kernel never reads it. The machine-only `[telemetry]` table (`level` = `off`, `metrics`,
  `spans` or `content`, and its bounds; `AF_TELEMETRY__<KEY>`) is ignored and reported in a
  repository's `.af/af.toml`, and `af help telemetry` documents it. `af telemetry status` and
  `af telemetry reindex` (`--json`: `af/telemetry-status@1`, `af/telemetry-reindex@1`) show and
  rebuild the store; `status` accounts every byte beneath it, a path it does not recognize
  under an `unrecognized` entry it never reads or removes, and rechecks each registered
  directory's overlap, `unknown` when the Store registry cannot be read. The credential rules for Provider-authored text moved into the crate
  unchanged, with key-aware JSON redaction added; only a kernel identity in an identity position
  escapes them, and `status` re-reads a sealed segment changed under its name. No command records
  spans yet
  ([ADR-0153](docs/adr/0153-record-telemetry-in-its-own-store-and-never-let-it-steer.md)).

- `af project survey|refresh|history`: the token-free Surveyor measures a Project's head into a
  Survey, `af/ProjectSurvey@2` in the Project Store, with the sections identity, tree, history,
  contributors, decisions, vocabulary and checks, every number decimal text, shares basis points
  rounded down (`3333` for one third of three), and each section complete, partial or unavailable
  with the bound it reached. A complete Survey is reused only while its survey key is unchanged (the
  head commit, the digest of one bounded `for-each-ref` observation of `refs/heads` and `refs/tags`,
  which also yields `history`'s tags and branches, the policy digest and Surveyor version 8), taken
  from the record; replay binds every `SurveyRecorded@1` to its record, or a collected Survey's to
  its tombstone, which copies every identity field of the record (head, refs, cutoff, completeness,
  policy, Surveyor version and previous Survey) once the event was verified against it, and `af
  project history`, `af project survey` with and without `--section` and `af project refresh` refuse
  a mismatched event, an older one included, naming its sequence and field, before any of its fields
  is used. The fold refuses before it reads: a record of an uncollected Survey that af cannot read
  back, gone or corrupt, refuses replay naming the event's sequence, the Survey and the read error
  and none of the event's other fields, and collection refuses and reports a Survey whose event
  disagrees with its record. `unchanged:` names the key in one line; a new tag or branch, a policy
  change or a partial newest Survey records a new one, and `af project refresh --force` records one
  regardless. It reads the head only through supervised git plumbing, the history from objects alone
  in a private bare repository attached to the object database (so untracked or `.git/info`
  attributes, local configuration and diff drivers change no count), every stream as it arrives and
  no record past 64 KiB, under one absolute `[survey] max_wall_ms` deadline that starts before
  `rev-parse`, covers the survey key (a key not computed in time records `SurveyFailed@1`, and so
  does every other way a refresh of a registered Project stops without a Survey: a newest Survey
  that does not replay, a record the Project Store cannot take, a deadline before `rev-parse`
  answered, whose head one more one-second `rev-parse` names), bounds every git process at the
  lesser of its own limit and the time left (its process group ended at the deadline, a silent git
  included), and is checked at every item of every stream, every declaration and every match of
  every pattern list, `[survey.languages]` included, and after every section's finishing step, so a
  slow section stops partial, never late and complete. The Tree Digest comes from a digest-only path
  equal to the committed capture's digest: no blob written to any CAS, at most 16 MiB of manifest
  buffering and 256 MiB of temporary manifest data, and absent with `deadline`, `max_record_bytes`,
  `manifest_bytes` or `temp_bytes`, never the digest of a part; its spilled runs live in an
  `af-tree-digest-<pid>-*` directory that the crash sweep of sandboxes and check runtimes reaps
  under the same liveness and preservation rules, and every refresh sweeps a killed one's first,
  before it decides to reuse a Survey, and again after its Tree Digest on every way out, within its
  deadline and never touching a live process's directory. A Survey is published through a
  pending-publication entry beside the Project Store's lock (`af/survey-pending@1`, at most 4 KiB:
  the Survey ID and its length, written and synced before any byte enters the CAS, then the device
  and inode of the file at the Survey's CAS name, recorded and synced once the bytes are durable
  there and before the event), cleared only once `SurveyRecorded@1` committed, so a crash between
  the record and its event leaves the entry, which collection counts and recovers, and the next
  refresh of the same bytes finishes it; debug builds stop at both crash points
  (`AF_TEST_SURVEY_STOP_BEFORE_IDENTITY`, `AF_TEST_SURVEY_STOP_BEFORE_EVENT`). A complete Survey is
  byte-identical for the same head, refs, policy, Surveyor and git versions, cutoff and previous
  Survey, and one that is not complete records where it stopped (`stopped`: the section, the item
  index, the commit or path, the reason), which `af project survey` prints. The history walk
  includes merges with `--diff-merges=off`, so a merge counts as a commit and a merge with no churn,
  and reads `git log -z` in NUL-separated fields with the commit ID repeated, so an author name or
  subject holding 0x1f is read whole and a record that does not parse, or a log that ends before its
  last record's NUL, stops the walk with `malformed_record` naming its commit, never a guessed
  identity, never complete history and never `SurveyFailed@1`; a malformed first record leaves
  `history` and `contributors` partial with zero counts, naming the head, and both sections name the
  record that stopped the walk by its ordinal (`stopped_record`, the one after the commits counted),
  a record git is still writing being awaited until its own terminators and only the stream's end
  cutting one. A gitlink is one file of zero bytes and lines, language `other`, never fetched or
  opened, and left out of the Tree Digest as the committed Manifest leaves it. `max_paths`,
  `max_entries` and `max_survey_bytes` are ceilings a layer may only lower, every pattern list holds
  at most 64 patterns, the history walk tracks at most `max_paths` paths, and every list is ranked
  before `max_entries` cuts it with `seen` and a reason, while display strings are cut to 256 bytes
  and marked `truncated` and identity strings stay exact. Every `[survey]` pattern uses the one
  grammar of `.af/af.toml` (brackets literal, `?` one byte), a decision's status and date come from
  its `**Status:**` or `Status:` line alone (the first status named, `amended` a flag, the first
  date that is a day of the calendar), terms are listed by name, `[survey.languages]` patterns give
  each file one language, the bytewise-first ahead of the built-in map, checks come from
  `.af/code-policy.toml` and every `.af/pipelines/*.toml` as `<source>:<name>` with the eight listed
  toolchain files, and the measurement policy is stated. Contributors carry a `key` they are grouped
  and ranked by beside their bounded display `name`, and are split into human and agent-assisted
  commits, each counted once; an author email is never stored, and an author name carrying an email
  address, blank or holding a control character anywhere in the whole name is `h:` and the hash of
  the name under both identity policies. `af project survey --section NAME` picks sections of the
  text; with `--json`, which prints the complete record, it is usage error 2. `af onboard --apply`
  records the first Survey without changing its own exit code, and `af project show --json` stays
  `af/project@1`
  ([ADR-0159](docs/adr/0159-measure-a-project-into-surveys-with-a-token-free-surveyor.md)).
- PJ2 round 9 re-verified round 8, which had no code review, item by item: (i) a log that ends
  inside a record is `malformed_record`, partial with the counts before it and zero counts for the
  first record, `unavailable` kept for a walk that never ran: fixed, adding the record's ordinal and
  the stated, tested difference between a record still being written and a cut one
  (`a_record_git_is_still_writing_is_awaited_and_only_the_streams_end_cuts_it`,
  `a_log_stream_cut_before_its_final_nul_is_partial_history_naming_the_cut_commit`); (ii) the spill
  sweep on every refresh, before and after the Tree Digest, sparing a live Surveyor's directory:
  fixed, the sweep after the digest was missing
  (`the_spill_sweep_runs_after_the_tree_digest_on_every_way_out`); (iii) a gitlink as one file of
  zero bytes and lines in `files_without_lines`, language `other`, never fetched or opened:
  complete, its fixture now also proving no `cat-file` asks for the target
  (`a_gitlink_is_one_file_of_no_bytes_or_lines_never_fetched_and_digests_as_the_manifest`); (iv)
  `previous_survey_id` and the partial stopping point as reproducibility inputs: complete
  (`forced_refreshes_at_one_cutoff_differ_only_in_the_chain_and_a_partial_one_in_where_it_stopped`);
  (v) the ceilings: `max_paths` complete
  (`the_fixed_maxima_cut_with_their_reasons_and_identity_strings_stay_exact`,
  `a_historical_commit_over_max_paths_leaves_history_partial_and_the_survey_recorded`), the 64 KiB
  `max_record_bytes` fixed with its missing end-to-end fixture
  (`every_streamed_record_over_64_kib_stops_at_max_record_bytes`), while the discovery ceilings and
  the activity CAS-read budget belong to the `activity` section, which is PJ5's and not on this
  branch, so neither was found complete nor fixed here and ADR-0159 clause 12 states them for PJ5;
  (vi) every persisted shape at its version with its schema and parity fixture: complete, every
  Survey shape still unreleased at `@1` (this round's `stopped_record` changes
  `af/ProjectSurvey@1`'s schema and parity in the same change) and Surveyor version 8 so no earlier
  build's record is reused. The writer's publication order was fixed too: the CAS made durable
  before the identity, every entry write's directory synced, the identity only of a regular file of
  the record's length, the 4 KiB bound and the second stop
  (`the_writers_pending_entry_is_what_collection_reads_at_either_crash_point`), and every early stop
  now records `SurveyFailed@1` (`every_refresh_that_stops_without_a_survey_records_its_failure`).
- PJ2 close-out, the writer: the Survey record is `af/ProjectSurvey@2`, since round 9 added
  `stopped_record` to `@1` without a new version; `ProjectSurveyV2`, its schema
  (`schemas/project-survey-v2.json`) and parity fixtures replace `@1`, whose schema is gone, and
  the label `af/ProjectSurvey@1` is refused by type with ADR-0145's message
  (`the_superseded_survey_label_is_refused_by_type_and_schema`). A record of it already in a
  Project Store is never deleted for its version: `af project history` lists it by ID and time as
  `unreadable: superseded version` (`--json` is now `af/project-history@2` for that entry),
  `af project show` says so, `af project survey` refuses it naming the version, and a refresh
  never reuses it (`a_superseded_survey_record_is_listed_unreadable_kept_and_never_reused`). A
  refresh tries `project.lock` only until its `max_wall_ms` deadline and stops with the bounded
  failure `store_locked`, recorded while the holder still holds the lock
  (`a_held_project_store_lock_stops_a_refresh_with_store_locked_within_its_deadline`). A retry of
  a Survey whose pending entry already names an object keeps that identity and goes on only while
  its CAS name holds that object, so a retry whose CAS write fails leaves the entry naming the
  first object for collection to reclaim
  (`a_retry_whose_put_fails_keeps_the_first_objects_identity_for_collection`), and the writer
  refuses a `pending-surveys` that is a symbolic link or no directory of the Store, naming it and
  writing nothing through it
  (`a_pending_directory_replaced_by_a_link_refuses_the_refresh_and_nothing_is_written_outside`)
  ([ADR-0159](docs/adr/0159-measure-a-project-into-surveys-with-a-token-free-surveyor.md) clause
  15).
- Second close-out, the writer: a Survey's bytes are staged in the writer's own
  `pending-surveys/<64 hex>.object`, its identity recorded in the pending entry, and only then
  renamed to its CAS name, never over a taken name, so nothing is ever at a CAS name no entry
  describes and every state between has its recovery: a refresh stopped before the identity
  (`AF_TEST_SURVEY_STOP_BEFORE_IDENTITY`) leaves staged bytes and an empty CAS name, one stopped
  after it (the new `AF_TEST_SURVEY_STOP_BEFORE_LINK`) an entry naming the staged object, and the
  next refresh of the same bytes finishes either
  (`the_writers_pending_entry_is_what_collection_reads_at_either_crash_point`). The entry is now
  `af/survey-pending@2` (`schemas/project-survey-pending-v2.json`), whose `object` carries the
  birth time `born_unix_ns` beside device and inode, since ext4 hands a freed inode number to the
  next file at once; `@1` is refused naming its recovery, and a file system that reports no birth
  time refuses the publication. The Surveys ADR is renumbered ADR-0159, after the numbers main
  took
  ([ADR-0159](docs/adr/0159-measure-a-project-into-surveys-with-a-token-free-surveyor.md) clause
  2).

- Surveys join the Storage Budget: every Survey is an `af storage` entry of kind `survey` with its
  allocated CAS bytes, last used at its cutoff (`af/storage@3`, `af/storage-prune@2`; the
  superseded `af/storage@2` schema is gone and both earlier versions are refused, while the Task
  sweep record keeps its kinds and counts a Survey's removal without listing it). An explicit `af
  storage prune --apply`, or the sweep after a run when `[storage] auto_gc = true`, collects the
  Surveys of each Project beyond the newest `[storage] keep_surveys` (50,
  `AF_STORAGE__KEEP_SURVEYS`), oldest first and idle at least `keep_days`, and the budget may
  take any Survey, oldest first, while af is over `max_bytes`; neither takes the newest complete
  Survey, one the Project Store's lock holds or one used within the hour, and `af project
  refresh` collects nothing. Collection appends a `SurveyCollected@1`
  tombstone with the Survey's head, cutoff, completeness and tree and history totals before it
  deletes the record. A sweep plans once over one inventory snapshot: `af storage prune` prints
  that plan, every removal in order with the bytes it frees, its rule (`recovery`, `collection`,
  `keep_surveys`, `budget`) and the total after it, and under `left` what it leaves (`held`,
  `foreign`, `oversize`, `unreadable_name`, `too_deep`, `unmeasurable`); `--apply` runs the same plan entry by
  entry, keeps an entry of any kind whose removal failed (the lock lost, what it names replaced
  or gone since the inventory, an I/O error) in its place as `outcome` `failed: <reason>` with
  nothing freed and the running total unchanged instead of choosing again, while a deletion that
  stopped part way, a failed final `rmdir` included, reports the bytes that actually left the disk,
  accounted from the removal's own start (what the directory held immediately before anything was
  unlinked less what its claim still holds), and so stops where its preview stops, a pending recovery's bytes
  counted once and its totals what af then holds. A directory nested deeper than the removal
  walk's 128 levels is left `too_deep` by preview and apply alike and listed in use by `af
  storage`. The claims dead processes left (`.af-removing-*`) and the containers of dead passes
  are in that one inventory, `claim` entries under `recovery` that the preview and the applied
  sweep list alike with the same bytes, a container that held a moved record planned after it,
  and nothing is removed beside the plan; every one is an `af storage` entry of kind `claim`,
  `state` `abandoned`, with its measured bytes, so the listing's claims are the preview's claim
  recoveries, and its bytes are counted once: in its kinds and total as the preview counts it,
  or, for a claim inside a Project Store, in the Store's bytes, the claim saying so with
  `counted_in`, while one the inventory
  cannot measure is left `too_deep` or `unmeasurable` with no bytes and never touched. A registration whose path holds no Store is dropped by the plan
  (`registry_dropped`, the same in preview and apply), after the inventory reached its parent's
  claims, so a registered Store a dead process renamed into its claim is recovered alike by
  both. Each uncollected Survey's `SurveyRecorded@1` is verified against its record once, in the
  Project Store's one read, and the plan and the inventory read only what that verified: an event
  whose cutoff its record disproves is never the Survey's last use, and the Survey is refused and
  kept. A Survey's record is measured once by device, inode and bytes, and one replaced between
  inventory and the plan is `refused (replaced since inventory)` and never adopted. A record is reached and read only through directory descriptors that follow no link, and
  deleted inside a container each job creates for itself, `cas/removing/<pass-id>/` (mode 0700, a
  fresh random ID), where it is stated again relative to the container just before the unlink, so
  only the inode found before the tombstone is deleted and a replacement is refused, put back,
  and the Survey reported `refused (replaced during collection)`; the next sweep collects it. The
  next sweep also settles a dead pass's container the same way: a file that took a collected
  Survey's CAS name after its record was moved, or that holds bytes other than the record's, is
  `foreign`, listed with its bytes, never counted as the Survey's and never deleted; once a
  Survey's record has left the CAS the Project Store's `settled.json`
  (`af/ProjectSurveysSettled@1`) says so, so a byte-identical file at its name stays `foreign`
  sweep after sweep, and a settlement whose directory sync fails puts the earlier record back and
  deletes nothing, for the next sweep to retry. A crash between a Survey's bytes and its
  `SurveyRecorded@1` leaves the writer's pending-publication entry
  (`pending-surveys/<64 hex>.json`, `af/survey-pending@1`) that `af storage` counts as a Survey
  `pending` and recovery settles: cleared when the event committed, otherwise only the object it
  describes (device, inode and length, checked again under the lock, `skipped: object changed`
  otherwise) is reclaimed, the entry cleared only once that object is gone, so a reclamation
  interrupted inside its pass's container is finished by the next sweep, never left `foreign`;
  any object nothing names is `foreign`; and a
  leftover whose name, or whose container's name, is not UTF-8 is walked with its raw name, listed
  `unreadable_name` (its `--json` path percent-encoded) and left with its container. A CAS prefix directory replaced by a symbolic link
  refuses its Survey, which is reported and keeps its record and its place in the log; the
  Surveyor, the reader of a Survey and collection share one 1 MiB ceiling, and a record over it is
  left as `oversize` with its size, while a corrupt record within it is collected with an
  `unreadable` tombstone in place of its totals and its bytes freed, shown as `collected
  (unreadable)` by the sweep and `af project history`; one the file system will not let af read
  is refused and left as it was. The preview and the sweep decide `held` by one predicate,
  `project.lock` taken as the sweep takes it: the Surveys of a Project Store a refresh or a plan
  holds, or whose lock af cannot open (`lock unavailable: <error>`), are left `held` with the
  same reason by both, and nothing is collected. `af project history` marks a collected Survey
  `collected` from its tombstone, and `af project survey` says that a collected newest Survey was
  collected and names `af project refresh`. `.af/af.toml` patterns, the routes' and the
  Surveyor's alike, now match by dynamic programming with the same grammar, `?` one byte of the
  canonical encoded spelling, so a match costs at most (pattern length + 1) × (path length + 1)
  cells and `*a` thirty times then `b` no longer hangs; a `[survey]` pattern is at most 256 bytes
  and a list at most 64 patterns, and the Surveyor checks its deadline before each pattern it
  matches ([ADR-0159](docs/adr/0159-measure-a-project-into-surveys-with-a-token-free-surveyor.md)).
- PJ2c close-out, collection: a pending entry whose event never committed is cleared only once no
  file with its inode remains at its Survey's CAS name or in a pass's container, whatever its
  length; an object truncated or grown in place, or moved so, keeps the entry (`failed: … kept:
  object changed, still present`) and is listed `foreign` until it is gone
  (`a_pending_entry_stays_while_its_changed_object_is_present_and_goes_once_it_is_deleted`).
  When the directory sync after `settled.json` is installed fails, the put-back is synced too
  before the pass goes on, and when that fails as well both failures are reported and the
  Survey is left `unverified settlement`, its record not deleted until a later sweep reads a
  consistent `settled.json` (`a_settlement_whose_rollback_sync_fails_too_is_left_unverified_and_deletes_nothing`).
  Where the inventory walks Stores it holds to the Store discovery ceilings: the Surveys of at most
  1,000 Project Stores, and each Store's log read through one cursor of at most 100,000 rows, 1 MiB
  per row and 16 MiB over all rows; a Store past one is reported naming the ceiling and its
  Surveys are kept, one fixture per ceiling
  (`project_stores_beyond_the_store_discovery_ceiling_are_reported_and_their_surveys_kept`,
  `a_project_store_log_beyond_the_row_ceiling_keeps_its_surveys`,
  `a_project_store_row_beyond_the_row_bytes_ceiling_keeps_its_surveys`,
  `a_project_store_log_beyond_the_buffered_bytes_ceiling_keeps_its_surveys`). The 64 MiB
  per-Store activity CAS-read budget is out of scope here: it bounds the `activity` section, which
  does not exist yet and belongs to PJ5
  ([ADR-0159](docs/adr/0159-measure-a-project-into-surveys-with-a-token-free-surveyor.md) clause
  15).
- Second close-out, collection: before its tombstone a collection records the object it selects,
  its device, inode, birth time and length, in `settled.json`, now `af/ProjectSurveysSettled@2`
  (`selected`, `schemas/project-surveys-settled-v2.json`; `@1` is refused naming its recovery),
  and recovery follows that identity, never a hash: a byte-identical copy put at the name after a
  crash that followed the tombstone, another inode or the same one born later, is `foreign` sweep
  after sweep (`a_byte_identical_replacement_after_a_tombstone_survives_two_sweeps_as_foreign`,
  `a_record_replaced_before_its_rename_is_refused_and_its_copy_stays_foreign`). Objects the writer
  staged are reclaimed under `recovery` before their entries
  (`a_survey_stopped_before_its_cas_name_leaves_staged_bytes_a_later_prune_reclaims`), and a file
  system that reports no birth time refuses collection and deletes nothing
  (`a_file_system_without_birth_time_refuses_collection_and_deletes_nothing`). Every byte count
  of `af storage` and `af storage prune` is the allocated bytes of regular files, directories and
  links counting nothing, so the numbers agree on ext4 and APFS. The prune's running total
  reconciles a claim that grew since inventory, what its removal measured at its start taking the
  place of the inventoried bytes before what left is subtracted (inventoried at 10 MiB, grown to
  20 MiB, 8 MiB freed: 12 MiB retained, `a_grown_claims_partial_deletion_reports_the_bytes_it_retains`),
  and reports the total and `stop` `unknown` when what a partial deletion left cannot be measured
  (`a_partial_deletion_whose_remainder_cannot_be_walked_reports_the_total_unknown`), so its
  document is now `af/storage-prune@3` (`schemas/storage-prune-v3.json`) and `@2` is refused
  ([ADR-0159](docs/adr/0159-measure-a-project-into-surveys-with-a-token-free-surveyor.md) clause
  13).
- Third close-out, collection: a corrupt record collected `unreadable` whose pass stopped after
  its tombstone and before the record's move is reclaimed by the next sweep under `recovery` by
  the object its collection selected (device, inode, birth time and length) alone, since its
  bytes never hash to its Survey ID, while a file that took its name since, the same bytes and
  length included, stays `foreign`
  (`a_corrupt_record_interrupted_after_its_tombstone_is_reclaimed_by_its_selection`). The sweep
  that ends a Task's run is now recorded on the Task whenever it removed or failed anything, its
  total after it unknown included: `af/TaskStorageSweep@2` (`schemas/task-storage-sweep-v2.json`)
  spells `total_after` as its bytes or `unknown`, adds the stop `unknown` and `freed_bytes`, the
  bytes that left the disk, and `af task show` prints `→ unknown`; `af/TaskStorageSweep@1` is
  refused as a superseded version, so a Task log holding one does not replay
  (`a_sweep_whose_remainder_cannot_be_measured_leaves_a_replayable_receipt_on_its_task`)
  ([ADR-0159](docs/adr/0159-measure-a-project-into-surveys-with-a-token-free-surveyor.md) clause
  13).

- `af` checks for a newer release after eligible headless and `--json` commands too, not only on a
  terminal: a detached `af self refresh-check`, at most once per `[self] check_every`, a failed
  query or a self state that cannot be written included, that never changes the command's output
  or exit status, and that a command whose versions lock is held by a prune, an installation or
  an uninstall skips rather than waits for. Inside a pinned project
  the dispatching `af` schedules it once. The cached notice still reaches stderr only on a
  terminal and never under `--json`, and clearing it never overwrites a concurrent check's cache;
  a check that another one answered while it waited asks nothing, and an installation, `always`
  included, no longer holds the versions lock while it downloads, so a pinned command never waits
  for it, nor does `always` overwrite a default the operator changed meanwhile, an update then a
  rollback to the same version included; a seen pin is never recorded without the versions lock;
  `af self remove` and `af self prune` recheck the default and the pins under that lock and delete
  under it, so a concurrent activation never leaves the default pointing at a deleted release; an
  "updated the default" notice a later update or rollback made stale is cleared unshown; a pinned
  command on a terminal shows the cached notice before it dispatches, and the notice is bound to
  the activation that made it, so an update back to the same version after a rollback drops it too
  (the cache is now `af/self-check@2`; an earlier release's cache is read as none);
  `af self uninstall`, with or without `--purge`, refuses while a background check runs, and
  neither a check nor an installation that was under way recreates what it removed; a dispatch
  rechecks the pinned release's bytes and records its pin in one versions-lock critical section,
  so `af self remove` or `prune` can no longer delete it in between, and runs the release only once
  that pin is written, failing closed when the self state or the versions lock cannot be used; a
  command still running when `af self uninstall --purge` ran recreates nothing it removed; an
  "updated the default" notice is printed only once clearing it is written, so a failed write never
  shows it twice; a killed installer's staging
  counts
  against the Storage Budget until the next installation recovers it; `CI`, `AF_SELF_OFFLINE`,
  `update_check = false` and `auto_update = "never"` still turn the check off ([ADR-0154](docs/adr/0154-check-for-updates-after-headless-and-json-commands.md)).

- The identity-recheck deadline test's fixture shell now publishes its PID marker by writing a
  temporary file and renaming it, so a deadline kill leaves no marker or a complete one, never
  the empty file that failed a kernel gate with `ParseIntError`; a new regression pins it
  (issue #278, [ADR-0155](docs/adr/0155-publish-a-fixture-marker-whole-or-not-at-all.md)).

- `make test`: two runs in one checkout, even overlapping ones, each print the test-time
  summary of their own tests. Each run writes its JUnit into a private store under
  `target/nextest-reports/`. It then copies that JUnit to `target/nextest/ci/junit.xml`, which
  CI uploads, in one rename, or removes that file if it wrote none; runs take turns, so the
  file is the last finished run's. Before, a run could print the totals of another run that
  wrote the shared file first. Exit statuses are unchanged (#219,
  [ADR-0156](docs/adr/0156-give-every-make-test-run-a-private-junit-store.md)).

- Project Briefs: a pipeline's root port `root_default = "project_brief"`, `brief = "<variant>"`,
  declared in an `af.pipeline/2` pipeline (the new version of the pipeline shape; an
  `af.pipeline/1` pipeline carrying the port is refused, and every other pipeline keeps
  `af.pipeline/1`), carries `af/ProjectBrief@1`, a bounded selection of the Survey the Task pinned
  when it was planned, into the Workers that wire it and no other, each listing it in its context
  manifest (`required_by` `port:<name>`). Variants are `[brief.<name>]` tables of `.af/af.toml`
  (`sections`, `max_bytes` 1,024 to 262,144, `decisions`, `hot_paths`; built in: `orient` and
  `none`), refused at policy load outside their closed schema. Whole sections are dropped,
  last-declared first, until the exact canonical bytes fit; a Brief that cannot fit fails the plan.
  The initial capture pins the Survey of the Project of the resolved source Snapshot (a `source`
  bound from another repository gets that repository's Survey or an empty port, never the invoking
  checkout's) and records `af/ProjectSurveyPin@1` and `af/ProjectBriefCapture@1`, so a refresh, a
  removed Project Store or a deleted checkout changes nothing; each plan receipts its Brief ports
  (`af/ProjectBriefReceipt@1`) with `behind_commits` observed at capture, which `af task plan` and
  `af task explain` print. A plan whose Worker request, serialized as the runner renders it, would
  exceed the 1 MiB cap with the Brief is refused before dispatch. No author email enters a Brief,
  while repository text such as a decision title stays verbatim. A Task file may bind the port to a
  recorded Brief output, which reads no Survey and pins none; a Brief the kernel did not
  construct, a Worker output among them, is refused (`not_kernel_constructed`). An empty port's
  `no_survey` receipt names the source commit. `af project brief [--variant NAME] [--survey ID]
  [--json]` prints the exact bytes a plan would bind (`af/project-brief-preview@1`), recording and
  registering nothing: it opens the Project Store strictly read-only, so it changes no byte there
  and reads a Store nobody may write
  ([ADR-0160](docs/adr/0160-carry-a-bounded-project-brief-on-a-declared-port.md)).

- `af catalog digest PACKAGE_DIR` prints the digest a catalog pin records for one local Task
  package: one `sha256:` line, from the same recipe and validation Task capture uses. Every
  regular file under the directory contributes its relative path and bytes. An absent, incomplete
  or malformed package prints nothing on stdout, says why on stderr and exits 1. The command
  writes nothing, runs no Worker, contacts no Provider and is never dispatched to a pinned
  release nor followed by an update check, so it installs and activates nothing. It refuses a
  package past capture's 4,096 files or 16 MiB by each file's size before reading it. Package
  reads now open every entry descriptor-relative without following symlinks, a registry's
  package root included, so an entry swapped mid-read is refused rather than read from outside
  the package, and a package root replaced after its check, even by another valid package, is
  refused rather than digested; a symlinked package root is refused however it is spelled,
  `LINK/` and `LINK/.` included (#150,
  [ADR-0157](docs/adr/0157-print-a-task-package-digest-with-the-pin-recipe.md)).

## [0.12.0-rc.4] - 2026-10-16

### Authority compatibility

Committed .af/ policy keeps working as is relative to 0.12.0-rc.3; no authority migration is required. Model bindings are now validated before any Provider process starts: a binding that names an alias (opus, fable), an effort level the model does not list, or a misspelt ID is refused with the exact spellings to use (af provider models <id>); custom IDs that pass the spelling rule are accepted as unverified.

### Changes

- release: v0.12.0-rc.3 (#261)
- ci: authenticate gh in the release pin job (#262)
- Stop the Claude model fallback and name the model that answered (#264)
- Model discovery: af provider models, one validation authority, hints and TUI (#268)

- Claude Task Attempts now run with `CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1`, so Claude Code no
  longer answers with another model when the bound model's safety classifier flags a request. A
  refusal is classified `model_refused`, charged what it reports and not retried; a reply from a
  model the Worker did not bind is still refused, now naming both models, their output tokens,
  `safety_stops` and `num_turns`; a model ID Claude Code does not know is `model_unavailable`.
  `af task run` and `af review run` print the reason and the next step on stderr for each such
  failed Attempt, the switch first even when the result's usage is also malformed or lists more
  than 32 models, with those accounting reasons after it in the diagnostic; an entry for another
  model that reports no activity, or only malformed counters, is never a switch. An error result whose `result` carries Claude
  Code's safeguards text is `model_refused` whatever its `errors` holds. A model or effort binding
  outside the one spelling af prints (an ASCII letter or digit, then at most 127 letters, digits,
  `-`, `.`, `_` or `:`) is refused before any process starts, naming only its byte length, ahead
  of the versioned-model rule, and every Worker's binding is checked before the first Provider is
  probed; a reported model ID outside it reads `[not printable]`
  ([ADR-0147](docs/adr/0147-stop-the-claude-model-fallback-and-name-the-model-that-answered.md)).
  The Provider admission probe, which `af provider doctor` also sends, now says what it is for (a Provider admission
  check by the af review tool that needs no file and no tool) before asking for exactly `OK`,
  because Claude Fable 5.1's safety classifier refused the bare instruction every time from an
  empty directory.

- `af provider models [ID …] [--json] [--resolve SPELLING[/EFFORT]]` lists each Provider's model
  catalog token-free, from the harness's own protocol (Claude Code's `list_models` on a
  stream-json session closed before any turn, Codex's `model/list` on a bare app-server), with
  its source, harness version and freshness, one row per exact ID with its display name, aliases
  and effort levels, and what the list proves; only the charged `af provider doctor` proves that
  an account can run a model. `--resolve 'Fable 5.1/xhigh'` names the exact ID, checks the effort
  level and prints the `reviewer.toml` `[runner]`, the `worker.toml` `[runner]` and the bindings
  `[providers]` blocks, each a TOML document that parses on its own; `--json` prints
  `af/provider-models@1`; exit 4 when a CLI cannot serve af, 7 when a catalog is unavailable. One validation module now checks every model and effort level before
  anything is charged: an alias is refused with the exact ID it resolves to, an effort level the
  Provider or model does not list with the levels it lists, a misspelling of a known ID with the
  nearest IDs, and a custom ID that passes the spelling rule is accepted as `unverified against
  the catalog`. `af task plan` and `af review plan` print a `model` line per Model Worker and
  refuse before any Provider process starts; both adapters refuse such packages too, so a
  reviewer package naming `--model opus` must name `claude-opus-5-5`. Every refusal names the
  Provider, the rule, the nearest accepted spellings and `af provider models <id>`. The TUI's
  Providers pane shows each Provider's own models and the Workers pane each runner's verdict
  once per Provider of its kind, named by its ID
  ([ADR-0148](docs/adr/0148-discover-provider-models-and-validate-every-binding-before-a-charge.md)).

## [0.12.0-rc.3] - 2026-10-14

### Authority compatibility

Committed .af/ policy keeps working as is relative to 0.12.0-rc.2; no authority migration is required. `af provider status --json` now emits `af/provider-status@2` (new state `incompatible`); readers of the @1 document need updating.

### Changes

- release: v0.12.0-rc.2 (#238)
- test: replace redundant stale-view repair process case with contract coverage (#242)
- Remove dormant session protocol and obsolete compatibility code (#256)
- Detect incompatible Claude Code and Codex CLIs before dispatching Workers (#260)

- Tests: the real-process `task_repair` case for a fix verifier that names a stale Finding view is
  replaced by the `stale_snapshot_view` contract case, which applies the same mutation to the
  shared `validate_context` predicate behind `validate_fix_output` and `repair_acceptance`; exit 4,
  8 Attempts and an unchanged replay stay asserted by the stale-subject case. The claim-less receipt
  case stays real-process, because the Worker output schema rejects it before `validate_context`.

- A release now reads only the state, configuration and flags it writes itself: no reader,
  migration or fallback is kept for anything an earlier release wrote, and a changed persisted
  shape takes a new version whose predecessor is refused
  ([ADR-0145](docs/adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).
  Finish or discard in-flight Campaigns and Tasks before upgrading. The Warm Set is now
  `review.kernel/WarmSet@2`, selected by `WarmSetSelected@2`, with no session layer; a Campaign
  that recorded `WarmSetSelected@1` is refused by its replay error. The never-hosted Claude
  session layer and Cold Closeout are removed with their events, so a pipeline that sets
  `warm.session`, `warm.session_max_age_secs` or `convergence.cold_closeout` no longer loads:
  delete the key. A Campaign the pre-Task executor wrote is refused by the same replay error as
  any other record this release does not write, instead of a dedicated message. The `.af/`
  layout no longer declares `task-compat/`, so a tracked copy is reported as undeclared.

- af now checks, token-free, that the installed `claude` or `codex` accepts its invocation before
  `af provider status`, `af provider setup` or Task Provider admission asks about a login
  ([ADR-0146](docs/adr/0146-refuse-a-provider-cli-that-does-not-accept-afs-invocation.md)). It
  reads `claude --help` or `codex exec --help` and refuses a CLI that does not declare every
  option af passes, such as Claude Code before 2.1.248, which has no `--restricted`. No isolation
  flag is ever dropped to make a CLI run. The report names the Provider, the path, the version,
  every missing option and the upgrade command. `af provider status` shows the new state
  `incompatible` and exits 4, and `--json` now emits `af/provider-status@2`. Task admission
  refuses before any Worker runs or any token is charged.
- The Codex app-server is started without `--stdio`, so codex-cli 0.121.0 works again. For Codex
  the check also starts the bare `codex app-server` and asks it to `initialize`; only an answer
  whose `result` is a JSON object proves it. An app-server that exits first, rejects the request,
  answers `{"id":1}`, or writes a line that is neither a response (an `id` and exactly one of
  `result` or `error`) nor a notification or request is `incompatible` at once on plain status,
  `--usage`, setup and admission alike, reported with its exit status (or `was stopped by af`) and
  the line it answered with. The identity and usage probes judge every line by the same grammar
  and accept only an answer read after af wrote the request, so a response written early never
  binds an account or shows limits.
- A Worker that exits without a parsable result (for Claude, anything but an object with a
  boolean `is_error` and its `result`; for Codex, anything but the `-o` final message after a
  clean exit) now reports its exit status and a bounded stderr excerpt whatever its failure is
  classified as, and one whose CLI refused af's arguments is classified `unsupported_invocation`
  whatever its exit status, even beside an authentication hint or after unrelated Codex events.
- One shared filter replaces any Provider-authored line that looks like credential material
  (`API_KEY=…`, `Bearer …`, `Authorization:`, `sk-…`, 32 or more hex characters, 32 or more
  base64 characters mixing cases and digits, a JSON Web Token of three whole base64url segments)
  with `[line withheld: looks like credential material]`, the Codex rate-limit names shown by
  `status --usage` included.

## [0.12.0-rc.2] - 2026-10-09

### Authority compatibility

Committed .af/ policy keeps working as is relative to 0.12.0-rc.1; no authority migration is required.

### Changes

- chore: pin af 0.12.0-rc.1 in .af/af.lock (#227)
- Keep a long path's reason on the TUI status line (fixes a test that failed every Task gate) (#233)
- Report slow tests and suite totals from every nextest run (#218)
- Run only the self-optimizer's timed phase alone (#222)
- Wait once, not per case, in looping lease and deadline tests (#204) (#234)
- Judge the same Task writer at its later recorded time everywhere (#231) (#235)
- Fix main: check the real nextest config against its own text, not a pinned exclusive list (#236)
- Give non-timeout tests a load-safe wall so a loaded machine cannot fail them (#206 part 1) (#237)

- `make test` now prints where test time goes after every nextest run, passing or failing:
  nextest's wall time and test count, summed test-seconds, achieved parallelism, the time spent
  in the exclusive block that `.config/nextest.toml` runs alone
  ([ADR-0124](docs/adr/0124-run-tests-in-parallel-processes-and-link-them-once.md)), failures,
  a duration histogram and the slowest tests. CI also writes it to the step summary. A Task
  gate (`scripts/verify.sh`) records every `make check` step's time beside its nextest reports
  and ends its output with the same summary. The JUnit an earlier run left is removed before
  nextest starts, and a JUnit whose testcases do not add up to its declared counts gets a
  warning instead of totals; the report never changes the test step's exit status. It runs
  under Python 3.9 and reads `.config/nextest.toml` without `tomllib`. `scripts/test-time-report.py compare BASE_JUNIT... -- HEAD_JUNIT...` compares runs
  by their per-side medians and lists per-test changes of at least 1 s and added or removed
  tests, as a Markdown table for a pull request description. `--base-nextest-config PATH` and
  `--head-nextest-config PATH` give each side its own nextest config (each defaults to
  `--nextest-config`, which still sets both), so each side's exclusive block counts exactly the
  tests its own config runs alone, and the output names both configs when they differ.

- The self-optimizer light-strategy test no longer holds every test thread for its whole run
  (#203, [ADR-0124](docs/adr/0124-run-tests-in-parallel-processes-and-link-them-once.md)). It
  is split into two tests in `crates/af/tests/it/self_optimizer.rs`. Each builds its own
  repository and Store with the same `af` commands through shared helpers. Of the two, only
  `light_cache_candidate_measures_real_latency_and_grounds_later_adoption_evidence` stays in
  the exclusive override of `.config/nextest.toml`. It first runs, delivers and adopts the
  light candidate without asserting on it, then runs every phase from line 719 to line 1277
  of the original test in order: the cache latency comparison (8 trials, 3 s baseline) with its
  replay, delivery, adoption and ordinary cache-consuming Task (719-1090), the unknown-toolchain
  comparison and refused delivery (1092-1228), and the light adoption observed with the cache
  Task as evidence (1230-1277).
  `light_strategy_generates_one_candidate_without_exposing_source_to_author_workers` runs in
  parallel and keeps every other phase in the original order on one repository: plan review
  without source (186-259), the approved five-Attempt run and its replay (260-376), delivery and
  adoption observations (378-530), source invalidation (532-571), the uneconomic recommendation
  (573-669), the unexecuted binding (671-717) and, after the same cache configuration and
  toolchain removal, the unsupported recipe (1279-1322). Every assertion of the original is in
  exactly one of the two, unchanged. The original repeated its `git` exit-status checks inline;
  they now live in one `commit` helper, so both tests check the setup commits they both make.
  No sleep, trial count, budget, retry or slow-timeout changed. The previous seven-test split
  had dropped the original's restore of the baseline worker without its sleep before the
  unknown-toolchain comparison. That comparison slept 3 s on every baseline trial and took
  20.2 s instead of 8.1 s. `remove_cache_toolchain` now restores the baseline as main does.
- Measured with temporary per-phase timers, since removed. Each test ran alone under
  `cargo nextest run --profile ci` on a 14-core macOS host after one warm-up launch of `af`.
  Figures are medians of three runs unless marked otherwise.
  - Main's single test took 60.3 s. Setup up to the adopted light candidate took 8.8 s:
    fixture 0.2, plan review 4.0, approval and run 3.5, delivery and adoption 1.1. Phases that
    read no comparison state took 13.3 s: replay 0.1, adoption observations 0.7,
    invalidation 2.2, uneconomic 5.7, binding 2.2, unsupported recipe 2.5.
  - The two real comparisons took 28.4 s: the cache `task run`, with its trials and 3 s
    baselines, 20.2 s, and the unknown-toolchain `task run` 8.1 s. Their configuration, plan
    and signed approval took 5.5 s.
  - The non-timed work that only reads the comparisons' state took 3.7 s: cache assertions 0.1,
    replay 0.1, delivery and adoption 1.5, the ordinary Task 1.3, unknown-toolchain assertions
    and refused delivery 0.1, and the light observation with cache evidence 0.6.
  - The previous split's exclusive test took 60.2 s: light setup 9.6, plans 5.5, cache
    comparison 20.5, unknown-toolchain comparison with the sleeps 20.2, reads 4.0. Its seven
    moved tests, each run once alone, took 37.0 s together: 10.7, 7.8, 6.0, 5.7, 2.4, 2.2 and
    2.2 s.
  - In the chosen pair, the exclusive test takes 45.5 s: light setup 9.6, cache plan 2.4,
    cache comparison 19.7, its reads 2.8, unknown-toolchain plan 2.5, its comparison 7.6, its
    reads 0.7. The parallel test takes 19.8 s.
- The decision uses the expected wall time W(T) = E*(T-1)/T + S/T, where E is the exclusive
  seconds and S the sum of all the light-strategy tests:

  | Variant | E (s) | S (s) | W(4) (s) | W(7) (s) |
  | --- | --- | --- | --- | --- |
  | Main's single test | 60.3 | 60.3 | 60.3 | 60.3 |
  | Previous seven-test split | 60.2 | 97.2 | 69.4 | 65.5 |
  | Chosen pair | 45.5 | 65.3 | 50.5 | 48.3 |

  The pair beats the single test by 9.85 s at T = 4, clearing the 5 s bar, so the split is kept
  in this form.
- `setup_repairs_auth_directory_and_lock_modes_under_a_restrictive_umask` stays in the
  exclusive override. Its work takes 0.37 s, but running first and alone it pays the first
  launch of a freshly linked `af`, which macOS assesses before it runs: 11.2 s measured
  straight after relinking, and every Task gate starts from a fresh build. Outside the override
  that cost lands on whichever parallel test launches `af` first, under the 15 s provider status
  probe deadline. In a before/after bench (one warm-up, then three alternating full-suite runs
  per side, 7 threads, 14-core macOS host), one of three runs without it failed that test and
  `status_keeps_a_default_context_whose_status_probe_failed` with "provider status probe timed
  out after 15 seconds"; no run with it failed. Its gain outside was about 1.3 s. The reason
  is written beside the exclusive override.

- Tests that loop over cases bound by a real lease or deadline now prepare every case, wait once
  on the real clock until every case's lease or deadline has passed, and then run each case,
  which first asserts that its own lease or deadline has passed (issue #204). No production
  timing constant, real clock or assertion changed; tests whose subject is the wait are untouched.
  `task_runtime::usage_recovery::worker_and_provider_cas_failure_recover_full_reported_usage_without_another_call`
  and `incomplete_native_observation_survives_cas_outage_and_both_admission_and_worker_recovery`
  went from two real lease waits each to one (about 30.5 s to 15.5 s), and each case now also
  asserts that the Task's absolute deadline has not passed when it recovers.
  `task_campaign_review::host::an_expired_review_records_incomplete_without_inventing_unstarted_gate_facts`
  went from three real deadline waits to one (5.2 s), and each mode now asserts the exact
  dispatch refusal `Task plan deadline expired` instead of any error. In review-store,
  `recording_resume_rejects_revoked_decision_or_replaced_latest_report` first gained exact refusal
  causes for every case: the expired decision and the revoked decision each refuse with the
  approval conflict, pinned to expiry or revocation alone by the decision's state; the replaced
  report's resume refuses for carrying no publication failure; and its stale-report append refuses
  at the expired-pause gate, whose other conditions the preceding refusal proves. A temporary
  variant without the wait then failed: the expired-decision and replaced-report resumes refused
  with `Recording recovery requires the exact expired admitted publication pause` instead. The
  revoked case refuses the same way with or without the wait, because revocation is checked
  before the deadline, so its own past-deadline assertion is what catches a missing wait. Only
  then were its three deadline waits coalesced into one (9.2 s to 3.2 s). No target was left
  unchanged.

- `af` browser: a message on the status line that names a long path, such as the `:cd` refusal
  `<path>: not a directory`, keeps its reason visible. The path is shortened from the left with
  `...`, whole leading components first and then, when its final component alone is too wide,
  inside that component, instead of the line being cut before the reason. A quoted path with
  spaces is shortened as one path. A message that fits is unchanged (#229).

- A live Task writer is no longer fenced when its own heartbeat records a renewal between one of
  its operations reading the clock and that operation observing its lease or writing (#231). Each
  place that compares the current writer's clock with the last recorded time now judges the same
  writer and epoch at the later of the two: the heartbeat's lease observation
  (`EventStore::task_lease_state`), the projection's clock check when it applies a transition
  (`TaskProjection::apply`, which also records that time and ends a released lease there), and the
  review integration append (`append_integration_atomic`), which now stamps its transition before
  it is persisted, as every other Task append already did. The lease check of each Task write
  (`TaskProjection::check_lease`) and the append stamp already used this time. Another writer or
  epoch, a premature takeover, Task collection's own clock check, an expired lease and a finished
  Task are refused as before
  ([ADR-0128](docs/adr/0128-renew-a-live-task-writer-lease-through-its-own-connection.md)).

- Tests: a test whose subject is not a deadline now gives its fixture processes a named,
  documented two-minute wall, `LOAD_SAFE_WALL`, instead of a short fixed one, so a loaded gate
  (seven nextest threads, load 7-28) no longer fails it on a timeout its subject never asked
  about. Only a hung fixture waits for the larger wall; a passing test takes as long as before.
  Tests whose subject is a timeout keep their exact walls, every elapsed-time assertion keeps its
  bound, and no production constant or nextest setting changes. A debug-only, raise-only test
  setting lifts the provider probes' timeouts, and one fixture grows its reserve with its walls;
  no reserve shrinks (#206,
  [ADR-0114](docs/adr/0114-budget-cli-task-fixtures-for-loaded-machines.md), amended).
- Changed, `review-runner-codex` (5 s provider wall): `task_auth_failure::` all seven tests
  (`authentication_failure_survives_missing_usage_without_retaining_challenges`,
  `non_auth_native_failures_keep_their_evidence_and_classification`,
  `plaintext_auth_failure_is_private_even_without_a_json_event`,
  `network_failure_does_not_publish_device_challenges_from_either_stream`,
  `successful_output_about_device_authentication_is_unchanged`,
  `model_text_about_revoked_credentials_cannot_replace_a_network_failure`,
  `a_failure_event_without_usage_names_its_classified_cause`);
  `task_capture::held_output_retains_reported_overrun_without_admitting_the_message`;
  `task_final_message::native_final_message_objects_are_bounded_and_keep_exact_usage` (also its
  15 s child-harness wall); `task_worker::review_role_keeps_the_legacy_workspace_write_sandbox`,
  `execute_checks_runs_workspace_write_rooted_at_the_sandbox`,
  `typed_document_and_malformed_or_failed_results_retain_the_same_provider_usage`,
  `multiple_native_turns_retain_exact_components_and_uncached_charge`,
  `malformed_native_usage_refuses_message_and_survives_raw_capture_outage`;
  `task_cancellation::native_cancellation_retains_full_turn_usage_and_reaps_owned_process`
  (shared fixture: 10 s wall, 5 s readiness window).
- Changed, `review-runner-claude` (5 s provider wall): `task_auth_failure::`
  `refresh_contention_survives_missing_usage_without_retaining_challenges`,
  `non_auth_native_failures_keep_their_evidence_and_classification`,
  `successful_output_about_authentication_is_not_classified_as_failure`,
  `network_failure_does_not_publish_device_challenges_from_either_stream`,
  `model_text_about_revoked_credentials_cannot_replace_a_network_failure`;
  `task_capture::held_output_retains_reported_overrun_without_admitting_the_message`;
  `task_worker::review_role_keeps_the_legacy_read_only_tool_grant`,
  `execute_checks_grants_bash_and_ends_shell_children_with_the_attempt`,
  `typed_document_and_malformed_or_failed_results_retain_the_same_provider_usage`,
  `malformed_native_usage_refuses_message_and_survives_raw_capture_outage`;
  `task_model_usage::title_suppression_reaches_the_child_without_replacing_personal_auth_grants`,
  `top_level_and_model_usage_charges_persist_and_reopen_exact`,
  `a_breakdown_above_the_top_level_summary_keeps_the_reply_and_is_charged_in_full`;
  `task_structured::` every test through its `invoke` helper
  (`typed_native_command_preserves_input_schema_and_role_permissions`,
  `execute_checks_command_derives_bash_from_the_access_alone`,
  `typed_reply_never_falls_back_and_failed_outputs_keep_accounting`,
  `malformed_or_oversized_typed_requests_do_not_spawn_and_legacy_stays_textual`,
  `nested_payload_references_keep_their_own_roots_and_literal_values`);
  `task_cancellation::native_cancellation_retains_full_usage_and_reaps_owned_process`.
- Changed, `review-runner` `model_supervision::`
  `controlled_capture_preserves_redacted_evidence_after_cancellation` (10 s wall, 5 s readiness),
  `a_model_descendant_holding_output_is_charged_not_empty_evidence`,
  `a_model_descendant_holding_only_stderr_preserves_the_complete_answer`,
  `settled_held_output_retains_redacted_bytes_without_admitting_a_message` (1 s),
  `a_worker_may_ignore_its_stdin`, `large_stdin_and_stderr_are_drained_concurrently`,
  `a_briefly_lingering_descendant_cannot_truncate_a_large_answer` (5 s),
  `a_granted_secret_is_redacted_from_everything_kept`,
  `an_ungranted_variable_never_reaches_the_child` (10 s).
- Changed, `review-process`: unit tests
  `a_descendant_holding_only_stderr_does_not_destroy_complete_stdout` (1 s),
  `capture::tests::stream_input_failure_keeps_both_outputs_and_its_error_type` (5 s),
  `capture::tests::held_stdout_capture_and_compatibility_wrapper_keep_the_same_failure` (1 s),
  `drain::tests::a_read_failure_retains_the_prefix` (5 s grace) and
  `drain::tests::cancelled_drain_waits_for_buffered_prefix_and_keeps_cancellation_primary`
  (1 s readiness and grace), which assert a read failure or a cancellation, never a timeout;
  `cancellation::cancellation_retains_prefixes_and_stops_running_stdin_and_each_held_drain`
  (5 s readiness, 10 s wall).
- Changed, `review-sandbox` `container::tests`: the runtime-fixture writer takes its wall, so
  `an_installed_but_broken_runtime_is_unusable_not_usable` and
  `cancellation_still_confirms_container_removal_without_cancelling_cleanup`, which assert no
  timeout, write their fake runtime under the load-safe wall instead of 5 s; the deadline tests
  keep the writer's 5 s.
- Changed, `review-source-task` (5 s deadline): `transport::`
  `native_transport_owns_protocol_flags_and_keeps_credentials_off_argv`; `sources::`
  `jira_refuses_incomplete_changed_or_unsupported_sources_without_leaking_response_text`,
  `adf_list_continuations_preserve_nesting_and_ordered_marker_width`.
- Changed, `af` unit tests: `providers::task::currentness_tests::`
  `sandbox_environment_passes_the_identity_recheck_before_it_can_reach_the_native_client`
  (5 s Attempt wall); `providers::auth_handoff::adapter::tests::` the 5 s synthetic-login waits of
  `synthetic_claude_process_accepts_exactly_one_code_and_reaps`,
  `synthetic_claude_callback_completes_without_returned_code`,
  `native_nonzero_exit_never_presents_a_stale_challenge`,
  `synthetic_codex_device_cli_is_reaped_on_successful_exit`,
  `dropping_device_login_reaps_the_owned_native_cli_without_json_requests`,
  `codex_native_failure_cannot_complete_or_publish_a_stale_challenge`,
  `private_guard_codes_are_decoded_only_for_owned_guard_children`,
  `native_stderr_never_enters_the_private_challenge_buffer`,
  `native_output_limit_is_cumulative_and_never_a_diagnostic`.
- Changed, `review-pipeline`:
  `review_domain::integration::tests::legacy_check_order_and_one_writable_clone_survive_the_shared_sequence`
  (10 s Check wall and 10 s deadline option), which runs the same two successful Checks three
  times and asserts only their order and equal receipts, never a timeout or a deadline refusal;
  `task_runtime::control::captured_command_cancellation_retains_both_streams_and_never_retries`
  (3 s readiness),
  `task_runtime::domain_observes_started_attempt_and_persists_through_the_runtime_store` (2 s
  Store wait), `review_domain::integration::tests::`
  `controlled_check_sequence_retains_interrupted_raw_result_and_stops_before_the_next_check`
  (20 s Check wall, 3 s readiness).
- Unchanged, subject is a timeout or deadline: each test here asserts a timeout, a deadline
  refusal or an elapsed-time bound, and keeps every wall it sets, including the
  fixture-preparation walls inside it. Codex and Claude `task_worker::`
  `timeout_and_cas_failure_preserve_reported_overrun_without_admitting_the_message`, which now
  also asserts that its message error names the 500 ms wall's `TimedOut`; Claude
  `task_model_usage::synthetic_native_multi_model_usage_survives_refusal_timeout_and_cas_outage`,
  whose message error is the model-identity refusal, so its timeout scenario now asserts it
  returns before the fixture's 10 s sleep could end it; `model_supervision::`
  `a_hung_reviewer_is_killed_at_the_deadline`, `a_killed_reviewer_keeps_what_it_wrote_so_far` and
  `a_model_parent_exit_cannot_leave_the_stdin_writer_unbounded` (`TimedOut` and elapsed);
  `review-source-task` `native_source_cancels_inflight_process_and_bounds_output` and
  `replacement_sources_have_equivalent_requirements_and_exact_field_provenance` (`TimedOut`);
  `review-check` `deadline::` `a_hung_check_is_killed_and_recorded_not_run`,
  `a_backgrounded_grandchild_does_not_hang_the_deadline` and
  `a_passing_check_with_a_backgrounded_child_returns` (elapsed); `review-source-git`
  `git_deadline` (deadline error and elapsed); `review-process` `concurrent_pipes` (`TimedOut`
  and elapsed) and `drain::tests::held_pipe_cleanup_keeps_a_chunk_delivered_after_group_termination`
  (its 1 s grace ends in a held stdout, and the shared cleanup deadline); `review-sandbox`
  `container::tests::` `runtime_detection_stops_at_the_callers_deadline`,
  `a_wedged_runtime_is_bounded_and_unusable` and `a_wedged_container_execution_is_bounded`
  (deadline error and elapsed), and `a_failed_reap_is_distinct_from_a_safely_stopped_timeout`
  and the live-runtime `a_timed_out_container_is_removed_before_execution_returns`, which now
  also assert that their container command did not finish; `af`
  `providers::installation::a_working_cli_and_a_slow_or_cancelled_check_are_not_installation_failures`
  (elapsed) and `identity_rechecks_use_remaining_attempt_deadline_and_prespawn_cancellation`
  (timed out and elapsed); `review-pipeline`
  `an_absolute_attempt_deadline_refuses_setup_and_bounds_the_running_check` (deadline refusal
  and elapsed) and `unconfirmed_container_cleanup_preserves_the_writable_sandbox_and_stops_checks`,
  whose container is ended by its 100 ms Check wall and which now also asserts that its Check's
  reason names that timeout.
- Unchanged, no process starts under the wall: `model_supervision::`
  `a_missing_provider_is_unavailable_not_silent`,
  `an_untrusted_option_is_refused_before_the_model_starts`;
  `cancellation_before_spawn_cannot_execute_the_command`;
  `capture::tests::spawn_failure_does_not_invent_output`;
  `a_removed_executable_with_no_installed_client_refuses_by_name_before_any_probe`;
  the 1 s execution wall of `an_installed_but_broken_runtime_is_unusable_not_usable`, refused
  before launch; `drain::tests::cleanup_retains_a_prefix_even_when_its_reader_failed`, whose
  in-process reader fails at once under the production 5 s grace of `collect_after_kill`, the
  function it tests, so no test parameter sets that bound; and `task_model_transport`, whose
  1234 ms is a sentinel forwarded to a fake adapter.
- Unchanged, the bound is the asserted promptness: the elapsed-time assertions of the changed
  capture, final-message, held-pipe and cancellation tests; the waits in which a killed process
  must disappear (`native_cancellation`, claude
  `execute_checks_grants_bash_and_ends_shell_children_with_the_attempt`, `review-process` and
  `review-source-task` cancellation); `review-sandbox`
  `cancellation_still_confirms_container_removal_without_cancelling_cleanup`, whose readiness
  window is its 3 s elapsed assertion; `review-graph` `independent_reviewers_run_concurrently`;
  the lease and heartbeat tests of `task_runtime::control`, `review-store` and `af`
  `campaign_loop::heartbeat`, `common_review_summaries::recovery` and `task_interrupt`.
- Unchanged, the wall is already long: `af` `tui` (30-300 s), `provider_auth_handoff` (30 s),
  `task_report_command` (60 s), `task_command_process_group` (30 s) and the container probes'
  60 s execution wall.
- Changed, `af` integration tests (15 s Codex, 10 s Claude usage and 30 s Claude status
  provider probes): a debug build's provider probes now honor `AF_TEST_PROVIDER_PROBE_TIMEOUT_MS`, which can only raise
  their timeouts (a value at or below production, outside 1 ms to ten minutes, or not a number
  changes nothing), never reaches a Worker or provider CLI, and is never read by a release
  binary. The shared `crate::common::af()` helper, and the PTY spawns of `provider_registry` and
  `tui` that clear the environment, set it to the two-minute load-safe value, so
  `provider_registry::status_keeps_a_default_context_whose_status_probe_failed` and every other
  `af` integration test that meets a probe without being about its timeout no longer race it.
  The probe-deadline unit tests (`providers::installation`) keep the production value.
- Changed, `af` `provider_auth_handoff`: `Fixture::command_kind`, `Fixture::begin_kind` (through
  its Python host) and the inline `af` spawn of
  `duplicate_stdout_and_named_fifos_are_not_private_capabilities` clear the environment, so their
  `af` never got the setting and a completed handoff's fake Codex or Claude status probe ran
  under the production 15 s or 30 s. Each now sets it again after `env_clear`; no test there is
  about a probe's timeout. The fake CLIs record the setting if it reaches them, and
  `Fixture::no_secrets` and `claude_private_code_and_callback_paths_complete_without_task_dispatch`
  assert it reached neither a status probe nor a login.
- Checked, no other change needed. Every other `env_clear` or `env_remove` before `af` or a
  provider probe in a test: `tui`'s `af()` helper and `Browser` PTY spawn, and
  `provider_registry`'s `in_terminal` (through `setup_in_terminal` and the umask setup test),
  already set it after clearing; `common::af()` callers that only remove other names
  (`task_warm_checks`, `task_experiment`, `task_remote_checks`, `campaign_loop`, `cli_surface`,
  `storage_budget`, and `provider_registry`'s status runs) keep it; the umask `provider add`
  runs of `provider_registry`, `onboarding_quickstart` (`af onboard`, `af review plan`) and
  `common::Layout::command` (self-management) reach no provider probe and clear nothing; the
  `af` unit-test `env_clear`s (`providers::auth_handoff::adapter` and `guard` synthetic logins,
  `tui::panes::pipelines` git) start neither `af` nor a probe; and no other crate's tests start
  `af` or a provider probe.
- Changed, `af` `task_file` native-model tests (the four `native_client_*`,
  `native_model_cli_*`, `native_codex_multiturn_*` and `native_task_account_change_*`): the
  reviewers' 5 s Attempt walls are now the two-minute load-safe wall, the verification reserve
  grows from 60 s to the 290 s it now encodes (45 s Provider admission, both reviewers and the
  5 s check), and the Task wall keeps ten minutes above that reserve; token and Attempt limits
  are unchanged. Their admission and rechecks run under the raised probe timeout above, and
  they assert the setting reached neither a probe nor a model call. The pinning test, renamed
  `native_model_fixture_widens_only_its_walls_and_the_reserve_that_covers_them`, asserts the
  reserve equals that sum instead of the old literal.
- Not a wall: `optimization_configuration`'s failures are #149 (reflink cache materialization);
  its tests set no wall, and nothing here changes them.

- `scripts/test-test-time-report.py` no longer pins the live exclusive list of
  `.config/nextest.toml`: it reads the expected filter and its `test(/.../)` clauses from that
  file's raw text, independently of the TOML subset parser it checks, and keeps the exact-list
  assertions on the committed fixture `fixtures/test-time-report/nextest.toml`. Changing which
  tests run alone no longer breaks `make preflight-check`. It still requires every live
  exclusive pattern to be a plain test-name pattern anchored with a trailing `$`, and fails
  naming the offending pattern otherwise, since an unanchored one would silently run more tests
  alone: a temporary local edit removing the `$` from one live clause made the script fail,
  while dropping or renaming anchored clauses left it passing.

## [0.12.0-rc.1] - 2026-10-08

### Authority compatibility

Committed .af/ policy keeps working as is. Machine files: the remote-checks mapping no longer takes `checks` (a pipeline's `remote_checks` chooses), and `[storage]` is new machine-only configuration (budget-only by default).

### Changes

- release: v0.11.0 (#183)
- Remote Checks: the pipeline chooses where a check runs (#186)
- Leave out a logged-out CLI default context once its kind is registered (#182)
- Bump jsonschema, num-bigint, minisign-verify and minisign; test damaged signatures (#188)
- build(deps): bump minisign from 0.9.1 to 0.10.0 (#177)
- build(deps): bump minisign-verify from 0.2.5 to 0.3.0 (#176)
- build(deps): bump num-bigint from 0.4.8 to 0.5.1 (#175)
- Bump jsonschema from 0.57.0 to 0.58.4 (#174)
- Add af task report, and require its block in every pull request (#192)
- Pin af 0.11.0 in .af/af.lock (#184)
- Charge zero and record unknown usage when a Provider reports none (#165) (#195)
- Hold af's disk use to a machine budget (#213)

- Remote Checks are chosen by the pipeline, not by the machine. A Task pipeline's check node lists
  `checks` (run on this machine) and `remote_checks` (run through the check's declared remote
  executor); a project that wants both gates keeps two pipeline variants, and there is no
  per-machine switch. The machine-local mapping (`$XDG_CONFIG_HOME/af/remote-checks.toml` or
  `AF_TASK_REMOTE_CHECK_POLICY_FILE`) now names only where this machine may push gate branches
  for a repository: **its `checks` key is gone, and a file that still carries it is refused**. A
  pipeline with remote checks cannot be planned without a target, and its plan carries the effect
  `publish-gate` and the destination `github:<owner/name>`, which `af task plan` prints on
  `EFFECTS` and `SEND`, so confirming the plan is the consent; the target is read again before
  the check Attempt pushes. A pipeline without `remote_checks` plans and runs exactly as before
  ([ADR-0140](docs/adr/0140-run-a-declared-check-through-a-gate-pull-request.md), amended).

- `af provider status` and the browser's Providers pane no longer list the Claude or Codex CLI's
  default context (`claude-ambient`, `codex-ambient`) when it has no login and a Provider of its
  kind is registered; it still shows on a machine with no Provider of that kind yet, and when it
  holds a login ([ADR-0141](docs/adr/0141-list-a-logged-out-default-context-only-until-its-kind-is-registered.md)).

- `af task report TASK_ID...` summarizes how recorded Tasks ran and what they cost, as one
  Markdown block between `<!-- af-task-report:v1 -->` and `<!-- /af-task-report -->`, ready for
  a pull request description, or with `--json` one `af/task-report@1` document
  (`schemas/task-report-v1.json`). The block leads with one line per pipeline the Tasks ran, its
  steps in dependency order with each step's Worker by Provider kind and model and the gate's
  check names, parallel steps of one role grouped
  (`review (bugs, correctness: codex gpt-6-sol/high)`), and `**unknown pipeline**: not
  retained` once for Tasks whose plan was collected or never made; then one row per Task, each
  a round, with its outcome, its review findings by severity (`6 major, 1 minor`, `none`,
  `unknown` when a round has no complete finding set, `gate failed`, or `—` without a review,
  and how many reviewers failed), tokens with thousands separators
  (`205,295`) and active time, and a `Total:` row of Attempts (failed), tokens and active time;
  then per round its runs, wall time, failed Attempts by reason class and charged tokens, and per
  node the Worker, Attempts, tokens, elapsed time and check results. Findings are counted once
  each as the round's reduce step recorded them, and review rounds come from the Task's log, so
  a round recorded before `af task refresh` still counts. Active time sums the Task's runs, the
  leases in which an Attempt started, so waiting between `af task run`s, a refresh, the recovery
  of a pending Attempt and a resume that only publishes a settled result are not counted; a
  figure the Store does not record is shown as unknown. Each Attempt is charged to the Worker of
  the plan it ran under, so a node that `af task refresh` moved onto another Worker has one row
  per Worker. It only reads the Store and never prints a Provider label, path, credential,
  prompt or Worker output: a Worker's model is copied only when it is a model identity (at most
  96 characters of letters, digits and `._:+-` with at most one `/`, and no `@`, URL, drive
  letter or `..`) and is `unknown` otherwise, and every cell encodes `\` and `|` so a value stays
  in its cell. This repository now requires the block in every pull request description: changes
  are made through af Tasks, and the `PR report` workflow, which runs on `pull_request_target`
  from the base branch's checker so a pull request can change neither the check nor its
  workflow, refuses a description without exactly one well-formed block — a pipeline line, the
  six round-table columns Round, Task, Outcome, Findings, Tokens and Active in order, at least
  one round row and a last `Total:` row; placeholder, short and long rows (with or without their
  outer `|`), a Round, Task or Outcome cell that is empty or holds only an HTML comment, a
  separator row of the wrong width, and a block inside a code fence or indented as code, even
  right after a heading, a thematic break, a fence, an HTML block or a list item, fail —
  apart from Dependabot and `release/` pull requests
  ([ADR-0142](docs/adr/0142-carry-the-af-task-report-in-every-pull-request.md)).

- An Attempt whose Provider reported no usage is charged 0 tokens and its usage is recorded as
  unknown, with its cause, instead of being charged its whole reservation: a Codex Attempt that
  failed with `Selected model is at capacity` no longer costs 400,000 tokens, nor does one
  recovered after its writer's lease expired while the machine slept. The settlement's
  `af/TaskExecutionRecord@5` carries `unknown_usage: { cause }` (`capacity`, `rate_limit`,
  `authentication`, `model_unavailable`, `network`, `lease_expired`, `interrupted` or
  `unreported`); such an Attempt adds nothing to the Task's, node's or verification reserve's
  charged tokens and still counts against the Attempt limits, and Attempts with reported usage
  are charged exactly as before. A Codex Attempt that ends with an `error` or `turn.failed`
  event names its classified cause, such as `Provider model at capacity (capacity)`, instead of
  the bare exit status. `af task show`, `af task list`, the browser's Tasks and Workers panes and
  `af task report` show such usage as unknown, never as 0 spend: a Tokens cell reads
  `1,200 (+1 unknown)`, the round's details name the cause, and the JSON documents carry the
  count. A usage observation, even one that reports 0 tokens, makes an Attempt's usage known,
  and `af task gc --apply` keeps the count in the collected Task's `af/TaskCollected@1`
  tombstone, so a collected Task still lists and reports `(+N unknown)`. Stores written by earlier releases read as before, with their recorded charges
  ([ADR-0143](docs/adr/0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md)).

- af now keeps a bounded, visible amount of disk
  ([ADR-0144](docs/adr/0144-hold-afs-disk-use-to-a-machine-budget.md)). A machine-only `[storage]`
  table (20 GiB `max_bytes` and a 10 GiB `min_free_bytes` floor by default; `AF_STORAGE__<KEY>`
  overrides it, and a repository's `.af/af.toml` cannot) bounds warm build keys, warm Workspaces,
  review campaigns, Task Stores and installed versions, evicting the least recently used entry first
  and never one in use or used within the hour, after every `af task run` and `af review run`.
  Age-based collection (`keep_days`, `keep_tasks`, `keep_campaigns`) runs on `af storage prune
  --apply`, or after every run with `auto_gc = true` (off by default), and reaches Stores this
  release cannot read and Stores made with `--state`, which `$XDG_STATE_HOME/af/stores.toml` now
  records; every removal opens its target from its configured root through descriptors, never
  following a link and never removing a directory that changed since it was measured, and an
  installed version is checked again under the lock `af self` takes to change the default or a pin.
  The sweep that ends `af task run` is recorded on that Task as a `storage_sweep` observation, shown
  by `af task show`. Below the floor on any volume af works on, or when one cannot be measured, a
  check reports `insufficient_disk`, a measurement fails with `insufficient_disk` without running
  its command, and a Worker Attempt is refused before any token is spent. `af storage` shows what af
  holds and `af storage prune [--apply]` reclaims it. A warm check with a native toolchain mapping
  now reuses one key instead of making a new one every Attempt (the key domain moves to
  `af.task-build-cache.toolchain/2`, so old keys are evicted once). Every check, review gate checks
  included, gets its own empty `HOME`, `TMPDIR`, `AF_CHECK_SCRATCH` and `XDG_CACHE_HOME`, removed
  after it: write to `$AF_CHECK_SCRATCH` or `$TMPDIR`, never `/tmp`. Provider probes run in a
  directory af makes for them, and af removes each Claude Worker Attempt's and probe's history from
  the Claude config directory (`keep_worker_transcripts` keeps it). It closes a finished Task's gate
  pull request and deletes its `af-gate/` branches (`keep_gate_pull_requests` keeps them) only while
  they still equal the Task's recorded repository, refs and commits, recording the result as
  `gate_cleanup` without changing the Task's result, and collects such a Task only after a cleanup
  is done.

## [0.11.0] - 2026-10-05

### Authority compatibility

Committed `.af/` policy from v0.10.0 keeps working as is. Remote Checks are opt-in twice: a check's `[checks.<name>.remote]` table is optional, and without an operator's machine-local mapping every check still runs locally. To move a project pin to v0.11.0, run `af onboard --refresh-lock --af 0.11.0`. This is a public alpha: compatibility guarantees begin at 1.0; persisted pre-GA Task and Campaign state is unsupported across upgrades, so finish or inspect a Task with the release that started it.

### Changes

- release: v0.10.0 (#154)
- Add af provider remove, and a d key for it in the Providers pane (#168)
- Sweep the sandbox directories a killed af process leaves in $TMPDIR (#169)
- Show TUI state in the brand's colours, as chips (#172)
- Pin af 0.10.0, and pin every release the workflow publishes (#173)
- Pin af 0.10.0 in .af/af.lock (#164)
- Fix executable onboarding and first-run guidance (#156)
- Open the TUI on a splash of three workers while it loads (#181)
- Add private-host Provider authentication (stage 1 of #122) (#171)
- Remote Checks: run a declared Task check in CI through a draft gate pull request (#178)

- Make the first review walkthrough executable with reviewed, locally committed authority and
  explicit policy, Base and candidate selectors; no remote push or Provider login is needed to
  inspect a plan. Clarify empty Change Sets and source-build versus signed-release pins.
- Select the software catalog profile explicitly in implementation entrypoints and keep tutorial
  Task requests outside committed source Snapshots. Exercise the documented Quickstart and
  generated tutorial through token-free planning in regression tests.

- Sandbox directories are named `af-sandbox-<pid>-…` under `$TMPDIR`, and every command that
  runs review or Task work (`af review run`, `af task start --execute`, `af task run`,
  `af provider doctor`) begins by removing the ones whose process no longer exists, finishing the
  removal before it exits. A sandbox is removed by the handle that owns it, so only a killed or
  aborted process leaves one behind, and a leftover tree that a Gate built into carries a whole
  `target/`: one development machine held 15 GB of them. The sweep keeps every directory whose
  process is still running and every directory the kernel preserved on purpose after an
  unconfirmed container cleanup (now marked `preserved` beside the tree), removes a tree only
  through directory descriptors opened without following links, touches nothing else under
  `$TMPDIR`, and reports what it removed in one stderr line.

- `af provider remove ID...` drops named Providers from the machine-local registry through the
  same locked, atomic publication `add` uses. Auth directories and their logins are left
  untouched, and the previous registry is preserved. It also repairs a registry made invalid by a
  deleted auth directory, runs in the invoking release from any repository, and refuses ambient
  IDs by name. In the browser, `d` on a registered Provider fills the `:` line with the command
  ([ADR-0136](docs/adr/0136-remove-a-registered-provider-by-id.md)).

- Add explicitly permissioned, private-host Provider authentication for browser-only setup and
  reauthentication, with Codex device approval, Claude code/callback support, bounded session
  state, recipient fencing and secret-free status. Generic setup login remains terminal-only;
  login never implicitly spends a model budget or resumes a Task
  ([ADR-0137](docs/adr/0137-permit-provider-logins-through-private-host-capabilities.md)).
- Include a runnable, consent-bound personal-chat host for validated one-time browser challenges,
  native Codex headless device login and Claude 2.1.289's exact unterminated prompt; reusable
  credentials remain native and ordinary af status stays challenge-free
  ([ADR-0139](docs/adr/0139-deliver-native-login-challenges-to-a-verified-private-requester.md)).
- Preserve native authentication failure categories alongside usage diagnostics and suppress
  credential-bearing failed output before ordinary capture, retaining exact parsed usage.
- Classify native login failures into closed response/proxy/TLS/rejection/transport states while
  keeping stderr private and bounded; retain the native lifetime guard and credential boundary.
  An absent or unstartable official CLI reports `provider_cli_missing` (exit 4).

- Remote Checks: a declared code check may add a `[checks.<name>.remote]` table (`executor =
  "github-pr"`, `workflow`, `required` job names), and an operator's machine-local mapping
  (`$XDG_CONFIG_HOME/af/remote-checks.toml` or `AF_TASK_REMOTE_CHECK_POLICY_FILE`) selects it per
  repository. Local checks run first; the kernel then builds two `af-gate/<task-id>/` branches
  from the Task's Snapshots in a private repository, pushes them without force, opens one draft
  pull request between them, waits on the declared workflow's `pull_request` run for that head
  commit, and accepts the result only after reading `refs/pull/<n>/merge` back as the candidate
  tree. Every remote fact is one `af/RemoteCheckEvidence@1`; a remote `CheckResult@1` names it
  instead of a command; every refusal is `not_run` with a named reason; the last 256 KiB of each
  unsuccessful job's log (1 MiB per check) is kept as the result's `stdout`; no record holds the
  push URL or the mapping's path. `af task show` prints the pull request, run, jobs, kept log
  excerpt and cleanup commands, and `--json` carries the evidence under `remote_checks`. Without
  a mapping nothing changes. This is the one operator-authorized exception to "publishing is a
  human action"; delivery still never pushes
  ([ADR-0140](docs/adr/0140-run-a-declared-check-through-a-gate-pull-request.md)).

- The TUI shows state in the brand's colours: chips of ink on blue (running), green (passed) and
  pink (failed or awaiting approval) on a Task's STATE, its stage marks, the progress in the bar,
  a Provider's STATUS and the Workers pane's Attempt counts, and an `error` chip before every
  error row. The status line turns pink while it carries an error, and the help header's worker
  is drawn in solid pink. Text on the terminal's own ground is never coloured, so every colour
  reads on a light or a dark terminal; `NO_COLOR` still leaves attributes only.

- This repository's own `.af/af.lock` pins `0.10.0` (it still pinned `0.9.0-rc.6`). From now on
  the release workflow pins each release it publishes, and `make check` fails when the pin falls
  more than one release behind `CHANGELOG.md`
  ([ADR-0138](docs/adr/0138-the-repository-pins-its-newest-release.md)).

- Bare `af` paints a splash a few milliseconds after it starts: the three workers on a conveyor
  belt, the one a Task block reaches lit in its colour, while the scope and its panes load behind
  it. The browser replaces it as soon as it has loaded, so `af` is never slower; outside a
  repository, where reading every Task Store takes seconds, the terminal no longer sits blank.
  `q` or `<C-c>` quits from the splash, and other keys typed meanwhile reach the browser.

## [0.10.0] - 2026-10-02

### Authority compatibility

Committed `.af/` policy keeps working as is.

### Changes

- release: v0.9.1 (#152)
- Fix Task writer lease under load, interrupted Workers, and unstartable Provider CLIs (#134, #135, #136) (#153)
- Research pipelines: warm checks, measure and compare, report Tasks, port bindings, store hygiene, and two verified research Tasks (#132)

- Ctrl-C (SIGINT) or SIGTERM during `af task run`, `af task start --execute`, `af review run`
  or `af provider doctor` no longer leaves Worker processes running after af exits. af stops
  and reaps every Worker process group, then ends by the same signal, so the shell reports 130
  (SIGINT) or 143 (SIGTERM). The interrupted Attempt is recorded as a failed, cancelled Attempt
  and keeps its charge. The Task is not finished; stderr says to resume it with
  `af task run TASK_ID`. A second Ctrl-C while stopping kills the remaining Worker groups and
  exits at once ([ADR-0129](docs/adr/0129-stop-task-workers-when-af-is-interrupted.md)).

- A live Task writer no longer fences itself with `Task writer lease is expired or fenced` on a
  loaded machine. The lease heartbeat now observes the lease through its own Store connection.
  It renews through that connection when the work still holds the shared one with 4 s of lease
  left. That connection waits at most 1 s for SQLite's write lock and retries with backoff, so
  neither a slow Store operation nor its write lock can outlast the lease. A renewal that
  commits after its lease expired is now refused inside the write transaction. A successor or an
  expired lease still fences and cancels the old writer, and is now seen without waiting for the
  held Store
  ([ADR-0128](docs/adr/0128-renew-a-live-task-writer-lease-through-its-own-connection.md)).

- A Provider CLI that cannot start is reported as a Provider installation failure instead of
  surfacing later as an unrelated Worker or credential error. A CLI cannot start when it is
  missing, not executable, or exits non-zero on its own `--version` check. `af provider status`
  marks the context `installation_failed` and exits 4, and `af provider setup` returns
  `provider_cli_missing`. Task Provider admission refuses the binding before any Worker is
  dispatched or any Attempt is charged. Each report names the Provider ID, the program path, the
  CLI's own first error line and the fix it suggests. A CLI that breaks after admission fails its
  Attempt as a Provider environment failure, not a model or credential failure
  ([ADR-0130](docs/adr/0130-report-a-provider-cli-that-cannot-start.md)).

- A Worker's draft reply may spell its `citations` and `repository_citations` in any order: the
  runner admits both sets in canonical order (sorted, unique) before validating the reply, so an
  author is judged on what it cited rather than on the order it listed it. This repository's
  `kernel/report` gives its author two Attempts.
- Store hygiene and the warm cache's two bounds (ADR-0135, package R5 of
  `docs/design/research-pipelines.md`; amends ADR-0131). `af task list --sizes` prints each Task's
  CAS bytes — those only it reaches and those it shares with another Task or Campaign record —
  and the Store's total; `--json` adds `sizes` to each entry and `store` to the document. `af
  task gc --older-than DAYS --keep N` previews, writing nothing, which finished Tasks beyond the
  newest `N` it would collect and why every other Task stays (`running`, `unfinished`,
  `writer_lease`, `bound_by`, `kept_newest`, `kept_recent`); `--apply` takes the Store's writer
  lock, is refused while any writer lease is live, appends one `task_collected` transition
  carrying `af/TaskCollected@1` per Task — referencing no artifact — and then removes every CAS
  object no uncollected record reaches, by a conservative walk through every digest a record
  spells. A stopped sweep is finished by the next run. A collected Task's projection stops at
  its tombstone: `af task list` and `af task show` print `collected <time>` with the retained
  summary (`af/task-collected-inspection@1` for `show --json`, a `collected` member on
  `af/task-list-entry@2`), `af task output`, `deliver`, `run` and `explain` refuse it, and replay
  never calls its removed objects corrupt; the browser lists the Tasks it can open. An append now
  rechecks, under the writer lock, that every object it references is still filed. `[warm]
  max_bytes` is now the eviction bound, applied before a check and after one whose result
  stands, and a new `[warm] hard_max_bytes` (twice `max_bytes` by default, at most 32 GiB) is
  the only bound that ends a running check with `warm_cache_bound_exceeded`;
  `TaskCacheObservationV1` records the acting `bound` and `af task show` prints it. New schemas
  `task-collected-v1.json`, `task-collected-inspection-v1.json` and `task-gc-v1.json`;
  `task-transition-v5.json`, `task-list-entry-v2.json`, `task-runtime-evidence-v1.json` and
  `code-task-policy-v1.json` gain the optional members. A Store without a tombstone and a policy
  without `hard_max_bytes` behave as before, except that such a policy's check now fails only
  above twice its `max_bytes`. A reader that loses an artifact to a concurrent sweep reports the
  Task as collected, a listing projects the uncollected Tasks before reading the tombstones, an
  opening Task re-checks every Task its bindings name under the writer lock, and a collected
  Task's row under `--sizes` carries a zero footprint.
- Bind any declared root port (ADR-0134, package R4 of `docs/design/research-pipelines.md`;
  amends ADR-0117). A Task file's `inputs` table may bind any root input the selected Pipeline
  declares — the one the Task file names, or else every captured Pipeline accepting its kind,
  alike — to a recorded, finished Task's result output whose artifacts verify in the CAS and
  whose type and cardinality equal the port's exactly; `requirements`, `base` and `continuation`
  stay refused by name, and `source`, `history` and `sources` keep ADR-0117's rules. Only result
  outputs bind: naming an Attempt's raw artifacts, runtime evidence or any other record is
  refused with a message saying so, and an exact `{ "artifact" }` reference binds only
  ADR-0117's three ports. A list of one to sixteen `{ "task", "port" }` references binds a `many`
  port in order, so `measurements` can take an experiment's `baseline` and `candidate`; a `one`
  port keeps its output's Snapshot ID, a `many` port bound from several outputs names none while
  each artifact keeps its own, and a list into a `one` port is refused. Every refusal names the
  port and both types before any Worker or Provider admission. The Store, the executor (for a
  `many` port the consuming contract declares `unbound`) and the Worker renderer accept such a
  Snapshot-less `many` input port; output ports keep the one-Snapshot rule. A port bound without
  naming the Pipeline must be declared alike by every Pipeline accepting the kind, else the Task
  file is asked to name one; a list bound to a `one` port has each reference judged before its
  shape; a bound `many` output records every artifact it holds; and `source`, `history` and
  `sources` are checked against the selected Pipeline's declaration at resolution. This repository's `kernel/analyst` and `kernel/report-verifier` schemas take the
  `snapshot_id` a bound Measurement or comparison carries.
  `af/TaskInputBindings@1` records a port's further outputs in an optional `also` list, absent
  for every single-reference binding, and `af task explain` and `af task show` print their
  existing binding rows once per output. `task-file-v1.json` gains the list form and
  `task-input-bindings-v1.json` the `also` list. The `builtin/report` starter's Worker schemas
  admit a `snapshot_id` on `comparison` and `measurements` values. A Task file without `inputs`
  is unchanged.
- Report Tasks (ADR-0133, package R3 of `docs/design/research-pipelines.md`). The built-in kind
  `report` selects a new installed profile: an author reads the source Snapshot, the kernel
  renders its draft and resolves its repository citations against that exact Manifest, and an
  independent verifier on the same Snapshot accepts the report. Its captured policy is
  `af.report-task-policy/1`, named by `report_policy` in `.af/task-catalog.toml` (a newly
  declared `.af/report-policy.toml`). A Task file's `report_sources` captures
  `af/ReportSources@1` — the `af.document-sources/1` shape with zero to 256 entries, 256 KiB each
  and 512 KiB in total in a file of at most 640 KiB, the empty set when absent; the `sources`
  port is optional, and a Pipeline that binds nothing there seals against the empty set. An
  execute-checks Worker may add beside the source, never under a name the source holds. `af/DocumentDraft@2` adds
  `repository_citations` of `{ path, line? }`, spelled exactly as the Manifest spells them and
  rendered as `path` or `path:line`. The installed `report_seal`, `report_check` and
  `report_accept` operators record `af/ReportCheckReceipt@1` (with each failed citation's
  reason: `absent`, `directory`, `symlink`, `binary`, `line_out_of_range`),
  `af/ReportEvaluation@1` and `af/ReportVerification@1`, every one naming the source Snapshot;
  a verifier whose checks judged another Snapshot is refused at admission. An author whose
  effects are `read-source` and `execute-checks` gets ADR-0118's shell in a clone that seals
  nothing back. A report Task allows no `write-source`, has no `snapshot` output and is refused
  by `af task deliver` with a message naming `af task output --port report`; `af task show`
  prints the report's title, the verifier's outcome and the cited Snapshot. `af catalog init
  --profile report` emits the credential-free `builtin/report` starter, and this repository's
  `kernel/report` Pipeline with `kernel/analyst` and `kernel/report-verifier` is staged in
  `fixtures/kernel-report/` for installation into `.af/`. New schemas: `report-sources-v1.json`,
  `document-draft-v2.json`, `report-check-receipt-v1.json`, `report-evaluation-v1.json`,
  `report-verification-v1.json` and `report-task-policy-v1.json`; the catalog, Task-file,
  Task-kind and operator schemas gain the new fields, profile and operators. Document and
  implement Tasks are unchanged.
- Measure and compare (ADR-0132, package R2 of `docs/design/research-pipelines.md`). A code
  policy may declare `[measures.<name>]` — a command, 1 to 16 `repetitions`, `warm`, `wall_ms`
  per repetition and `metrics` of `{ key, unit }` in `ms`, `bytes`, `count` or `ratio` — and
  `[objectives.<name>]` — a measure, a metric, `lower` or `higher`, `min_improvement_ratio`
  (decimal text such as `"0.1"` or the integer 0 or 1; a float is refused because the parser has
  rounded it) and `min_repetitions`. The installed `measure` operator runs a measure against a
  fresh read-only Snapshot per repetition with a private `HOME`, `TMPDIR`, `XDG_CACHE_HOME` and,
  unless `warm = true` binds the Warm Check Cache, `CARGO_TARGET_DIR`. It re-verifies the source
  after every repetition and records `af/Measurement@1`: every run's elapsed time, exit status,
  output digests, the cache condition it actually had (warm or cold, bytes, and why when cold)
  and the metrics the command reported on an `af.measure-report/1` last line with the declared
  keys and units. A repetition the kernel ends at a time bound is recorded from the supervisor's
  typed ending as `timeout` or `deadline`, with what it printed kept, and a command a signal
  ended records no exit code. A failure (`exit`, `timeout`, `deadline`,
  `malformed_report`, `unit_mismatch`, `source_mutated`) stops the measurement and leaves no
  summary. The installed `compare` operator folds two Measurements into
  `af/MeasurementComparison@1` in exact decimal arithmetic: medians with an exact even-sample
  mean, signed improvements, ratios in lowest terms, and `improved`, `below_threshold`,
  `unchanged`, `regressed` or `inconclusive` per metric. It is `passed` only for `improved` on
  the objective's metric. The plan compiler refuses a measure node whose repetitions exceed
  `check_wall_ms`, undeclared measures and objectives, and mixed comparisons. `af task output
  --port comparison --format markdown` renders one table, `af task show` prints medians and
  conclusions, and `af catalog init --profile experiment` emits the `builtin/experiment`
  starter, whose evaluator runs only after passed checks and a passed comparison.
  `scripts/measure-release.sh` and this repository's `release_build` measure,
  `release_build_time` objective and `kernel/experiment` packages are staged in
  `fixtures/kernel-experiment/` for installation into `.af/`. New schemas:
  `measurement-v1.json` and `measurement-comparison-v1.json`; `code-task-policy-v1.json` and the
  operator schema gain the new tables and operators. A policy without them is captured, planned
  and shown exactly as before.
- Warm Task checks bind the kernel's rustup home and keep Cargo's home warm (ADR-0131 amended,
  package R1 of `docs/design/research-pipelines.md`). Under `[warm]` a check and its toolchain
  probe receive `RUSTUP_HOME`, from the kernel's own, else its `HOME`'s `.rustup`, and
  `RUSTUP_AUTO_INSTALL=0`. A rustup proxy therefore answers from the installed toolchain
  instead of downloading one into the check's fresh `HOME`. That download is what made the
  probe exceed its 30 s bound. `RUSTUP_HOME` joins the toolchain key, and where it came from,
  or why it is unset, is recorded. `build_cache` admits `cargo_home`, bound as `CARGO_HOME`
  beside `cargo_target` under one toolchain key and one shared `max_bytes`. A `cargo_home`
  holding `credentials.toml` is suspect. A declared `caches = ["cargo"]` supersedes it
  (`cargo_home:superseded`). Four verification findings are closed. First, the directories are
  measured again after every check, so a fast check that wrote past the bound fails with
  `warm_cache_bound_exceeded` too. Second, traversal is descriptor-relative
  (`openat`/`fstatat`, `O_NOFOLLOW`) and fails closed: an unreadable subtree makes a
  directory suspect before reuse and fails a check that left it. Third, `ensure()` reports a
  discard, and bytes are measured only after it, so a recreated directory is cold, never its
  old size. Fourth, every warm check, started or not, keeps one evidence group naming it
  (`TaskRuntimeEvidence@1` gains an optional `check` binding) with one observation per
  declared kind, such as `deadline_exhausted` or `cache_refused`. `af task show` never prints
  an unnamed line. A checked-in golden recorded by a kernel without this package pins every
  document of a Task without `[warm]`, and a fixture proves from the implementer's sandbox
  manifest, the sealed candidate, the derived Snapshot and the delivered tree that no cache
  byte reaches them.
- Warm Task checks (ADR-0131, `docs/design/research-pipelines.md` package R1). A code policy
  may declare `[warm] build_cache = ["cargo_target"]`, `caches = ["cargo"]` and `max_bytes`
  (default 8 GiB, at most 32 GiB). A `trusted_local` Task check then builds into
  `$XDG_CACHE_HOME/af/task-build-cache/<project>/<toolchain>/cargo_target`. The toolchain key
  digests the Snapshot's `rust-toolchain.toml`, `rustc -vV`, `cargo -vV`, the host triple and
  the check's fixed environment. The directory has one exclusive lock; a check that waits 60 s
  for it runs cold. It is bounded before, during (`warm_cache_bound_exceeded`) and after every
  check, and removed rather than repaired. Declared Cache Snapshots bind `CARGO_HOME` from the
  check's runtime directory. `[warm]` with `require_container = true` is refused at load. Each
  warm check records its own `TaskRuntimeEvidence@1` with one cache observation per kind, whose
  `kind` may carry a `:reason` suffix. `af task show` prints `check <name>: <ms> ms, cargo_target
  warm <bytes>` or `cold <reason>`. `schemas/code-task-policy-v1.json` is new. `scripts/verify.sh`
  honours a `CARGO_TARGET_DIR` that is already set. A policy without `[warm]` is captured, run
  and shown exactly as before. A check holds an exclusive lock over its whole toolchain key from preparation to the end of
  its removal step, the bound is measured over the whole key, a directory that is suspect once
  the check ended (a link, a special file, a forbidden `credentials.toml`, a root swapped for a
  link) fails the check with `warm_cache_suspect` and is removed under the lock, and a root that
  is no longer a real directory counts as above every bound. Every compile-time
  `CARGO_MANIFEST_DIR` in the workspace is now a fallback behind the run-time `AF_WORKSPACE_ROOT`,
  because a warm gate reuses test binaries compiled in the previous gate's sandbox, and a test
  refuses a new one; this repository bounds its warm cache at 16 GiB. Every operation below a
  toolchain key goes through the key directory's descriptor, never a path, so a check that swaps
  the key's parent for a link cannot redirect cleanup; the shared bound is measured over the whole
  key before and after every check and an eviction removes every kind below it; and an
  observation's `evicted_bytes` carries its `evicted_reason`, which `af task show` prints. Only the
  two warm kinds are ever locked and only the kernel's own lock files are exempt from the bound
  (they are truncated on acquisition), a policy whose kinds a Cache Snapshot supersedes still holds
  and bounds the key, and a check whose warm directory is suspect once it ended fails even when
  nothing was left to evict. Only the lock inodes the holder opened are exempt from the key's
  bound, the key directory itself must stay private and its held locks in place for a check to be
  accepted (a widened key is emptied before reuse, never repaired), and a check that holds the
  key is monitored and judged even when every declared kind is superseded. A held lock is exempt
  only at its own name with its own inode and must stay a single empty name; the key is made
  writable through its held descriptor before an eviction, entries are removed by their exact
  bytes, and an eviction that leaves anything behind is an error; every declared observation of a
  failed check carries the eviction and its cause. A lock's inode is judged a plain, singly linked
  file of this user at its name before acquisition writes through it, a key that a check renamed
  and recreated is displaced and suspect, eviction empties or drops the held locks and reports
  success only when nothing but sound empty locks remains, and suspicion outranks the byte count
  in the recorded cause. ADR-0131 states where these rules stop: `trusted_local` is not
  isolation, and the cache is honest as evidence, not a defence against a check acting on the host. A link or a
  directory at a lock's name is removed before the name is opened, and a waiter judges the inode
  again after acquiring the lock, before truncating it. A held lock a check grows counts toward
  the running bound, removal addresses every entry by its exact bytes, and a directory's
  `source_digest` no longer varies with the lookup's outcome.

## [0.9.1] - 2026-10-01

### Authority compatibility

Current-shape committed .af/ authority needs no edit for these fixes; updating the installed default does not change project pins. To adopt 0.9.1 deliberately, run af onboard --refresh-lock --af 0.9.1 online and review/commit the authority. Native Rust snapshots require an explicit captured request and operator mapping; absent mappings preserve cold setup. Pre-GA (0.x) Task/Campaign state has no cross-release compatibility guarantee and is not migrated; retain its original release to finish or inspect it.

### Changes

- Explain Task verification budget rejections (#145)
- fix: clarify review timeout scope and externalize read-only gate reports (#146)
- Keep Claude Task Workers running on Claude Code 2.1.285 and across its updates (#147)
- feat(tasks): pinned private Rust snapshots for native checks (#151)

- Infeasible Task verification reserves now report required and available tokens, Attempts and
  wall milliseconds, identify exceeded resources, and list each protected node's contribution,
  including Provider admission. These are declared allowances, not estimated runtime; admission
  rules and configured budgets are unchanged.

- Keep nextest CI JUnit reports outside read-only source trees during `scripts/verify.sh`,
  alongside the existing external Cargo target. Unique per-run report directories preserve
  failed-run evidence; temporary store-only tool configuration is removed afterward. Direct
  `make test` retains its existing report location and all test/profile settings are unchanged.
- Run `make check` and `scripts/verify.sh` as an unprivileged user, including in containers.
  The new sealed-source regression requires mode bits to deny writes; root or
  `CAP_DAC_OVERRIDE` bypasses that seal and fails the explicit precondition rather than
  silently skipping the regression.

- Correct the `af review run --timeout-secs` help: without `--file` (including the default
  routed run and explicit `--campaign`/`--pipeline`) it bounds each
  reviewer Attempt (default 1800 seconds, pinned in the Campaign manifest as
  `reviewer_timeout_seconds`), not the whole run; with `--file` it caps the whole Task at the
  lower of the flag and the file's `limits.wall_ms`. Timeout behaviour is unchanged.

- Claude Task Workers no longer fail with `Native billing usage is incomplete` on Claude Code
  2.1.285, whose final result counts requests in `modelUsage` that its top-level `usage` leaves
  out. A breakdown that no top-level counter exceeds is charged as the complete bill; a top-level
  counter above the breakdown is still refused
  ([ADR-0125](docs/adr/0125-charge-a-claude-model-breakdown-that-covers-the-top-level-summary.md)).
- An Attempt refused for incomplete billing now names the cause in its diagnostic, for example
  `Native billing usage is incomplete: Claude top-level and per-model usage cannot be reconciled`.
- A Claude Code or Codex update in the middle of a Task no longer refuses the Task's next model
  Worker with `Captured Task Provider identity is no longer current`. The Task keeps running the
  client executable it captured when it started, and moves to the installed client if the update
  removed that file; the account is still proven before every invocation
  ([ADR-0126](docs/adr/0126-keep-the-captured-native-executable-when-its-launcher-moves.md)).

- Native code checks can opt into pinned Rust from an explicit operator mapping; AF verifies and privately copies only that toolchain before dispatch, avoiding repeated rustup downloads in fresh check homes. Host Cargo/Rustup homes are not passed through, inherited Cargo subcommands remain available, and check writes are never promoted. Missing mappings preserve cold setup; invalid mappings fail closed. Trusted-local isolation is unchanged ([ADR-0127](docs/adr/0127-snapshot-pinned-rust-before-native-task-checks.md)).

## [0.9.0] - 2026-09-30

### Authority compatibility

Committed `.af/` policy from v0.9.0-rc.9 keeps working as is. To move a project pin to v0.9.0, run `af onboard --refresh-lock --af 0.9.0`. This is a public alpha: compatibility guarantees begin at 1.0; persisted pre-GA state is unsupported across upgrades.

### Changes

- Publish the tested v0.9.0-rc.9 CLI as the public alpha, available through the standard installer
  and `af self update`. Includes Task execution, sandboxed Review and the terminal browser.
- Mark the README as public alpha and document security support for the latest alpha.
- Add advisory Task preflight for lineage, budgets, and evidence (#142)

- Add an offline advisory orchestration helper, `scripts/task-preflight.py`, for original-source
  identity, declared admission/Worker allowances and explicit acceptance-evidence mappings.
  It reads native inspection exports without changing Task authority, budgets or acceptance;
  ambiguous replay, live reservations and semantic completeness remain explicitly unverified.

## [0.9.0-rc.9] - 2026-09-29

### Authority compatibility

Committed `.af/` policy keeps working as is.

### Changes

- release: v0.9.0-rc.8 (#126)
- af self update: keep unverified --version out of the latest cache; make a failed update unmistakable (#128)
- brand: the afactory design system for the README, the website and the TUI (#127)
- Bump jsonschema from 0.56.0 to 0.57.0 (#125)
- TUI: fast Workers open; list old-release Stores once; uix measures latency (#130)
- tui: the bare ? help opens with the pixel worker; CHANGELOG entry for the brand (#129)
- Halve the gate: parallel test processes, linked-once test binaries, a compile cache that hits (#131)
- Change notes per pull request under changelog.d/; CHANGELOG.md never conflicts (#140)
- providers: a 15-second Codex probe budget (#133)
- TUI: Pipelines read in batches; spawn-count guard; index flags do not hide drift (#138)

- The design system: `brand/` keeps the designer's wordmark (AFACTORY with three workers) and
  derives every asset from it with `brand/gen.py`; `brand/tokens.css` and `tokens.json` carry
  the palette, type, spacing and radii; `brand/README.md` is the brand book. The README opens
  with the banner; the tagline is "agent pipelines made fast". The browser paints its status
  line ink on the brand's blue and its errors pink where `COLORTERM` says the terminal takes
  truecolor, and as before otherwise (#127).
- The browser's bare `?` help opens with the pixel worker, the version and the tagline above
  the key reference.

- `make check` runs its tests one process per test across every test binary at once
  (`cargo nextest run --profile ci`), with `TEST_THREADS` set from the machine's cores; the
  five tests that assert against a fixed real-time budget run alone and first, listed in
  `.config/nextest.toml`. The longest tests are split one case per test, the eight largest
  crates link their integration tests as one binary under `tests/it/`, doctests are off (there
  are none), and debug information is line tables only. CI compiles through `sccache` keyed
  by rustc inputs, caches only the crate registry, links with `lld` on Linux, and lints in a
  job beside the tests
  ([ADR-0124](docs/adr/0124-run-tests-in-parallel-processes-and-link-them-once.md)). Measured
  on a 14-core developer machine: the test step 661 s to 289 s, a cold test build 58 s to
  46 s with 122 to 48 executables and 5.7 GB to 3.5 GB, the doctest step 22 s to none.

- The browser's Workers pane opens in a tenth of a second instead of most of one: it reads every
  committed declaration, prompt and lock with one `git cat-file --batch` and checks their drift
  with one `git diff --literal-pathspecs --name-only`, side by side, where it spawned about four
  git processes per Worker on every open (0.79s to 0.06-0.13s for 14 Workers on a loaded
  machine). A file name with `*` or `[` is now compared literally.
- The Tasks pane lists the Task Stores an earlier (pre-GA) af release wrote as one bar entry,
  `! N old Stores`, and one note saying where they are and that af does not read pre-GA state
  (ADR-0113), instead of an error row each. A Store refused for any other reason still shows its
  own error. `review_core::event::ANOTHER_RELEASE` is the phrase both sides share.

- The Codex status and subscription probes allow 15 seconds instead of 5. On a loaded machine
  (a full build, an IDE indexing) starting `codex` alone took long enough that `af task run`
  refused a healthy provider with `Codex subscription probe timed out after 5 seconds`, and
  provider tests failed in Task gates. Claude's structural probe already allows 30 seconds.

- The browser's Pipelines pane reads every committed pipeline, the catalog and their drift
  with the same two batched git calls as the Workers pane, instead of two git processes per
  pipeline. Tests assert both panes spawn as many git processes for 30 entries as for 2, so
  a per-item read cannot come back unnoticed.
- A declaration marked `skip-worktree` or `assume-unchanged` in the index is still marked `*`
  when its working-tree copy differs from `HEAD`: `git diff` does not compare such files, so the
  panes hash those, and only those, against the committed blob.

## [0.9.0-rc.8] - 2026-09-28

### Authority compatibility

Committed `.af/` policy keeps working as is; a source-writing Worker that declares `execute-checks` gets a shell, and only then (ADR-0120).

### Changes

- release: v0.9.0-rc.7 (#118)
- Give a source-writing Worker that declares execute-checks a shell (ADR-0120) (#119)
- The af browser's Tasks pane (docs/design/tui.md §5.5) (#120)
- TUI: the Workers pane (M4) (#123)
- TUI: hand af commands off from the : line (M5) (#124)
- Give a source-writing Worker that declares `execute-checks` a shell (ADR-0120): `worker_access`
  maps `write-source` plus `execute-checks` to `WorkerAccess::WriteSourceWithShell`, the Claude
  adapter grants `Read,Glob,Grep,Edit,Write,Bash` and Codex runs `workspace-write`, and the shell's
  process group dies with the Attempt. Candidate capture skips the scratch a shell leaves: paths
  under a top-level name the source did not hold, and new top-level dotfiles
  (`SealedSandbox::capture_snapshot_where`). A writer without `execute-checks` is unchanged. The
  kernel's `kernel/implementer` declares it and formats, lints and tests before replying.
- The browser's Tasks pane (ADR-0121, `docs/design/tui.md` package M3) lists the scope's Tasks
  under `running/`, `awaiting approval/`, `done/` and `failed/`, newest first, as
  `task-id  outcome  progress%`. The user scope adds one level per repository's Task state. A
  Store the binary cannot read is an error row naming its directory. One Task's pane shows TASK,
  PLAN and SNAP lines, one PROGRESS row per stage of the plan's graph order (`[ok]`, `[..]` with
  its attempt, `[!!]`, `[  ]`, with recorded wall and charged tokens), the TOKENS and TIME totals,
  and one HISTORY row per event. Every value comes from `af task list --json` and the
  `af task explain --json` document (`af task show --json` with the plan and its graph), or from an
  artifact they name. A running opened Task is read again about once a second off the key loop.
  `Enter` on a HISTORY row shows its artifact, `p` opens the Task's pipeline, `y` yanks the Task
  id and `R` reads again. The `af task show` builder is split from its printer, and
  `default_task_state` is the one spelling of the default `--state`. No `--json` document
  changes.
- The browser's Workers pane (ADR-0122, `docs/design/tui.md` package M4) lists the reviewer
  Workers and Task Worker packages `HEAD` commits under `.af/workers/`, `.af/task-packages/`,
  `.af/packages/` and `.af/vendor/`, grouped by source when more than one has Workers. The
  folder is read when it is first opened. One Worker's pane shows IDENTITY (its declaration and
  its `af.lock` or catalog pin, `unpinned` or `pinned elsewhere`), then STATE, then PROMPT
  (`reviewer.md` or `instructions.md` as committed, with `gf` opening the working-tree file).
  STATE counts the Attempts in this scope's Task Stores whose invocation ran a slot that the
  recorded plan binds to the Worker at the digest the committed pin records (other digests of
  the name on one `other` line), a Review shard's through the node that owns it, and a
  reviewer and a Task package of one name apart: reserved, settled ok, settled failed and
  released, plus the tokens charged and the wall time. Every open reads again. A drifted file is marked `*`, and a failed read of
  `HEAD` is shown above the last good entries. No `--json` document changes.
- The browser's `:` line runs `af` commands (ADR-0123, `docs/design/tui.md` package M5). A line
  that clap parses and the browser does not own (`:q`, `:cd`, `:scope`, `:e` and `:help` keep
  their behaviour) runs as a child of the running `af` executable, with exactly the parsed words
  as its arguments. It runs in the scope's root, inherits the environment, and carries
  `AF_DISPATCHED_FROM`, so it is never dispatched to another release. The browser releases the
  terminal, and the child's process group owns the foreground, so `<C-c>` stops the child, not
  the browser. The released screen then shows `af LINE: exit N` (or the signal) and waits for
  Enter. After it, the browser reads again the scope, the settings and the Tasks, Workers and
  Providers panes (without the charged probe), keeping what is opened. A line without a
  subcommand, which would open a second browser, is refused. `<Tab>` completes subcommands at
  every level from the clap definition, and a Task ID argument from the Tasks pane. In the Tasks
  pane, `r` prefills `task run ID --confirm-plan PLAN`, and `D` on a verified Task prefills
  `task deliver ID --branch af/ID --worktree ../ID --confirm` and a space, with the
  confirmation left to type. `D` on an unverified Task says why. No `--json` document changes.

## [0.9.0-rc.7] - 2026-09-24

### Authority compatibility

Committed `.af/` policy keeps working as is; `execute-checks` on a review Worker is opt-in.

### Changes
 Stage progress counts only records of the Task's current plan, so a refresh or a Review continuation that reuses node names starts from nothing; a dynamic Scatter's completion settles its parent; an invocation without a reserved Attempt is not shown running; a finished Task's elapsed and wall time end at its `finished` transition; a Task the Store lists but the pane cannot inspect refuses the Store with the cause instead of being grouped by a partial summary. Opening a Task that can no longer be inspected refuses its Store the same way; a HISTORY row opens the artifact its change is about by change kind (a revocation, not the decision; a report, not an event id); an existing state directory that is not a Task Store, or one the process may not read, is refused, never listed as empty (`task_execution::store_present`), a dangling `events.sqlite` link included. A load or `R` inspects every listed Task again rather than reusing a finished Task's cached summary. A state directory that exists but cannot be listed is refused; an opened Task awaiting approval is read live too, since the CLI may start it; `p` opens the package the committed catalog pins under the Task's Pipeline name at a fresh read of `HEAD`, never another file declaring that name; a Store refused on a live read closes its opened Task. A load or `R` re-resolves every recorded artifact as well; PROGRESS counts a failed stage only once no Attempt is left for it; `p` is refused when `HEAD` cannot be read; a refreshed Task's time ends at its last finish; the bar groups a Task by the phase its inspection read; opening another Task drops the previous one's live read; the user scope reads a symlinked Task state directory and refuses a link to nothing. A bar row's outcome and charge come from the same inspection read as its group, and a Store refused on a live read also closes an artifact opened from the Task's HISTORY. A reservation released before dispatch is not counted as an Attempt; a stage's charge takes an Attempt's highest recorded charge, a usage observation included even when it arrives after the settlement; a failed report of an unfinished Task leaves a stage with Attempts left open; a record newer than the last run report decides its stage, so a retry after a failed report shows; opening a Task rebuilds its bar row from the same read as its detail; cached summaries and artifact lookups are kept per Store, so one Store's reads never stand in for another's; a run report not superseded by newer records decides its stage over an older success (a later suppression shows skipped); `crates/af/tests/tui.rs` also drives the Tasks pane at 80x24 on a pseudo-terminal; `p` works again once `HEAD` reads again after a failure; the TASK line cuts a long id before it hides the goal; a stage's latest settlement decides its mark (a newer failure after a success is failed); a node suppressed for a missing upstream stays open while the Task can still recover; a HISTORY artifact the Store cannot give back refuses the Store.
- release: v0.9.0-rc.6 (#107)
- Name the CLI crate after its binary: crates/af (#108)
- Bump base64 from 0.22.1 to 0.23.1 (#103)
- Bump the cargo-minor-and-patch group across 1 directory with 3 updates (#101)
- test: stop reporting a reaped descendant as live after cancellation (#109)
- Take the toml, toml_edit and jsonschema updates, parsing TOML documents as tables (#111)
- Bump toml from 0.8.23 to 1.1.6+spec-1.1.0 (#102)
- Bump toml_edit from 0.22.27 to 0.25.15+spec-1.1.0 (#105)
- Bump jsonschema from 0.26.2 to 0.56.0 (#104)
- Make Provider onboarding safe for agents (#112)
- Remove pre-GA legacy, migration and compatibility code (#114)
- Keep .af/ to declarations: layout table, undeclared-path report, Task input bindings (ADR-0112..0114) (#113)
- Pin Opus 5.5 and GPT-6 Sol defaults (#116)
- Fix source install adoption by af self (#115)
- Bare af opens a vim-native TUI; review Workers may run a shell (execute-checks) (#117)
- Bare `af` at a terminal opens a read-first, vim-shaped browser (ADR-0119,
  `docs/design/tui.md` package M1). On a pipe, bare `af` still prints help to stderr and exits
  2. The scope comes from `config::load`: the user scope outside a repository, the project scope
  inside one, or `af --repo DIR`. A 28-column folding bar holds `providers/`, `workers/`,
  `pipelines/` and `tasks/`. Beside it, the Settings pane shows the layer table and the effective
  values with their origins, as `af config paths` and `af config show --origin` print them, and
  `e` opens a layer with `af config edit`. The Providers pane shows the `af provider status`
  columns with a bar per quota window, and `R` runs the bounded usage probe off the key loop. The
  Pipelines pane shows, verbatim, the `af task explain --tree` text of a token-free plan compiled
  as `af task plan` compiles one, into a scratch Store; the status line shows the highlighted
  slot's Worker binding. The Workers and Tasks panes arrive in later packages. `:` lines are
  parsed by the CLI's own clap definition. The terminal runs through `rustix::termios`, an
  existing dependency, instead of the planned `crossterm`. No `--json` document, schema or
  fixture changes. Discovery under `.af/task-packages/` walks every real directory, however deep, and keeps walking below a package. Escape sequences split across terminal reads decode whole (`tui::keymap::Decoder`), and a lone `ESC` is Escape only after a read brought nothing. Outside a repository the user scope never opens a directory layer, so a broken `.af/af.toml` above a plain directory cannot block it. `af --repo DIR` with no subcommand is exempt from pin dispatch like bare `af`. The bar lists the pipelines `HEAD` commits and reads every declaration from `HEAD`, the authority a preview compiles, marking a working-tree file that differs; a package the kernel refuses to plan shows its declared contract; a review Pipeline shows its committed declaration. `af config edit user` names the user layer without reading the ladder, so a directory layer that does not parse cannot block it. `e` on a highlighted layer opens that exact file, so two present directory layers cannot swap; a settings refresh or an editor hand-off whose reload fails says the settings are stale and why, instead of calling them refreshed; the Worker-binding status follows a modified package's notice row; a malformed escape stream is bounded and dropped through its terminator. Git runs for the pipelines pane with a cleared environment and an allowlist, so an inherited `GIT_DIR` cannot redirect it; a failed read of `HEAD` is an error above the last good entries, not an empty catalog; entries are bound to one resolved commit and read again before a preview when `HEAD` moved; a package is previewed only when the committed catalog pins its name at that file; an editor hand-off refreshes the opened pane; the status line yields the breadcrumb before cutting a binding or an error at 80 columns. A preview compiles the exact commit its entries were read from (`plan_tree_preview_at`), and a result for a commit the pane left is dropped; drift is judged by `git diff` (bytes, mode, existence); a failed `HEAD` read is shown on an opened entry too and nothing is compiled from stale entries; a scope change drops the previous repository's entries; the terminal's panic hook restores the screen only from the thread that owns it. `R` acts on the bar's selected node while the bar has focus; an editor hand-off from the bar refreshes that node's pane too and the bar follows; opening an entry the new `HEAD` no longer commits falls back to its folder; a failed reload after an editor is reported as stale settings; an unborn `HEAD` is told apart from a branch naming a missing commit. The kernel's review-light reviewers get two Attempts each, so a malformed reply is retried instead of ending the round. A toplevel is an ancestor whose `.git` git would open, not any path so named; a cancelled provider probe is forgotten and the last complete inventory stays; a modified pipeline's bar label carries `*`; a reload that finds the place became another scope enters it and drops the old entries.
- Let a review Worker that declares `execute-checks` build and drive the candidate (ADR-0118). A
  model Worker whose `roles` contain `review` and whose effects are
  `["read-source", "execute-checks"]` now runs in the same `Mode::EphemeralWrite` clone that
  AF-owned preparation uses, with its readable Review inputs, instead of a read-only tree. The
  Claude adapter adds `Bash` to its adapter-owned `--tools` and `--allowedTools` and keeps
  `--safe-mode --restricted --permission-mode dontAsk --strict-mcp-config`. Codex runs
  `-s workspace-write` rooted at the sandbox. The model process's whole group is killed when it
  exits, so a shell child cannot outlive the Attempt; wall-clock and token bounds are unchanged.
  Nothing is sealed back. At `finish` every Snapshot entry must be byte-identical; anything the
  reviewer added — build output, its harness, the dotfiles tools write into `HOME`, which is the
  sandbox root — is discarded with the clone and is not a source edit.
  Any source edit fails the Attempt with a diagnostic naming the changed paths, and a read-only
  Worker's refusal names its paths the same way. The adapter's `writable: bool` became the
  kernel-derived `review_runner::task::WorkerAccess`, computed from the captured effects by
  `review_pipeline::task::source::worker_access`, so no package or `.af/` policy can name a tool.
  Existing Workers derive exactly the access they had; fixtures and `--json` documents are
  unchanged. A command Worker declaring the same effect runs under the process-group-killing
  exit policy too, so a background child it starts cannot outlive its Attempt
  (`crates/review-runner/tests/task_command_process_group.rs`).
- Kill a supervised child's process group before reaping the child, not after: a reaped pid is
  free for reuse, so the late `SIGKILL` could land on an unrelated process that had just been
  spawned into its own group under the recycled id — on a loaded machine, a fresh `git rev-parse`
  dying with an empty stderr. `review-process` now observes the exit without reaping
  (`waitid(WNOWAIT)` on Linux, kqueue `NOTE_EXIT` on macOS and the BSDs), routes every group
  kill through the unreaped leader, and reaps last on every path — deadline, cancellation and
  held-pipe cleanup included; the provider probes poll the same way, ending a probe's group
  before reaping it. A failed git command with no stderr now reports its exit status, so a
  signalled git reads as signalled. The two provider lock files are opened with a
  bounded retry on the spurious `ENOENT` APFS returns to a concurrent `openat(O_CREAT)`.
- Declare the `.af/` layout once and keep Task files out of git (ADR-0115): every canonical entry
  under `.af/` — its kind, its writer of record, whether git versions it and what it holds — is
  now one table in `review_config::layout`, and `layout::classify` answers whether any
  repository-relative path is declared or undeclared. `af help config` renders that table and one
  paragraph saying what never belongs under `.af/` and where it lives instead, and the old claim that `af init`
  gitignores `af.local.toml` is gone — there is no `af init`, and no command writes a `.gitignore`. A test walks every production source under `crates/*/src` and
  fails on any `.af/` path the table does not declare, which added `document-policy.toml`,
  `packages/`, `artifact-reuse/`, `cache/` and the in-memory-only `task-compat/` to the declared
  set. `af task plan`, `af task start --file`, `af review plan --file` and `af review run --file`
  now warn on stderr, after capture, when the Task file resolves inside the repository and `git
  check-ignore` does not ignore it, naming the file and `$XDG_STATE_HOME/af/tasks/`. The warning
  is advisory: exit codes, stdout and every `--json` document are unchanged, and a Task file that
  is gitignored or outside the repository draws no warning.
- Report undeclared `.af/` paths and let a project refuse to deliver them (ADR-0116):
  `review_config::layout::classify_manifest` groups every `.af/` path of a captured Snapshot
  manifest into declared and undeclared, with a path count and a byte total for each. It is a
  pure function of the manifest and the L1 table — it reads recorded entries, never a working
  tree or a sandbox, and judges a path by its decoded bytes while keeping the manifest spelling —
  so the same Snapshot answers the same on every machine. `af task plan` and `af task start` now
  print one advisory line naming the count, the byte total and up to ten paths, and carry the
  whole group in a typed `undeclared_af_paths` field of the `--json` document; `af task deliver`
  records it in `af/task-delivery@1` beside `ignored_paths` and prints it in the delivery
  summary. A new project policy `[delivery] undeclared_af_paths = "warn" | "refuse"` in
  `.af/af.toml` defaults to `warn`; under `refuse`, delivery fails before the prepared record and
  before any Git mutation, naming every undeclared path and leaving no branch, no worktree and no
  record. The policy is captured into the Task's project policy identity at plan time and read
  back from that record at delivery, so editing `.af/af.toml` afterwards cannot change an
  admitted plan, while the default is not written down and moves no existing policy identity.
  Nothing is removed: Snapshot identity, the delivered tree and `ignored_paths` are exactly what
  they were, receipts written before the field deserialize with an empty group, and a repository
  whose authority tree is fully declared reports nothing at all.
- Decide how a Task file binds a root input port to a recorded Task's output (ADR-0117, proposed;
  documentation only, no behaviour changes yet). A Task file gains one optional `inputs` table
  mapping `source`, `history` or `sources` to `{ "task": "<task_id>", "port": "<output port>" }`
  or to an exact `{ "artifact": "sha256:…" }`, so chaining implementation to repair to review no
  longer means exporting a candidate tree and reviewer results to files that then ride along in
  every later Snapshot. References resolve once, at plan time, from the `--state` Store into exact
  artifact IDs in the compiled plan, so resume, retry and replay never read the referencing Task
  file again; type and cardinality are checked against the port before any Worker or Provider
  admission, and the compiler's existing root-input check remains the one that cannot be
  bypassed. The referenced Task must be recorded and finished with the named port in its result,
  but need not be verified — the reference carries provenance only, and no acceptance,
  verification, plan approval, delivery or budget authority crosses. A bound `source` that is
  already a root capture is carried verbatim and delivers as usual; a derived one — what a
  `snapshot` output is — is republished as a root Snapshot over the identical Manifest with a new
  `af.task-source-origin/2` origin that names the referenced Task, result and port and carries no
  `source_revision`, so `af task deliver` refuses it for the one honest reason: a derived tree has
  no commit for the target's `HEAD` to equal. ADR-0031's exact comparison is unchanged. The Task-file shape,
  the display in `af task explain` and `af task show`, and the implementing package's crates,
  types and tests are written up in `docs/task-execution/task-inputs.md`, linked from
  `docs/README.md`.
- Bind a Task input port to a recorded Task's output (ADR-0117, accepted; the behaviour the
  entry above decided). A Task file's optional `inputs` table now resolves at plan time, from
  the `--state` Store only, into ordinary root ports: the referenced Task must be recorded and
  `Finished`, its result must read and validate, the named port must be in `result.outputs` and
  every artifact it names must verify in the CAS, and the recorded type and cardinality must
  equal the destination port's exactly — a `many` output never binds a `one` port however many
  artifacts it holds. `requirements`, `base`, `continuation` and any other name are refused by
  name. Every refusal is an ordinary Task-file input error naming the Task and the port, exit 1
  with `af/error@1` under `--json`, raised while the revision is still being built and therefore
  before any Worker dispatch or Provider admission. A bound `source` that is already a root
  capture is carried verbatim; a derived one is republished as a root Snapshot over the
  identical Manifest with a new `af.task-source-origin/2` origin and a new `af/SourceTree@1`
  envelope, and a parentless generation-2 Snapshot referenced again is carried verbatim, so
  nothing is re-rooted twice. `history` and `sources` carry the referenced artifact ID verbatim;
  a bound `history` suppresses the `empty_review_history` root default and a bound `sources`
  makes `document_sources` unnecessary. One `af/TaskInputBindings@1` artifact records the
  binding from `TaskRevisionV1.provenance.input_artifact_ids`, written only when the Task file
  carried an `inputs` table; `af/TaskRevision@1` is unchanged and a Task without bindings keeps
  its exact revision, plan and `--json` documents. `af task explain` annotates the `IN` line and
  `--tree` adds `BOUND` rows, `af task show` prints one `bound <port> <- …` line and advances
  its `--json` document gains an `input_bindings` field only when a binding exists, which the
  self-optimizer's AF history adapter admits without changing how accounting is read. `af task
  deliver` reads either origin generation and refuses a re-rooted source before the prepared
  record and any Git mutation, naming the referenced Task and port or the artifact ID: a derived
  tree has no commit for the target's `HEAD` to equal. ADR-0031's exact comparison is unchanged,
  and no acceptance, verification, plan approval, delivery or budget authority crosses a Task
  boundary. One limit is recorded rather than worked around: a bound `history` feeds a Pipeline
  that *reads* the ledger, not one whose Review would continue the predecessor's Round, because
  restoring a Round recomputes it and that recomputation requires every reviewer result to
  retain the current Task's `af/Requirements@1` — a rule this change does not relax.
  `docs/task-execution/task-inputs.md` says so and the fixture is built that way. Seven review
  findings were then repaired in place, each with the regression its reviewer asked for: a bound
  `source` is admitted only when the referenced envelope's payload, its `subject_snapshot_id` and
  the recorded port name one Snapshot whose origin belongs to the tree it describes; a refreshed
  revision keeps the `af/TaskInputBindings@1` record while replacing only its requirements
  artifact, and the Store's refresh validator derives the same expected list; selection preserves
  that record alone, so a Task with other non-port provenance and no `inputs` table keeps
  byte-identical revision, plan and inspection documents; the Claude usage probe observes its
  leader's exit without reaping and ends the process group before waiting, so a same-group
  descendant cannot outlive it; every untrusted label a refusal echoes — destination port,
  Task ID, output port, artifact spelling and the bindable-port reason — goes through the
  preview's display sanitizer; the `af/task-inspection@11` schema gains the optional `input_bindings` property. A command Worker declaring the same effect runs under the process-group-killing exit policy too, so a background child it starts cannot outlive its Attempt (`crates/review-runner/tests/task_command_process_group.rs`).

## [0.9.0-rc.6] - 2026-09-21

### Upgrading from 0.x

- GA reads only what GA writes
  ([ADR-0113](docs/adr/0113-ga-reads-only-what-ga-writes.md)). Review Campaigns and Tasks that a
  0.x release wrote are not supported (they may be refused or misread): that covers everything
  under `$XDG_STATE_HOME/af/review/` and `$XDG_STATE_HOME/af/task/`, and any directory passed with
  `--state` or `--state-root`. Before upgrading, finish or abandon in-flight Campaigns and Tasks
  with the release that started them, then delete that state. Committed `.af/` files are read only
  in the shapes this release accepts. A key or shorthand that only an earlier release wrote is
  refused: delete the refused key by hand, because `af onboard --refresh-lock` cannot repair a
  file it cannot parse.
- Removed `af review tui`. It read Worker pins only from the lock's legacy `[reviewers]` table,
  so it failed on every lock this release writes. The subcommand is now a usage error, and the
  release no longer ships its `af-review-tui.1` man page.
- Removed `af onboard --migrate` and the `.review/` to `.af/` conversion; the retired `.review/`
  layout is not read at all. The flag is now a usage error. `af onboard` on a repository that
  still carries `.review/` scaffolds `.af/` as for any other repository, a `.review/…` pipeline
  path gets the generic "must live under `.af/pipelines/`" error, and Campaigns whose manifests
  pinned `.review/` paths (af 0.7 and earlier) can no longer be resumed, reported or compiled into
  a Task.
- `.af/af.lock` no longer has a `[reviewers]` table or the 0.7.1 top-level `af_version` key, and
  `.af/af.toml` no longer has `[worker.*]` tables: this release refuses a file that still carries
  them, so delete those lines by hand; `af onboard` no longer writes `[reviewers]` or
  `[worker.*]`. Worker pins live under `[workers]` and the release pin under `[af]`, as before.
  A Worker package's `reviewer.toml` must now declare `subjects`; an omitted list no longer means
  whole-tree only.
- `af review run|plan|render`, the `af review` shorthand and `af provider doctor` no longer accept
  `--authority REV` or `--light`; both are usage errors now. Write `--policy-rev REV`, adding
  `--base REV` for a diff pipeline (a whole-tree pipeline still refuses `--base`), and drop
  `--light`, which only restated the default. The `af/review-plan@1` document no longer carries
  `selectors.compatibility_authority`, and the text plan drops its `compat` line.
- `af provider setup` no longer starts an official Provider CLI login on its own — add
  `--login`, and run it at an interactive terminal — and `af provider status` no longer probes
  subscription and quota windows unless asked with `--usage`. Both print the command or flag to
  use, so an existing habit fails loudly rather than silently. This is the machine-local
  `af provider` surface, not repository authority: the machine-local registry stays version 1,
  and its transaction, lock, publication and recovery protocol is unchanged.
- Default Campaign state resolves only the opaque `c-<id>` directory under
  `$XDG_STATE_HOME/af/review/campaigns/`; a directory there named by the label (the layout af 0.4
  and earlier wrote) is no longer a fallback. `af review campaigns|gc --state-root` still list an
  explicit `--state` directory named by its label. A Campaign you placed under that root yourself
  with `--state …/campaigns/<label>` must keep being addressed with `--state`: omitting it starts a
  new `c-<id>` Campaign, and `af review campaigns|gc` then refuse the root because one label holds
  state under both names.
- Removed `af help trust` and its `af-trust.7` man page, which described an `af trust` command that
  never shipped.
- `af self` and `install.sh` no longer install, activate or dispatch to releases older than 0.8.0,
  the first release with a signed `SHA256SUMS`: `af self install 0.7.x` (or a 0.8.0 release
  candidate) is refused, and a project whose lock pins one runs the current `af` instead, with a
  warning. A binary that embeds the release key, and `install.sh` with `minisign` on PATH, now
  refuse any release whose `SHA256SUMS` is unsigned, instead of accepting a pre-0.8.0 release on its
  checksums alone.
- The pre-rename `~/.config/afactory/` directory is no longer read, and nothing warns about it:
  move `providers.toml` and `caches.toml` from there to `~/.config/af/` (or
  `$XDG_CONFIG_HOME/af/`), or `af` finds no provider registry and no cache policy. Setting
  `AFACTORY_CACHE_POLICY_FILE` is no longer an error; it is ignored, so use
  `AF_CACHE_POLICY_FILE`.
- `af task list`, `af task show` and `af task deliver` read only the common `events.sqlite` Task
  store. Implementation Tasks that af 0.8.x and earlier kept in `tasks.sqlite` no longer appear,
  `af task show` no longer emits `af/task-inspection@1`, and delivery no longer knows the
  `refs/afactory/deliveries/<task>` ownership ref. In `af/task-inspection` and `af/task-list`
  output, every delivery preparation and receipt now carries `result_id` and every receipt carries
  `ignored_paths`, as this release always wrote them; the published schema requires both, and a
  Task whose stored receipt lacks one is refused.
- `af task start` now requires `--file`: `--kind implement --goal …` and `--pipeline` are usage
  errors. The fixed implementation v1 format they read, `.af/pipelines/implement.toml` with its
  `.af/workers/` implementer and evaluator packages, is no longer read, and `.af/af.toml` no longer
  accepts `defaults.task_pipeline`: every `af` command refuses a project file that still sets it,
  so delete that line by hand. The `implement` pipeline, its two Worker packages and their pins in
  `.af/af.lock` are then unused and can go too. Run implementation Tasks from a Task file against
  a Task catalog instead; `af catalog init --profile software --destination <new-dir>` creates a
  new starter directory with a runnable catalog and Task files. The `make pilot-check` target is
  gone; `make check` runs the same delivery and recovery tests.
- A Task catalog Worker's `worker.toml` can no longer declare
  `runner.kind = "legacy_task_command"` with its `protocol` and `legacy_budget_tokens` keys: the
  catalog refuses such a package. That runner spoke the fixed implementation v1 Markdown and
  verdict protocol. Declare a `command` or `model` runner instead, which reads
  `af.worker-request/1` and replies with `af.worker-reply/1`. Worker context is always
  `af/TaskContext@1`; `af/TaskContext@2` is neither written nor read.
- Task Review has one generation. A Task catalog whose `[review]` table omits `generation` now
  captures `af.review-task-policy/2`, the same policy as `generation = 2`, instead of generation
  one; any other value is still refused. Reviewer packages must use the generation-two ports: an
  `assignment` input of type `af/TaskReviewAssignment@1`, a `subject` of type
  `af/TaskReviewSubject@2`, and a `review.kernel/ReviewerResult@2` result that lists
  `dispositions` (one per assigned prior Finding) instead of `disputes`. The Review pipeline wires
  each reviewer's `assignment` from `review-bind`. A package with the old
  `af/TaskReviewSubject@1` or `review.kernel/ReviewerResult@1` ports, or without an assignment, is
  refused at planning, so update it together with its pin. The `af/TaskReviewSubject@1` contract
  and its `task-review-subject-v1.json` schema are gone.
- Task execution records have one encoding per record kind: the combined `prepared` record is
  gone, and an Attempt's context is bound through separate `reserved` and `context_bound`
  records. Settlements and usage observations carry decimal-string charges.
  `af/TaskTransition` drops `revision_recorded`, which no release wrote, and
  requires `revocation_id` on `approval_revoked`, which this release always writes.
- `af self optimize` history sources: the `af` adapter reads only the `af/task-inspection`
  receipts that `af task show --json` prints and refuses any other line, including the
  `af.task-event/1` event export that no af command produced. The `af`, `codex` and `claude`
  adapters no longer read normalized records (receipted as `legacy-normalized-v1`): `af` refuses
  such a line, and `codex` and `claude` take no observation from it. Label such a source
  `adapter = "external"` and give it a new `source_id`, because a retained source cannot change
  adapter. Report-only and `--experiment` requests now derive their `optimize-…` Task ID
  the same way light requests do, so re-running one whose capture an earlier release took starts a
  new Task.
- `af/TaskRuntimeEvidence@1` cache observations no longer carry `layer` and `result`, which were
  always `dependency_preparation` and `prepared`, and a runtime span's `kind` is `check` or
  `dependency_preparation` only. `task-runtime-evidence-v1.json`, and the `af/task-inspection`
  schemas that embed it, are narrowed to match, so runtime evidence an earlier release recorded no
  longer decodes.
- Self-optimizer contracts are narrowed in place. `af/OptimizationEconomics@1` drops
  `cache_results`, a per-kind `hit`/`miss`/`unknown` map that duplicated `cache_economics`: read
  `cache_economics.<kind>.hits`, `misses` and `unknown_results` instead.
  `af/OptimizationResult@1` and `af/OptimizationReport@1` drop the constant
  `live_demonstrations: "pending"` field, and optimize Task requirements no longer carry it; the
  Markdown report replaces its "Milestone gates" section with one plain sentence saying live paid
  demonstrations and adoption observations are still pending. A result `conclusion` is
  `validated`, `rejected` or `recommendation_only` (never `inconclusive` or `no_change`), and an
  `af/OptimizationVerification@1` `profile` is always `candidate`. The `af/ExperimentalSlot@1` and
  `af/ExperimentTrialResult@1` contracts and their `experimental-slot-v1.json` and
  `experiment-trial-result-v1.json` schemas are gone. An experiment arm Worker that declares an
  `execution_configuration` input is now refused like any other unavailable input instead of
  having it silently dropped. Optimizer artifacts an earlier release stored may no longer decode,
  and native observations that older captures stored under source-dependent IDs are no longer
  merged, so replaying such a history can count them twice.
- Published schemas are narrowed. `campaign-manifest-v1.json` now requires
  `check_timeout_seconds` and `git_timeout_seconds`, which every current Campaign manifest already
  carries; a manifest from a release that predates them no longer loads. The schemas also drop
  values that no release ever wrote. A dirty `SourceSnapshot@1` capture `boundary` is always
  `revalidated` (`filesystem_snapshot` is gone), and a Gate Execution Binding's
  `provided_isolation` in `run-report-v6.json` and `run-event-v1.json` is `none` or `container`
  (`process` is gone). In `task-contracts-v1.json`, and every schema that
  embeds its Task phase or result, a Task phase is never `resolving`, `planning` or `verifying`, a
  Task result's `execution` is `completed`, `incomplete` or `exhausted` (never `blocked` or
  `cancelled`), and the unreferenced `reviewConclusion` definition is gone.
- A Codex Task Worker's reply is read only from the `-o` last-message file that `codex exec`
  writes (codex-cli 0.147.0 always writes it). The Task adapter no longer falls back to the last
  `agent_message` event on stdout, so with a codex CLI that does not write that file the Attempt
  fails with "Codex Worker returned no final message"; the usage it reported is still charged.
- Source Manifests have one path spelling. The `path_encoding` field (`legacy_v1` or
  `percent_v2`) is gone: every path is spelled the way capture already spelled new trees, so a
  path that starts or ends with whitespace, or holds a space together with a `%` or non-UTF-8
  bytes, is percent-escaped (a leading space, as in `" notes.md"`, is percent-escaped to
  `%20notes.md`, and `a%b c` becomes `a%25b%20c`). That now
  includes a file a reviewer or Worker creates during a run, which a sandbox seal, warm workspace
  scan or Task delivery spelled literally when the baseline was an ordinary tree. A Snapshot's
  content digest hashes the stored spelling, so ordinary trees keep their digests, but a tree
  with such a path gets a different Snapshot digest than an earlier release gave it. ADR-0024 is
  superseded by ADR-0113 and removed.
- `af` builds and runs on Linux and macOS only. A source build for any other host, including
  Windows and the BSDs, now stops with a compile error. It no longer compiles fallbacks that
  skipped read-only sandboxes, process-group kills, symlinks or executable bits. The release
  targets and `install.sh` are unchanged.
- `af review report` no longer has a `spend` section: the JSON of `af/review-report@4` drops the
  `spend` array (the schema no longer lists it), text output drops its `Spend:` block, and
  Markdown drops its `## Spend` table
  and `### Attempts` list. They described only Attempts of the pre-Task executor, so for a Round
  a Task hosts they were empty or held a zero-token placeholder row; `task_accounting` reports
  those Rounds' Attempts, usage, wall-clock and caps. `RunReport@1` and `RunReport@2` events
  are neither written nor read, so a Campaign whose log holds one can no longer be run, reported
  or listed. The unused `run-report-v2.json` and `review-report-v2.json` schemas are gone.
- The retired shell review harness is gone from the repository: `compat/legacy-harness/`, the
  `fixtures/synthetic/` corpus generated from it, and the `fixtures/legacy/` private-corpus
  tests. `make fixtures` and `make review-kernel-test-corpus` no longer exist, and `make check`
  no longer regenerates the corpus. A Campaign event log that holds an artifact-less
  `FindingReported@1` (the `"imported": true` shape that only the unused `ledger.jsonl` importer
  wrote) no longer replays, and `af review ledger`, `af review show` and `af review report` no
  longer print an "unavailable: legacy import" placeholder.
- `af review run` and `af provider doctor` run every Campaign on the common Task runtime; the
  pre-Task executor they fell back to is gone. A Campaign whose log holds events only that
  executor wrote (`RunReport@3` to `@5`, Cold Closeout or Session Snapshot events, from Rounds run
  by af 0.9.0-rc.0 or earlier) is refused with "Campaign predates the common Task runtime
  (af < 0.9); start a new Campaign"; one that holds its reviewer Attempt, Provider Operation or
  broker events no longer replays at all (see below). A Campaign whose first Task capture failed
  now retries capture on the common runtime, including after `--restart-round`,
  `af review policy-time advance`, `af review evidence add` or `af review demand waive`, where it
  used to run on the pre-Task executor. `--resume-provider` is gone and is now a usage error
  (exit 2); it only continued that executor's fenced Provider Operations, and the common path
  already refused it. The `af/review-outcome@1` and `af/provider-doctor@1` documents, which only
  that executor printed, are no longer produced: Providers are admitted by the Review Task's own
  probe Attempts, and doctor prints `af/provider-doctor@2`. `af review run` no longer requires
  `HOME` up front. `af review report`, `ledger` and `campaigns` count only Task Attempts toward a
  Campaign's wall-clock, so a pre-Task Campaign's report no longer shows one. ADR-0016 is
  superseded by ADR-0113 and removed. A `--restart-round` before the Task exists now keeps
  Round 1's original prior Finding Set even when the candidate changed, so the Task captured on
  the new epoch resumes; each later run used to fail with "restarted Review changed its original
  prior sets or adjacent epoch".
- The pre-Task executor's Campaign event types are gone from the event vocabulary and from
  `run-event-v1.json`: `AttemptAdmitted@1`, `AttemptDispatched@1`, `AttemptFailed@1`,
  `AttemptFenced@1`, `AttemptFeedback@1`, `AttemptInput@1`, `AttemptReleased@1`,
  `ReviewerExecutionBound@1`, `BrokerOperationCompleted@1` and `ProviderOperationTransition@1`,
  with the `provider-operation-transition-v1.json` schema and the `review.kernel/RefusalHistory@1`
  artifact type. No current command wrote them. A Campaign log that holds one fails to replay with
  "unknown review-kernel event type: <type>; this log was written by another af release; start a
  new Campaign or Task", so `af review run`, `report`, `ledger` and `show` fail on it, and
  `af review campaigns` lists it as a problem. Every event log that holds an event type this
  release does not know fails with the same message. `af review report` no longer carries the
  optional `recorded_not_gathered` field, or prints its "Recorded, not gathered" section, which
  only such events filled; the field is gone from `review-report-v4.json`.
  `af review run` still lists recorded, not gathered results from the Round's Task Attempts. The
  `af review ledger` notice for an absent latest-Round Ledger drops its
  "(N admitted result(s) remain recorded, not gathered)" clause, which always counted 0.
  ADR-0022 and ADR-0023 are superseded by ADR-0113 and removed.
- `RunReport@3`, `@4` and `@5`, the run conclusions only the pre-Task executor wrote, are gone
  from the event vocabulary and `run-event-v1.json`, with the `run-report-v3.json`, `-v4.json`
  and `-v5.json` schemas. `RunReport@6` is the only run conclusion. `run-report-v6.json` now
  defines its outcome, verdict, binding and cache shapes itself, and
  `task-review-gate-facts-v1.json` takes its Cache failure shape from it. A Campaign log that
  holds a retired report, `RunReport@1` to `@5`, no longer replays. When that report is the
  first record replay cannot read, `af review run` and `af provider doctor` refuse the Campaign
  with "Campaign predates the common Task runtime (af < 0.9); start a new Campaign"; when a
  retired Attempt, Provider Operation or broker event comes first, they print the unknown event
  type message above. `af review report`, `ledger` and `show` fail on it, `af review campaigns`
  lists it as a problem, and a new event for the Round such a report concluded is refused with
  the unknown event type message. The Round rows of `af review report` drop `reported_tokens`,
  which only those reports' plain numeric spend filled. Every row now carries
  `task_chargeable_tokens_at_report` and `task_accounting`, which `review-report-v4.json`
  requires.
- The `gate_blocked` suppression reason is gone; only the pre-Task executor's scheduler wrote it.
  A Review Gate is a Task condition, so a node behind a Gate that did not pass reads
  `branch_not_selected` in `af/TaskRunReport@2` and in `af/review-outcome@3` node
  outcomes, or `upstream_missing` once its predecessors were suppressed, and `RunReport@6`
  records both as `upstream_missing`, as before. `task-run-report-v2.json`, `run-report-v6.json`
  and `review-outcome-v3.json` no longer list `gate_blocked`, and the
  review-outcome `ledger_production` no longer lists `not_produced_gate_blocked`. A stored report
  that carries `gate_blocked` no longer decodes.
- A Campaign manifest records only the canonical `report-derived@1` Finding identity policy, and
  `campaign-manifest-v1.json` no longer lists `legacy-path-title@1`. A Campaign whose manifest
  pins that path/title policy (opened before path-independent Finding identity, ADR-0006) can no
  longer be run or continued: its manifest is refused for an unknown finding identity policy, and
  its Ledger reports the manifest as unavailable authority. The Ledger reads a Report only as an
  enveloped `FindingReport@1` whose locations are canonical repository paths. An un-enveloped
  Report, the flat pre-`FindingReport@1` shape, or a Report with a noncanonical location such as
  `./src/a.rs` now projects as an unreadable-authority placeholder that blocks convergence; a
  noncanonical location used to leave the claim readable with unknown Scope. A Ledger node's
  `FindingSet@1` output must be an envelope: the untyped `{round, sources, findings}` summary is
  refused.
- `af review report --json` Finding objects no longer carry `news_round`, a Round counter no
  decision read; convergence counts news by `scoped_news_round`, as before. A Finding view's ID
  hashes the view, so view IDs differ from the ones an earlier release computed for the same
  Finding. A `FindingReport@1` relation only `corroborates` a `finding`: the `disputes` kind and
  the `report` target, which no release wrote, are gone from `finding-report-v1.json`, and a
  Report that uses them no longer decodes.
- Review pipeline format 1 is gone. A pipeline that declares `version = 1`, the format without
  `[subject]` whose untyped `findings`, `prior_findings` and `change_set` ports were typed by their
  names, is refused as an unsupported version; formats 2 through 5 still load (see the typed-port
  entry below). Declare
  `version = 2` with `[subject]` and typed `FindingSet@1` or `ChangeSet@1` ports instead. A
  Campaign whose manifest pinned a format 1 pipeline can no longer be resumed or continued.
- Campaign review has one reviewer contract, `review.kernel/ReviewerResult@2`. A pipeline is
  refused when it loads if a reviewer declares `review.kernel/ReviewerResult@1` or an untyped
  result output such as `outputs = ["result"]`, or if its Generation emits
  `review.kernel/PriorFindings@1`. Every reviewer and every Scatter must declare one optional,
  singular `review.kernel/FindingSet@1` input with snapshot affinity `any`, wired from
  Generation's `FindingSet@1` output. A reviewer without it used to run as `ReviewerResult@1`,
  and a Scatter without it fell back to `@1` silently; both are now refused at plan time.
  Reviewers answer `dispositions`, one per assigned prior Finding (`corroborate`,
  `not_reproduced` or `dispute`, keyed by `finding_id`), instead of `disputes` keyed by
  `claim_id` with `confirm` or `refute`. `af onboard` and the software starter already write this
  wiring; update a hand-written pipeline and its pin in `.af/af.lock`. The
  `reviewer-result-v1.json` schema and its conformance corpus are gone, and
  `reviewer-result-v2.json` now defines the flat report shape itself. `finding-set-v1.json` accepts
  only `review.kernel/finding-reducer@2` and `task-review-result-metadata-v1.json` only
  `ReviewerResult@2`, so a stored `ReviewerResult@1` result or a Finding Set reduced by
  `finding-reducer@1` no longer loads; a Ledger that reduces no reviewer result now records
  `finding-reducer@2` as well, so that Finding Set's ID differs from an earlier release's. A
  command reviewer's stdin document now always carries
  `"result_contract": "review.kernel/ReviewerResult@2"`.
- `review.kernel/ReviewerResult@2` no longer has the reviewer's `verdict` and `summary`, which no
  decision, report or display read. A result is exactly `reports`, `benchmark_demands` and
  `dispositions`, and the model output contract no longer asks for the other two. A model reviewer
  that still sends them is unaffected, because its answer is normalized to the contract. A command
  or Task reviewer Worker whose reply still carries them is refused as
  `unexpected_or_missing_fields`: drop both keys from its output and from the package's
  `outputs/result.schema.json`, then re-pin the package digest. That includes the `bugs` and
  `correctness` Workers of a starter an earlier `af catalog init --profile software` wrote; run
  it again into a new directory to get the current ones.
- Every review pipeline port is a typed table, in every pipeline format. The string shorthand
  (`outputs = ["decision"]`, `inputs = ["reports"]`) is refused when the pipeline is parsed, and so
  is a node without `outputs`, which used to get an implicit `out` port. A Gate, Gather or Ledger
  output typed `review.kernel/Opaque@1`, the type the shorthand stood for, is no longer retyped by
  its node kind: it is refused as an unsupported Review output before the Round's Gate runs, and
  the Store no longer skips payload validation for Opaque@1 artifacts. Spell each port out as
  `{ name = "…", type = "…", cardinality = "one", optional = false, snapshot_affinity = "any" }`:
  a Gate outputs `review.kernel/GateDecision@1`, a Gather `review.kernel/ReportSet@1`, and a
  Ledger exactly one `review.kernel/FindingSet@1`, optionally beside a `review.kernel/DemandSet@1`;
  a Ledger without that one Finding Set output is refused when the pipeline loads, where a lone
  untyped Ledger output used to receive the Finding Set by position. `af onboard` and every
  shipped pipeline already write typed ports. A Campaign whose manifest pinned a pipeline with the
  shorthand can no longer be resumed or continued.
- Brokered credentials are gone; GA has no broker. A format 4 or 5 reviewer's `execution` accepts
  only `credential_mode = "credential_free"` or `"trusted_unsafe"` and `auto_apply`:
  `credential_mode = "brokered"` and an `operations` list are refused when the pipeline is parsed,
  in `af review plan` too. They used to pass `plan` and fail only at `af review run`, because no
  adapter could serve them. No shipped pipeline declares either. Provider admission always
  compiles to `af/TaskProviderAdmission@1` with the `af/TaskProviderContext@1` readiness context.
  The captured `af/LegacyReviewTaskPolicy@4` drops `settings.provider_probes`, which was always
  empty, and holds the review settings directly under `settings`, so a new Review Task's policy
  digest differs from an earlier release's. The `task-provider-admission-v2.json`,
  `task-provider-context-v2.json` and `task-provider-probe-policy-v1.json` schemas are deleted, and
  `compiled-task-v1.json` loses the `provider_admission_brokered` operator.
- The Broker's Task records went with the Broker; no release wrote them, because none ever
  installed a Broker. `TaskBrokerTransition@1` is no longer an event type, so a Task log holding
  one fails to replay. `af task show --json` and `af task explain --json` no longer emit
  `af/task-inspection@4`, a `broker_records` section or a `broker_transition` history entry, and an
  `af self optimize` history source with the `af` adapter refuses an `af/task-inspection@4`
  receipt. The `task-broker-binding-v1.json`, `task-broker-operation-v1.json`,
  `task-broker-transition-v1.json`, `broker-operation-receipt-v2.json` and
  `task-inspection-v4.json` schemas are deleted, and `run-event-v1.json` and
  `task-inspection-v11.json` drop their Broker entries.
- Task inspection has one version. Every command that prints a Task as JSON (`af task show`,
  `explain`, `plan`, `start`, `run`, `approve` and the others, and `af self optimize`) now emits
  `af/task-inspection@11`, instead of a version from `@3` to `@11` chosen by the sections the Task
  happened to have. Each section beyond the core (`owned_child_sets`, `review_handoffs`,
  `review_integrations`, `attempt_walls` with `runtime_observations`, `experiments` and
  `adoption_observations`) appears only when the Task recorded it, and `history` holds
  `TaskTransition@5` payloads. `task-inspection-v11.json` is now self-contained and
  describes all of it; it no longer requires `experiments` and `adoption_observations`, and a
  Failed settlement's `diagnostic` must be a JSON object, as this release always writes it. The
  `task-inspection-v3.json` and `-v5.json` to `-v10.json` schemas are deleted, and
  `task-list-entry-v2.json` and `task-plan-inspection-v1.json` now refer to
  `urn:af:schema:task-inspection:11`. The `af` history adapter of `af self optimize` accepts only
  `af/task-inspection@11` receipts, so a receipt an earlier release exported is refused. A script,
  skill or hub check that matches another `af/task-inspection@N`, or validates against a deleted
  schema, must switch to `@11`.
- Task usage and Review provenance have one encoding each. Every usage artifact is
  `af/TaskTokenUsage@3` and every Task Review provenance artifact is
  `af/TaskReviewAttemptProvenance@2`, whatever the width of their counters; narrow values no
  longer select `af/TaskTokenUsage@1` or `@2` or `af/TaskReviewAttemptProvenance@1`. The
  payload bytes are unchanged, but the artifact types and content IDs of new usage and
  provenance artifacts differ from those an earlier release wrote. A Task Review selection whose
  provenance or usage artifact carries a retired type, or no artifact envelope at all, is
  refused, and the `task-token-usage-v1.json`, `task-token-usage-v2.json` and
  `task-review-attempt-provenance-v1.json` schemas are deleted: every other schema now refers to
  `urn:af:schema:task-token-usage:3` for its decimal counters.
- Task records have one version per name. `TaskTransition@1` to `@5` collapse to
  `TaskTransition@5`, `af/TaskExecutionRecord@1`, `@3`, `@4` and `@5` to
  `af/TaskExecutionRecord@5`, `af/TaskRunReport@1` and `@2` to `af/TaskRunReport@2`, and
  `af/TaskReviewHandoff@1` and `@2` to `af/TaskReviewHandoff@2`. Every change kind, record kind
  and field is kept: the new versions are supersets of the old ones, so a transition now carries
  `review_continued`, `review_integration_selected`, `review_integration_finished`,
  `recording_resumed` or `adoption_observation_recorded` under the same number as `opened` or
  `finished`; a settlement or usage observation carries its decimal-text `charged_tokens`, and
  the owned-child and experiment kinds travel in the same record type. An Integration phase
  report is `af/TaskRunReport@2` with a `phase_id`; a Round report is the same type without one.
  The `task-transition-v1.json` to `-v4.json`, `task-execution-record-v1.json`, `-v3.json` and
  `-v4.json`, `task-run-report-v1.json` and `task-review-handoff-v1.json` schemas are deleted,
  and `run-event-v1.json` and `task-inspection-v11.json` name only the surviving versions. This
  is a wire break: a `.af/state` Task log or CAS record an earlier release wrote no longer
  decodes, and `af task list` and `af self optimize` fail for the whole store while one remains,
  rather than skipping it. Finish or delete in-flight Tasks before upgrading. The
  `execution_records[].artifact_type` and `review_handoffs[].artifact_type` values in
  `af task show --json` each collapse to one string.
- A Task catalog and its captured run authority have one schema each: `af.task-catalog/2` and
  `af.task-run-authority/2`. `provider_admission` is now optional in a catalog; omitting it means
  the fixed 4,096-token, 45-second admission allowance that `af.task-catalog/1` had, and
  declaring it keeps the explicit bounded cost. A committed `.af/task-catalog.toml` that still
  says `schema = "af.task-catalog/1"` is refused with `Task catalog requires schema
  af.task-catalog/2`; change that one line by hand (nothing else about the file changes).
  `af catalog init` now writes `af.task-catalog/2` for every profile. The captured run
  authority always records the resulting admission cost and re-checks it against the captured
  catalog bytes, and `task-catalog-v1.json` is deleted.
- The Attempt wall sidecar in `events.sqlite` keeps one exact usage column, `usage_v3_json`,
  beside `usage_observation_v1_json`; both are created with the `attempt_wall` table. The numeric
  token columns, `usage_v1_json`, `usage_v2_json` and the `ALTER TABLE` migration that ran on
  every open are gone, as is the Campaign-keyed narrowing view only the removed pre-Task executor
  wrote. Wall rows an earlier release recorded in the retired columns are no longer read, so
  finish in-flight Tasks before upgrading: recovery raises an abandoned Attempt's charge from its
  recorded wall usage, and without it the Attempt settles at its reservation floor.
- `af review run --json` always emits `af/review-outcome@3`, instead of `@2` when every selected
  Attempt's usage fitted u64, and `review-outcome-v2.json` is deleted.
- `af review report --json` always emits `af/review-report@4`, instead of `@3` for narrow usage or
  `@1` for a Campaign with no Task. `task_accounting` is always present; it is empty exactly for a
  Campaign whose first Task capture failed, which has no Rounds either, and
  `review-report-v4.json` describes that case. `review-report-v3.json` is deleted. A script, skill
  or hub check that matches `af/review-outcome@2`, `af/review-report@1` or `af/review-report@3`,
  or validates against a deleted schema, must switch to the single version.
- The reviewer output contract a model Worker is prompted with now names its report array
  `reports`, the key the stored `ReviewerResult@2` artifact and every Worker package schema
  already used, instead of `findings`. An answer that still says `findings` is read as `reports`,
  so hand-written command reviewers keep working, but the prompt bytes changed: reviewer Attempt
  context IDs differ from those an earlier release computed for the same node.
- This file starts at the first GA release: the `0.7.0`–`0.9.0-rc.6` sections are out of it and
  stay on the GitHub release pages and in git history. `README.md` and `SECURITY.md` now state
  the GA policy — compatibility obligations start at 1.0, and only the latest `1.x` minor line
  receives security fixes.

### Changes

- Make Provider onboarding safe for automated callers (ADR-0112): refuse to start an official CLI
  login unless the operator opted in with `--login` *and* the process owns an interactive terminal
  on stdin, stdout and stderr, returning the `human_action_required` result with the exact
  private-terminal command instead; warn about OAuth URLs and authorization codes before handing
  over the terminal; keep registering an already-authenticated context with no login at all; add
  stable versioned `af/provider-status@1` and `af/provider-setup@1` documents under `--json` that
  distinguish registration, authentication, usable-or-untested and usage without exposing account
  email, organization identity, credentials, OAuth material or raw Provider output; make
  `af provider status` a fast registry and authentication check with subscription and quota probes
  behind `--usage`, where an unavailable probe exits 7 and leaves an authenticated Provider
  authenticated; document exit codes 3 (human action required), 4 (Provider CLI missing), 5
  (registry conflict), 6 (authentication failed) and 7 (usage unavailable) with results on stdout
  and diagnostics on stderr; and keep `af provider doctor` the charged end-to-end usability check.
- Remove the `task_planning` integration-test timing race: a Task deadline is absolute
  wall-clock from creation, and the generated-plan tests hold one Task open across a long chain
  of `af` invocations, Git commits, catalog operations, signing and Python Workers. Under the
  four-thread full test gate those subprocesses consumed the fixtures' 60s budget, so valid
  generated-plan and imported-catalog resumes were correctly but unhelpfully refused with `Task
  deadline protects still-required verification`. The long-lived Tasks in
  `crates/af/tests/task_planning.rs`, `task_catalog.rs` and the native-model cases in `task_file.rs`
  now use a documented ten-minute wall budget, added to the total and taken from nothing:
  per-Attempt walls, Attempt counts, token budgets and the verification reserve keep their fixture
  values, and no production code changed. New tests pin both halves — only the total fixture wall
  moves, and `TaskBudget::prepare` keeps its exact deadline boundary and refusal wording at both a
  small and a large budget
  ([ADR-0114](docs/adr/0114-budget-cli-task-fixtures-for-loaded-machines.md)).
- The internal delivery records are out of `docs/`: the `P00`–`P14` package checklist, the product
  backlog, the release-timing and validation-cost measurement records, the self-optimizer plan and
  review record, and the pre-implementation design notes the shipped architecture was ported from
  (`docs/design/{overview,entities,state-machines,config,store,research,task-execution,task-execution-examples}.md`).
  Git history keeps them. The engineering values are now [`docs/values.md`](docs/values.md), and
  `docs/design/` keeps only the two designs still in flight, warm layers and the self-optimizer.
  `CONTRIBUTING.md` now documents the opt-in `make check TEST_RUNNER=nextest` runner.
