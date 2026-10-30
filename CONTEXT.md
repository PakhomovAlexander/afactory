# Afactory — shared language

The vocabulary of Afactory's Review Kernel and the `af review` command behind it. These words are
load-bearing: the kernel's guarantees are stated in them, and several of its invariants are
only expressible because two nearby concepts are kept apart. The mechanism itself is
documented in [`docs/architecture.md`](docs/architecture.md); this file defines only the terms.

## Language

### Task execution

**Task**:
The durable business request, with immutable revisions, typed inputs and required outputs,
acceptance obligations, captured authority and bounded resources. Review is one Task kind.

**Observation**:
A normalized, source-addressed fact about project execution, intervention, cache behavior or
outcome. It carries no instruction or causal authority, and absent measurements remain unknown.

**History Capture**:
One immutable, project-scoped set of Observation records plus exact source-prefix receipts,
exclusions, cutoff and completeness. Appended captures retain predecessor identity; overlap is
deduplicated by exact source/observation identity, never prose similarity.

**Optimization Economics**:
The deterministic retained-chain projection that keeps native counters, AF chargeable usage,
outer-session usage, overlapping time and optional estimates distinct. It is evidence for a
report, not a provider invoice or a verified savings claim.

**Execution Plan**:
The exact compiled Pipeline closure for one Task revision, including expanded child calls,
effective Workers, input identities, allowed effects, acceptance coverage and resource limits.
A Pipeline is the reusable public input/output definition from which a plan is compiled.

**Preparation Plan**:
An engine-installed fixed plan that runs the captured Planner Worker on the same Task ledger.
It protects future verification reserves and cannot satisfy business acceptance.

**Pipeline Proposal**:
A Planner's bounded generated Pipeline TOML. Compiler admission and durable selection establish
its provenance; the proposal itself grants no execution or developer authority.

**Developer Plan Decision**:
An authenticated approval or rejection of an exact Task revision, Execution Plan and captured
policy. Generated dependencies require approval before dispatch. A model response, writer label
or unsigned decision object cannot supply this authority.

**Shared Task Catalog**:
A Git-pinned set of Pipeline, Worker and Task-kind packages. Export uses typed public inputs as
parameters, shared defaults and separate contract fixtures; it carries no Task execution approval.

**Catalog Contract Fixture**:
An exact expectation for public interfaces, applicability, coverage and bounds. Passing it checks
package compatibility; independent Task acceptance still requires execution.

**Document**:
A rendered data artifact bound to an exact structured draft and captured sources. Document
acceptance requires current source/content checks and an independent evaluation; it carries no
code Snapshot identity. Captured source text supplies requirements and evidence, never execution
authority.

**Report Task**:
A Task of the installed report profile: a Document written against one exact source Snapshot,
whose repository citations the kernel resolves against that Snapshot's Manifest and whose
acceptance an independent verifier on the same Snapshot decides. Every one of its receipts names
that Snapshot; it writes no source and is never delivered.
_Avoid_: **Document** for it — a Document Task carries no Snapshot identity and its author
cannot read the repository. And **Report** — a Report is one reviewer's immutable claim in a
Campaign; a Report Task's output is a Document.

**Human presentation**:
The readable Markdown `af task output --format human-markdown` derives from a Document's exact
draft and sources at export time, with the Task's acceptance and the matching independent review
in plain words. It is never recorded or reviewed; the Document's canonical text stays the
accepted bytes ([ADR-0151](docs/adr/0151-export-a-human-presentation-of-a-report-beside-its-canonical-markdown.md)).
_Avoid_: calling it the report or the Document, or calling it verified.

**Measurement**:
The kernel's own record of one declared command run a declared number of times against one exact
Snapshot: every repetition's elapsed time, exit status, output digests and the metrics the
command reported in declared units, or the reason it failed. A model never writes one.
_Avoid_: **Evidence** — Evidence is linked to a Demand or a resolution, and the kernel checks
only that it exists and is linked; a Measurement is what the kernel itself observed, and it
satisfies no Demand by being recorded. Nor "the benchmark" a Worker reported in prose.

**Comparison**:
The deterministic fold of a baseline and a candidate Measurement of the same measure under one
declared objective, in exact decimal arithmetic, concluding `improved`, `below_threshold`,
`unchanged`, `regressed` or `inconclusive` per metric. It passes only for `improved` on the
objective's metric.
_Avoid_: **Demand** — a Demand is an obligation to measure a claim; a Comparison is a recorded
result, and neither creates nor discharges the other. Nor the evaluator's verdict, which reads a
Comparison and cannot change it.

**Remote Check**:
A declared code check that a Task pipeline's check node lists in `remote_checks`, so the kernel
hands it to the check's declared remote executor instead of running its command: the declared
workflow's `pull_request` run on GitHub Actions for a Gate pull request. It passes only when
every required job of that run's latest attempt succeeded and the kernel read back that GitHub
tested exactly the checked Snapshot's tree; every refusal or inconclusive state is `not_run`
with a named reason. Its record is one `af/RemoteCheckEvidence@1`, and its plan carries the
effect `publish-gate` and the destination `github:<owner/name>`
([ADR-0140](docs/adr/0140-run-a-declared-check-through-a-gate-pull-request.md)).
_Avoid_: "green CI" — a CI conclusion nobody bound to the Snapshot is not a Remote Check. Nor a
per-machine switch: the code policy declares what a check's remote form is, the pipeline
chooses where it runs, and a project that wants both gates keeps two pipeline variants.

**Remote Check mapping**:
The operator's machine-local file (`$XDG_CONFIG_HOME/af/remote-checks.toml` or
`AF_TASK_REMOTE_CHECK_POLICY_FILE`) that names, per repository, where this machine may push gate
branches and, optionally, the prose-only `no_perf_paths` that earn a Gate pull request the
`no-perf` label, and nothing else. Having a target is the authorization to push: a pipeline with remote
checks cannot be planned without one, and the target is read again before the check Attempt
pushes. It selects no check, and a file that still carries RC1's `checks` key is refused. No
record ever holds its path or push URL.
_Avoid_: putting it in `.af/` — authority to act on a remote belongs to a person at a machine.

**Gate pull request**:
The one draft pull request from `af-gate/<task-id>/head` to `af-gate/<task-id>/base` that a
Remote Check opens, both branches being commits the kernel built from the Task's Snapshots and
owned by that Task in its Store. Resume and repair rounds attach to it; it is not for review or
merge. When the Task finishes, af closes it and deletes both branches, each only while it still
equals the Task's recorded evidence, recording a `gate_cleanup` that never changes the Task's
result; the Task is not collected before one is done
([ADR-0144](docs/adr/0144-hold-afs-disk-use-to-a-machine-budget.md)). Its one label af changes
is `no-perf`: set when the whole source-to-candidate change lies in the mapping's
`no_perf_paths` and the repository already has the label, removed before a head update that
no longer qualifies
([ADR-0196](docs/adr/0196-skip-pull-request-measurements-only-for-an-explicit-no-perf-label.md)).
_Avoid_: the delivery pull request — delivery still never pushes or opens one.

**Storage Budget**:
The one machine-wide bound (`[storage] max_bytes`, machine layers only) on every byte af keeps
between runs: warm toolchain keys, warm Workspaces, review campaign Stores, finished Tasks of
every Task Store, Stores this release cannot read, and installed versions. Each is an entry af
evicts whole, least recently used first, never while in use or within an hour of its last use; a
finished Task goes through collection and keeps its tombstone. Its free-disk floor
(`min_free_bytes`) refuses a check, a measurement repetition or a Worker Attempt with
`insufficient_disk` before any spend, also when a volume af works on cannot be measured. The
sweep ending `af task run` is a `storage_sweep` observation of that Task. `af storage` shows it
([ADR-0144](docs/adr/0144-hold-afs-disk-use-to-a-machine-budget.md)).
_Avoid_: "cache size" — the Warm Check Cache's `max_bytes` bounds one key; the Storage Budget
bounds everything af keeps, how many keys included.

**Store registry**:
`$XDG_STATE_HOME/af/stores.toml`: every Task or review Store af opened or created outside its
default roots (`--state`, `--state-root`), and every Project Store, recorded best effort so the
Storage Budget's sweep reaches it, and dropped once its path holds no Store.

**Project**:
The durable identity of one repository across every clone, worktree and fork af has seen:
identified by its `repository_id` (the sorted root commits every Snapshot origin already
carries), labelled by `[project] name` in `.af/af.toml`, and owning one Project Store under
`$XDG_STATE_HOME/af/project/<id>/` that indexes its Workspaces and holds its Surveys.
_Avoid_: the `.af/` directory, which is a declaration the Project's tree carries, not the
entity; a checkout path, which is a Workspace; a Snapshot, which is one tree the Project had.

**Workspace**:
One registered checkout of a Project for one line of work: a worktree or clone, its Task Store,
its Task files and its bindings file, under one label. Every default Task Store af has ever
created is the Store of an unlabelled Workspace, adopted as such.
_Avoid_: **Warm Workspace**, a per-node template root inside one Campaign that is never a
checkout; and "workstream" as a second entity — the Workspace's label *is* the workstream's
name, and the owner's `workstreams/<name>/` directories are Workspaces made by hand.

**Survey**:
One immutable, kernel-measured description of a Project at one exact head, `af/ProjectSurvey@2`
in its Project Store: identity, tree, history, contributors, decisions, vocabulary and checks,
every number decimal text, each section bounded by the `[survey]` policy and marked complete,
partial or unavailable with the bound it reached. A model never writes one, it never holds an
author email, and a complete one is reused, nothing recorded, while its survey key (head
commit, refs digest, policy digest, Surveyor version) is unchanged and no `--force` asks for
another.
_Avoid_: **Measurement**, one declared command run against one Snapshot; **History Capture**,
the optimizer's economics observations; **Report**, one reviewer's claim; and the README.

**Surveyor**:
The kernel-installed, token-free operator that records Surveys: synchronously at
`af onboard --apply` and on `af project refresh`, reading the head only through supervised git
plumbing under one `max_wall_ms` deadline, and never while the survey key is unchanged.
_Avoid_: a daemon — it is one call that reads, writes one record and returns; and a Worker — it
has no Provider, no Attempt and no budget.

**Project Brief**:
The bounded, named selection of Survey sections, `af/ProjectBrief@1`, that a pipeline's declared
`project_brief` root port carries into the Workers it is wired to and no other: a
`[brief.<name>]` variant of project policy (built in: `orient` and `none`) rendered
deterministically from the Survey the Task's initial capture pinned, whole sections dropped
last-declared first until it fits `max_bytes`, pinned by artifact ID in the Execution Plan and
listed in the Attempt's context manifest. Its staleness is the consuming Task's receipt, never a
field of the Brief.
_Avoid_: the Survey itself, which never enters a prompt; ambient context; a prose summary a
model wrote.

**Telemetry**:
What af records about its own work — spans, events and, at the `content` level, redacted
content blobs — in per-process segments under the state root's `telemetry/`, never in a Store.
The kernel's execution authority never reads it and nothing it captures is a Worker input; its
level is the machine-only `[telemetry] level` (`off`, `metrics`, `spans`, `content`). A
**Telemetry Run** is the work one kernel run id covers inside one Store, its trace namespaced by
the Store directory; an **execution** is one process's part of it, identified by its segment
([ADR-0153](docs/adr/0153-record-telemetry-in-its-own-store-and-never-let-it-steer.md)).
_Avoid_: "logs" or "the Ledger" — Telemetry is evidence of nothing, and a run id alone does not
name a Telemetry Run.

**Check runtime**:
The directory `af-check-<pid>-<random>` af creates for one check and removes after it, holding
the check's `HOME`, `TMPDIR`, `AF_CHECK_SCRATCH` and `XDG_CACHE_HOME`. Every check gets one —
Task, measurement, review gate and integration checks alike — and a check writes nothing it
needs later anywhere else.

**Collected Task**:
A finished Task whose log ends in the `af/TaskCollected@1` tombstone `af task gc --apply` wrote:
its events stay, its projection stops at the tombstone, and it keeps only the summary the
tombstone retains. Its CAS objects are removed unless an uncollected Task or a Campaign record
still reaches them; its outputs can no longer be written or delivered.
_Avoid_: "deleted Task" — no event is ever deleted, and the Task ID stays taken.

**Implementation acceptance**:
The conjunction of independent verification against exact Task Requirements and the configured
Review or repair guarantee on the final Snapshot. Passing Review cannot cover a missing or
negative requirements verdict; every verifier retains the requirements it consumed.

**Task Review Continuation**:
Exact independently verified repair evidence projected into the next admitted discovery Round.
It retains the original Round and claim identities, grants no complete-Review coverage and does
not create Campaign Resolution authority. Later discovery can reopen a previously fixed claim.

**Task continuation**:
A new Task planned from a finished reviewed one (`af task run NEW --from TASK`, or a Task file's
`continue`): its source is the candidate a review round of TASK reviewed, and its implementer
receives that round's findings as one typed `af/TaskReviewFindings@1` input. It records the lineage and
carries no acceptance, verification or budget of the Task it continues (ADR-0212).
_Avoid_: **Task Review Continuation** — that is repair evidence inside one Review; "follow-up
Task" with the findings pasted into its goal. A Task that recorded no review round continues from
its **Partial candidate** instead, without findings (ADR-0257).

**Partial candidate**:
The tree a source-writing Worker left when its Attempt wall ended it, sealed exactly as a
candidate is and kept as `af/TaskPartialCandidate@1`, named by the Attempt's
`af/TaskAttemptWallEnd@1` with the wall and the time the Attempt took. Raw evidence of a failed
Attempt, never an output: it is never sealed into a result, checked, reviewed, accepted or
delivered on its own; a **Task continuation** binds it as the new Task's source (ADR-0257).
_Avoid_: "timed-out candidate", "malformed reply" for an Attempt its wall ended.

**Base sibling**:
The tree before the change, materialized read-only beside a judging Worker's own tree at
`../base`. Context for a reviewer, evaluator or verifier, never a port and never sealed back
(ADR-0212).

**Diff Subject**:
The change a judging Worker judges: one kernel-built `af/DiffSubject@1` artifact whose
`af/diff-subject@1` document lists each path that differs between the Base sibling and the
Worker's source, with its digest on each side. It reaches the Worker as a typed input on the
optional `diff_subject` port its contract declares, which no Pipeline binds, and is checked before
dispatch (ADR-0254).
_Avoid_: `../diff-subject.json` — that file is no longer written.

**Captured Issue Source**:
An exact read-only observation of ticket fields, with raw source, per-field value and normalized
text identities. A source revision label alone is not immutable identity. It supplies Task
requirements and never execution authority; resume reads captured input without fetching again.
An explicit source refresh creates a new Task revision when selected fields change, invalidates
its affected plan approval and retains the original execution allowance and all prior spend.

### Performance laboratory

**Performance Executor**:
The operator controller that selects independent local VM compute or GitHub Actions from a
closed configuration. Local compute calls the existing laboratory with installed committed
control, pinned containers and one machine lease; Actions dispatch preserves hosted identities.
Every local result is advisory and carries its actual backend and persistent environment.

**Local Experiment Original**:
One atomically reserved request directory, with frozen sources/configuration and a durable
claim before launch. Machine busy before that claim may wait; a claimed original is never
executed again, including after a crash or ambiguous handoff. Its locally retained source
objects, plan, samples and terminal receipts support offline admission without Actions IDs.
A local calibration cohort declares this journal and producer before its finite originals.

**Machine Experiment Fence**:
The durable sibling of the fixed performance machine lock, created before shared work and
settled only after confirmed cleanup and terminal evidence. Controller loss or uncertain owned
descendants retain it across users/workspaces. Every compute process sees the same lock inode;
private temporary namespaces cannot supply independent performance slots.

**Performance Case**:
A reviewed entry of `fixtures/performance/suite.toml`: one workload with its independent oracle,
timing boundary, cache treatment and policy, expanded over finite parameter domains into
instances before any plan is sharded
([ADR-0162](docs/adr/0162-freeze-performance-plans-and-judge-them-by-exact-paired-verdicts.md)).
Its capability state says whether it can run yet; a not-yet-executable case names what is missing.
_Avoid_: **Measurement** — the kernel never runs a Performance Case, and a case produces no
Measurement. Nor "benchmark" for its numbers.

**Performance Sample**:
One execution of one side in one planned slot of a Performance Run, failed or not, with exact
decimal metrics, its oracle outcome and the source, binary and host identities it ran with. A
failed sample is kept; nothing deletes, replaces or adds one.
_Avoid_: a repetition of a Measurement.

**Performance Run**:
A frozen plan (`run.json`) and the evidence bundles recorded against it. The plan fixes the
resolved revisions and the Cargo.lock digest of each, measuring authority, cases, pairs, seeded
order, sentinels and limits before measurement and is never rewritten; a rerun is a new Run with
a parent link.
_Avoid_: "a CI job" — one Run may span shards on several hosts.

**Performance Verdict**:
The laboratory's `pass`, `fail` or `incomplete` for one Performance Run under
`paired_median_tolerance_v2`, recomputed from the run's evidence alone: exact intervals of
baseline-normalized paired losses for the critical family, side-attributed failures, health,
budgets, and each shard's terminal status and record coverage. A missing record of any case or
sentinel, or a shard that did not complete, makes it incomplete.
_Avoid_: **Comparison** — the kernel's fold of two Measurements under an objective. A Performance
Verdict satisfies no Demand and drives no kernel decision.

**Sentinel**:
The baseline-versus-baseline pairs that bracket each shard's schedule. They are health evidence:
an unstable or missing sentinel makes its shard's cases incomplete, and sentinels never enter the
gate family.

**Performance Probe**:
The `perf-probe` example of `review-pipeline`: one process per library Performance Sample. It
generates the case's fixture through the product's public writers, times one production
operation per declared repetition, reports setup separately, and writes one `af.perf-probe/1`
message ([ADR-0163](docs/adr/0163-measure-native-workloads-through-a-public-api-probe-and-independent-golden-oracles.md)).
Its own `passed` is never the oracle.
_Avoid_: "benchmark harness" — the probe asserts nothing about speed.

**Performance Supervisor**:
The `perf-supervisor` example of `review-process`, built once from the measuring authority and
launched as the same bytes for both sides. One process supervises one sample. It brackets child
launch through the observed exit with a monotonic clock, drains capped output and forwards
termination signals. It establishes what its containment can about descendants: a whole Linux
PID namespace as its PID 1, or only a process group in trusted local mode
([ADR-0166](docs/adr/0166-build-frozen-plans-and-supervise-samples-with-one-authority-instrument.md)).
The host cancels it at its backstop or the run's total deadline by SIGTERM, which it forwards to
its sample, and its reply confirms the sample's end or says it is uncertain; a supervisor that
does not reply is killed, and nothing its sample could reach is then removed
([ADR-0171](docs/adr/0171-cancel-samples-through-their-supervisor-carry-the-deadline-through-preflight-and-observe-history-order.md)).
A cancellation admitted before its launch gate prevents the spawn, and one arriving during the
spawn cancels the launched child
([ADR-0173](docs/adr/0173-own-the-supervisor-a-test-runs-and-bound-admission-collection-scratch-and-cancellation-by-absolute-deadlines.md)).
_Avoid_: "process-group containment" for a Linux namespace guarantee; killing the supervisor in
place of its sample.

**Protected Harness**:
Every file a guarded kernel experiment's instrument is built or run from: the laboratory driver,
its fixtures and golden entries, the Performance Probe and Probe Adapters, the trace and
calibration helpers, and every Cargo manifest, lock, build script, toolchain pin and Cargo
configuration, wherever one is added. The literal bootstrap a captured code policy runs hashes
it, refusing links and any missing or extra file, before it imports the candidate's driver or
builds anything; the staged policy subtree that holds the expected digest is its only exclusion
([ADR-0185](docs/adr/0185-measure-guarded-kernel-experiments-through-a-literal-bootstrap-over-a-protected-harness.md)).
_Avoid_: calling product code under measurement trustworthy because the harness is protected;
golden oracles and review still judge it.

**Probe Adapter**:
A reviewed closure of authority example files in `fixtures/performance/adapters.toml`, copied
into a measured source to build its probe. Adapters are tried in fixed priority order and the
first that compiles is chosen; runtime results never select another. Two sides compare library
cases only within one equivalence group.

**CLI Adapter**:
A reviewed entry of `fixtures/performance/cli.toml` covering AF package versions from a declared
minimum. For every instance of a `process` Performance Case it names the actual AF argv, the
Fixture Setup before it and the oracle rules its output must satisfy, or why the instance is
pending. A side's version comes from its own Cargo.lock as the plan bound it, never from its
binary; a side no adapter covers has no CLI coverage
([ADR-0168](docs/adr/0168-run-frozen-shards-with-whole-cli-oracles-and-only-a-per-sample-scratch.md)).
_Avoid_: "golden output" — rules check versions, tokens, outcomes, counts and the declared JSON
fields the supervisor observes over the whole output as one complete document
([ADR-0172](docs/adr/0172-admit-candidate-files-through-bounded-descriptors-and-report-archive-and-trend-runs-offline.md)),
never whole formatting, and are never learned from an execution.

**Fixture Setup**:
The untimed step the Performance Supervisor runs before a sample's measured child, under the same
containment and in the same scratch, writing that side's fixture through that side's own
binaries. Only a setup that exits 0 with a verified lifecycle lets the measured child launch; its
time is reported apart from the child interval.
_Avoid_: counting setup inside a Performance Sample's primary metric.

**Seed**:
A large fixture one side's own writers write once per shard, under the Performance Supervisor
and in the shard's containment: CLI Task and Campaign state, or the repository and event log a
library probe reads, or an admitted Task whose setup leases were released while live. Authority
readers restore a private copy, capture a fresh validator and take a fresh production lease;
the original revision deadline is preserved
([ADR-0193](docs/adr/0193-seed-admitted-authority-fixtures-and-distribute-the-full-suite.md)).
Once every writer process has exited it is sealed and identified by one
digest, verified around every sample, and each sample copies it privately in its untimed
Fixture Setup; a native seed's writer check is judged against its Golden Entry first
([ADR-0169](docs/adr/0169-coordinate-a-machine-seed-each-side-once-and-build-linux-recipes-in-a-pinned-image.md),
[ADR-0170](docs/adr/0170-seed-native-fixtures-freeze-the-build-image-and-bound-the-run-by-one-deadline.md)).
_Avoid_: sharing one Store, projection or copy between samples or sides. Nor reading, sealing or
removing a seed except through held descriptors that never follow a symlink, within its declared
entry, depth and logical-byte bounds
([ADR-0172](docs/adr/0172-admit-candidate-files-through-bounded-descriptors-and-report-archive-and-trend-runs-offline.md)).

**Golden Entry**:
The expected fixture identity, digest and count of one native instance in
`fixtures/performance/golden/native.json`, derived by the independent reference generator from
reference bytes or explicit semantic facts, never from an execution being judged. The trusted
driver refuses a golden document that generator does not derive and a request the registry
does not declare, then checks each side's probe message against the entry
([ADR-0164](docs/adr/0164-judge-only-declared-probe-requests-against-a-reference-checked-golden.md)).

**Measuring authority**:
The revision whose registry, driver, fixtures and supervisor measure both sides of a Performance
Run. A full run's authority is the one the control revision's
`fixtures/performance/activation.toml` binds; a candidate cannot measure itself with its own
instrument.
_Avoid_: **Authority Snapshot** — the Review Kernel's pinned review policy.

**Performance Schedule**:
The `af.perf-schedule/2` record of one Actions event, resolved once to full SHAs by the trusted
control revision's own scripts: the event and its pull request, whether it is trusted and which
executor it may use, each comparison it freezes or why that comparison is unavailable or
refused, and the Release Claim it measures, if any. Enforcement in it is always advisory
([ADR-0177](docs/adr/0177-schedule-continuous-runs-from-trusted-control-and-move-builds-once-to-bounded-shards.md),
[ADR-0180](docs/adr/0180-hand-off-published-releases-read-only-and-measure-each-claimed-identity-once.md)).
_Avoid_: **Pipeline**, **Task** — the Afactory execution abstractions.

**Release Claim**:
The immutable `af.perf-release-claim/1` artifact a read-only release handoff writes before any
sample of a published release. It names the release's identity, the upstream Release run ID, tag
and SHA, never the attempt, and its canonical key. Only the first valid claim of an identity is
measured; a later run of it links that claim's retained result or reports it incomplete, and only
an explicit remeasurement measures again, under its own claim
([ADR-0180](docs/adr/0180-hand-off-published-releases-read-only-and-measure-each-claimed-identity-once.md)).
_Avoid_: **Remote Check**, **lease** — the kernel's gate pull request and Task leases.

**Nightly**:
An af built on the owner's Linux VM from the newest green commit of main, tagged
`nightly-YYYY.MM.DD`, versioned `<core>-nightly.<YYYYMMDD>` and signed with the nightly key, for
the two Linux targets only; a night on main's previous head builds nothing. Its record is
`af.nightly-build/1`. A nightly is never a release: `.af/af.lock` never pins it, and `install.sh`
and `af self` on the stable and rc channels never choose it
([ADR-0265](docs/adr/0265-build-nightlies-from-the-newest-green-main-on-the-owners-vm.md)).
_Avoid_: **Release** — only the release train cuts one; the **nightly** trigger of the
Performance workflow, which measures main against its baseline and builds nothing to install.

**Performance Report**:
The offline page, `af.perf-report/2` document and job-summary table of one Performance Run,
rendered by the laboratory's trusted renderer from its recomputed Performance Verdict: every
planned slot as a raw point, exact bounds, provenance and every reason the run does not satisfy
the required check. It shows measured results, never calibrated sensitivity
([ADR-0172](docs/adr/0172-admit-candidate-files-through-bounded-descriptors-and-report-archive-and-trend-runs-offline.md)),
and links validated diagnostic child runs by parent identity and artifact hashes, with
reproduction commands that keep the frozen configuration
([ADR-0177](docs/adr/0177-schedule-continuous-runs-from-trusted-control-and-move-builds-once-to-bounded-shards.md)).
_Avoid_: rendering or trusting a transported report; it is artifact data.

**Performance Archive**:
A content-addressed filesystem archive of whole Performance Runs, with an append-only index,
explicit retention records, pins and capacity health, filled only by the trusted collector
from verified run directories or Actions transport artifacts. Nothing in it is executed.
_Avoid_: "database" — none is required; and a benchmark holding the collector's credential.

**Compatible History**:
Performance Runs grouped by one case's workload, fixture definitions, policy, recipe,
environment and provenance identity. Run IDs, sources and seed artifacts are provenance, never a
reason to split a group.
_Avoid_: a same-host paired result — a trend compares separate runs.

**Diagnostic Child Run**:
A separate, bounded run of a Performance Run's cases under a diagnostic recipe of
`fixtures/performance/diagnostics.toml`, linked to its parent by run ID and plan digest: CPU
profiles of exactly the bracketed region with flamegraphs, phase traces with their measured
overhead, or counters, each with what it observed or why it is unavailable. It never changes
the parent's Performance Verdict
([ADR-0174](docs/adr/0174-diagnose-in-separate-child-runs-that-record-only-the-bracketed-region-and-share-one-test-executable-boundary.md)).
_Avoid_: a profile of a verdict sample — verdict samples stay uninstrumented; and presenting a
capture of fixture setup as attribution of the timed region.

**Calibration Cohort**:
A predeclared set of A/A, injected-slowdown and power comparisons of one proposed measuring
authority. Every member stays in its denominators; readiness is recomputed from its members,
never accepted as written, and only a reviewed activation that cites it lets full runs gate.
A member is a logical identity fixed before it runs; the Actions run that executes it is
provenance recorded beside the cohort, never written into it
([ADR-0181](docs/adr/0181-declare-cohorts-before-they-run-inject-only-the-candidate-and-count-whole-power-trials.md)).
A member's original is its first dispatch, named for the cohort and the member as the run is
created, at attempt 1, whatever became of its artifacts
([ADR-0182](docs/adr/0182-bind-each-calibration-member-to-its-first-dispatch-and-its-authoritys-plan.md)).
Only an original that succeeded counts. A deleted original is detected by the missing run
number after the cohort's declared anchor, never replaced
([ADR-0183](docs/adr/0183-detect-deleted-originals-recompute-parameters-and-verify-activations-from-retained-evidence.md)).
_Avoid_: counting per-case detections across different trials as a passing power trial; and
choosing a member's original among the runs whose evidence is still readable.

**Activation**:
The reviewed `fixtures/performance/activation.toml` that makes a calibrated measuring authority
active: it binds the cohort, its parameters and its readiness by path and SHA-256, every
original run, and the environment, recipe and CPU/image groups the cohort calibrated. Every
active plan verifies it from those records, and the activation change is verified from the
cohort's retained Actions evidence
([ADR-0183](docs/adr/0183-detect-deleted-originals-recompute-parameters-and-verify-activations-from-retained-evidence.md)).
_Avoid_: treating an author-written readiness document or an archive copy as promotion
evidence; calling an uncalibrated group or an equivalent adapter calibrated.

### What is reviewed

**Snapshot**:
An immutable, admissible source state carrying repository and capture provenance plus one Tree
Digest; its artifact identity is not interchangeable across repositories.
_Avoid_: "commit", "revision", "branch", "checkout" — every one of them names a pointer, and
a Snapshot's identity deliberately survives all of them.

**Tree Digest**:
The content-only identity of a Snapshot's ordered manifest of path, kind, executable bit, and
byte digest; identical trees have the same Tree Digest even when their Snapshots differ.
_Avoid_: using a Tree Digest where repository or capture provenance is required.

**Review Selector**:
The mutable, human-facing inputs used to locate what should be reviewed — branch name, PR
number, base ref, and authority ref — resolved into Snapshots and holding no authority over
identity.
_Avoid_: treating a selector as the thing reviewed; `origin/HEAD` is a Review Selector input,
never a Snapshot or Subject.

**Subject**:
The immutable object a reviewer is asked to judge, in one of exactly two kinds — `whole-tree`
(one head Snapshot entire) or `diff` (the delta between one Base Snapshot and one head Snapshot).
_Avoid_: "the change" used bare — it is only meaningful under a `diff` Subject, and saying it
under `whole-tree` describes something that does not exist.

**Base**:
The Snapshot a `diff` Subject is measured against, resolved once from the Review Selector's ref
at a Campaign's first Round and pinned by Snapshot ID for the Campaign's life.
_Avoid_: "parent" — `parent_snapshot_id` already means patch-integration lineage (S1 produced
by integrating a validated patch into S0) and cannot carry this relation.

**Proposal Base**:
The exact head Snapshot from which a reviewer's sandbox and Proposal patch were derived,
carried by the `base_snapshot_id` field of `PatchProposal@1`.
_Avoid_: **Base** — for a diff Subject, Base is the comparison Snapshot and Proposal Base is the
current head.

**Change Set**:
The derived, content-addressed description of a `diff` Subject: the changed path set and the
patch between Base and head. Recomputed every Round, because the Base is pinned but the head
advances.
_Avoid_: "the diff" when identity matters — a Change Set is an artifact with a digest, not a
command's transient output.

**Authority Snapshot**:
The trusted Snapshot from which a Campaign resolves its pipeline, reviewer lock, and project
policy before candidate content can influence execution authority.
_Avoid_: assuming Authority Snapshot and Base are synonyms; a diff Campaign commonly uses one
Snapshot for both roles, while a whole-tree Campaign still needs authority without having a Base.

**Campaign Manifest**:
The immutable resolved authority of one Campaign: exact Authority Snapshot, pipeline, lock,
reviewer package, policy, convergence, budget, focus, and genesis-root artifact IDs.
_Avoid_: calling it the Authority Snapshot; one trusted source Snapshot can resolve different
Campaign Manifests under different trusted invocation policies.

**Report Scope**:
Whether a Report's location falls inside the Change Set it was made under — `in` or `out`.
Attached deterministically to each Report claim from its exact Round Subject, never supplied by
the reviewer and never stamped on the Finding, because a file this branch has not touched yet may
be touched by a later Round. A Report with no derivable exact Round Subject is presented as
`unknown`; that is fail-closed metadata, not a third Report Scope.
An unavailable Subject or Report authority is counted in the active clean window and is persisted
as `authority_unavailable` in `RunReport@6` conclusions only when no real Finding or failed
Gate is already the cause; terminal output alone is never the only explanation for a failed
convergence decision.
_Avoid_: unqualified "scope" or "out of scope" as a dismissal; an out-of-set Finding is real,
recorded, and triageable — it simply does not block this Subject's convergence.

**Review Slice**:
A persisted, bounded subset of one Subject assigned to a reviewer shard by static policy or a
Planner.
_Avoid_: treating a Slice as a smaller Subject; shard results still belong to the whole Subject
and dynamic slicing requires whole-Subject closeout.

### How a review runs

**Campaign**:
One logical review, spanning as many Rounds as it takes to converge, owning one event log and
one Ledger.
_Avoid_: starting a second Campaign to escape an inconvenient Ledger; re-invoking the same
name continues the same review.

**Campaign Mode**:
The trusted invocation choice that bounds how many closed Rounds a Campaign may dispatch.
`light` is the default and closes after exactly one completed review Round; its next step after
Findings is deterministic repair and project Gates, not another Campaign. `heavy` is explicit
human-directed opt-in and retains the pipeline's full convergence window. The effective policy is
pinned in the Campaign Manifest and cannot change on resume.
_Avoid_: interpreting a finding-bearing light result as a request to open a fresh Campaign.

**Round**:
One immutable Subject/input-Set execution of the pipeline within a Campaign. Only a Round that
reaches a real verdict *closes*; an incomplete one resumes those exact inputs unless explicitly
superseded, and never consumes a clean/cap slot merely for failing to complete.
_Avoid_: "run" as a synonym - an `af review run` invocation may fail to close a Round at all.

**Gate**:
The Check nodes that must pass before any reviewer is dispatched. A Check that could not run
is not a pass, and neither is a Gate with no required Checks.
_Avoid_: "the build" — a Gate is whatever the pipeline declares, and a vacuous one is its most
dangerous state.

### What comes back

**Report**:
One immutable claim by one reviewer attempt — severity, location, body, and a concrete fix.
It carries no status, no round, and no resolution.
_Avoid_: "finding" for this; a Report is evidence, and several may attach to one Finding.

**Finding**:
The Ledger's unit of triage: the durable identity a defect holds across reviewers and Rounds,
carrying status, severity, and every Report attached to it.
_Avoid_: "issue", "bug" — both imply a tracker outside this system.

**Grouping**:
A recorded, reversible adjudication that two Findings represent one defect, preserving both
Finding IDs, every attached Report, and every Report's independent verification obligation.
_Avoid_: "merge" — grouping does not erase a claim, rewrite history, or make one verification
cover another.

**Dispute**:
One reviewer's recorded position that an existing Finding is wrong, naming the Finding and
carrying a reason. Attached immutably like a Report; it moves the Finding to `contested`,
which blocks convergence exactly as `open` does.
_Avoid_: reading a Dispute as a veto — it never lowers what blocks, it raises what a human
must adjudicate. And never conflate it with a Drop: a Dispute says the claim was wrong, while a
Drop says it is no longer reproduced on this Subject.

**Drop**:
One reviewer's explicit, immutable position that a prior Finding assigned to it is no longer
reproduced on the current Subject.
_Avoid_: inferring a Drop from silence; an omitted required disposition makes the Round
incomplete.

**Demand**:
A durable, snapshot-scoped obligation to measure a performance claim, naming the claim in prose
rather than a Finding because it may live in a commit message or comment instead of the Ledger.
_Avoid_: treating a Demand as attached to a Finding or as disposable round output; it is an
independent Campaign obligation.

**Evidence**:
The content-addressed measurement artifact linked to the exact Demand or resolution it supports
and to the Subject snapshot it measured.
_Avoid_: "the benchmark" as a number in a note — the kernel can check that Evidence exists and
is linked, never that the measurement behind it was sound.

**Change Attestation**:
An authenticated claim that an external change on an exact Subject is intended to address named
Finding/Report claims, carrying lineage and Evidence but no resolution authority.
_Avoid_: "mark fixed" — an Attestation moves work to verification; it does not prove success.

**Fix Verification**:
A trusted policy result that named active Report claims are no longer reproduced on one exact
Subject and that every required check and Evidence obligation passed there.
_Avoid_: treating patch application, a Drop, or a Change Attestation alone as verification.

**Resolution**:
A trusted, evidence-bearing disposition of named Finding/Report claims, scoped to a Subject and
policy revision and challengeable by materially new evidence.
_Avoid_: an unqualified status toggle; `wontfix-tracked` also carries a severity ceiling and
expiry.

**Ledger**:
The projection of a Campaign's Findings, rebuilt from the event log and the immutable artifacts
its events reference.
_Avoid_: treating it as storage — it is never written directly, and hand-edited state has no
way in.

**Finding Set**:
An immutable, Subject-bound Ledger view at one deterministic reduction boundary, passed to nodes
by artifact ID.
_Avoid_: an ambient query for "current findings"; every consumer names one exact Finding Set.

**Demand Set**:
An immutable, Subject-bound view of open, satisfied, stale, and waived Demands at one deterministic
reduction boundary.
_Avoid_: a list of whatever Demand events happen to exist when a node starts.

**Proposal**:
A reviewer-authored atomic patch linked to one or more Report/Finding claims, admitted only when
it equals the diff the kernel computed by sealing that reviewer's sandbox.
_Avoid_: "the fix" — a `fix` is the prose remedy every Report must carry; a Proposal is the
executable one, and a Finding can have the first without the second. Export is human-directed;
automatic Integration, when policy permits it, advances only an internal derived Snapshot.

**Integration**:
A trusted, transactional promotion of one or more validated Proposals into a new internal head
Snapshot after combined-tree checks pass.
_Avoid_: "apply to the repository" — Integration never writes a source checkout, branch, or PR.

**Attempt**:
One execution of one node. An abandoned Attempt is *fenced*, and anything arriving under a
revoked epoch is quarantined — recorded, charged, and unable to reach the FindingSet. An Attempt
is *abandoned* when its Writer was lost or its host cancelled it (SIGINT, SIGTERM): it is charged
the usage its Provider reported, never its reservation, and counts against its node's Recovery
Allowance instead of its Attempt limits
([ADR-0209](docs/adr/0209-recover-a-lost-or-interrupted-attempt-without-charging-its-reservation.md)).
_Avoid_: treating a fenced Attempt as a free retry; it spent tokens and the budget knows.

**Writer**:
The one `af` process that holds a Task's lease and may record on it, renewing the lease from its
own heartbeat thread. A successor takes the lease only after it expired or was released, so two
Writers never coexist; a lost Writer records nothing more, and its successor settles its
unfinished Attempt as abandoned.
_Avoid_: "owner" or "session" — a Writer is one lease epoch, not a person or a terminal.

**Recovery Allowance**:
How many abandoned Attempts one node may have and still be dispatched again: one, unless its
slot declares `recovery_attempts` (`af.pipeline/3`). Past it the Task cannot continue, and
`af task run` says so on its first line.
_Avoid_: counting it as an Attempt limit; Attempt limits count only Attempts that answered or
failed.

**Budget Scope**:
The accounting boundary charged by one Attempt — each named token scope it belongs to (one
node's cap, or a reviewer fan-out's) and the Task as a whole.
_Avoid_: **Report Scope**; Budget Scope governs spend, never whether a claim blocks.

**Execution Binding**:
The trusted, content-pinned sandbox, tool, network, environment, credential, and quota policy
under which one executable node runs.
_Avoid_: an ambient/default environment; an unresolved or insufficient Binding fails planning.

**Provider**:
An operator-named, machine-local authentication context for one adapter kind. A Provider ID is a
stable label for that context, not a verified or immutable external account identity: credentials
inside the context can change without changing the ID. An ambient CLI login may be displayed as an
explicitly unstable candidate but does not become a configured Provider until the operator names
its context. Credentials are capabilities used through a Provider and never part of its ID.
_Avoid_: using "provider" for a CLI binary, model, reviewer package, or verified account identity.

**Provider Admission**:
A Task's captured, paid proof that one configured Provider can satisfy one adapter capability:
after a token-free account identity probe, the Task runs one bounded acknowledgement inference
under its own Attempt and reservation, charged to the Task budget. Its selected output lets that
Task's Workers dispatch on the binding. Admission is local execution state, not pinned pipeline
authority, and `af provider doctor` runs only a Review Task's admissions.
_Avoid_: treating an ambient login or a Provider ID as proof of usable authentication.

**Cache Snapshot**:
A bounded, credential-free copy or copy-on-write clone of an administrator-approved dependency
cache, materialized inside one sandbox and discarded with it.
_Avoid_: "passthrough" — a direct host mapping is not a Cache Snapshot and cannot satisfy safe
isolation.

**Build Cache**:
A bounded, explicitly unsafe capture of a Gate's candidate-built output, admitted only under the
trusted-local policy and cloned into Worker sandboxes that declare its kind within the same
Round; removed before seal, so it never enters a candidate tree, Proposal or delivered worktree.
_Avoid_: **Cache Snapshot**; a Build Cache was produced by candidate code and carries no
administrator approval and no credential-free guarantee.

**News**:
Whether an in-scope claim was created, reopened, materially escalated, challenged, or moved to
pending verification inside the clean window, independent of its current status; an exact
corroborating re-report is not News.
_Avoid_: reading a fixed Finding that still blocks convergence as a bug; that is News doing its
job.

**Convergence**:
The kernel's verdict over one exact final Finding Set, Demand Set, required-node/gate state,
Semantic Closure, budgets, and clean-Round window.
_Avoid_: a reviewer verdict or an empty Finding list; neither proves the required review ran or
that all obligations closed.

**Semantic Closure**:
The proof that every verdict-bearing output from every selected node reached its authoritative
reducer, gate, verifier, or integrator, or an explicitly trusted disposition.
_Avoid_: assuming a successfully completed graph is closed; an unwired Finding or Demand makes
the result incomplete.

**Warm Set**:
The exact, per-node set of carried layers one Round's Attempts start from: Notes and the Head
Delta selected from the previous closed Round's admitted Attempt of the same node, the Build Cache
carried from this Round's Gate, and the basis of the node's Warm Workspace, recorded as
`WarmSet@2` before the first dispatch and listed in every Attempt's context manifest. A layer that
cannot be carried is recorded with its reason, never omitted silently. No harness session is
carried: every Attempt is a new conversation.
_Avoid_: "warm cache" or "the previous session" as ambient state; a Warm Set is an artifact, and
a retry inherits the Round's Warm Set rather than its failed sibling's state.

**Warm Workspace**:
One node's stable template root within one Campaign, named by an opaque workspace identity and
re-based to each new head by tree diff on a copy-on-write clone of the previous template. The
result is trusted only when its manifest digest equals the head's Tree Digest; otherwise the head
is materialized in full and the reason recorded. Per-Attempt sandboxes remain fresh clones of it.
_Avoid_: treating the root as the sandbox or as shared state; a Warm Workspace never crosses
nodes, and a host path never names it in a durable record.

**Worker Notes**:
A bounded, typed inspection map one admitted Attempt leaves for the next Attempt of the same
node: paths inspected, a model of the change, open questions, per-path hints. Data, not a
verdict, and never shared across nodes or slots.
_Avoid_: treating Notes as a disposition; every prior Finding still needs its explicit Report,
Dispute, or Drop.

**Head Delta**:
The kernel-derived relation between two consecutive heads of one node: exact from and to
Snapshot IDs, diff policy identity, complete path set with rename truncation, and one mark per
path over the union of the Notes paths and both Subject views.
_Avoid_: **Change Set**; a Head Delta names no Base, carries no Subject identity and no Report
Scope, and exists for whole-tree Subjects too.

**Delta Marking**:
The rendering of Head Delta marks beside the Change Set section: `changed`, `unchanged`, `new`,
`reverted`, `removed`, or `renamed` since the previous Round's head.
_Avoid_: a second full patch in the prompt; the marks exist to keep context minimal.

## Relationships

- A **Campaign** has many **Rounds**; a Round produces many **Reports**; Reports fold into
  **Findings**, many-to-many across reviewers and Rounds.
- A new **Report** creates a new **Finding** unless it explicitly corroborates an existing one or
  exactly matches a trusted semantic occurrence key; path and title are grouping hints only.
- **Grouping** changes adjudication, never identity: renames and rewordings do not change a
  Finding ID.
- A required **Demand** remains blocking until trusted policy records snapshot-current
  **Evidence** or an operator explicitly waives it; resolving a Finding never disposes of a
  Demand implicitly.
- Every prior Finding assigned to a required reviewer receives an explicit current-subject
  **Report**, **Dispute**, or **Drop**; absence is incomplete work, never evidence of repair.
- A **Change Attestation** or integrated Proposal moves covered claims to
  `pending-verification`; only a positive **Fix Verification** can produce a `fixed` resolution.
- A `rejected` or `wontfix-tracked` **Resolution** remains terminal only inside its recorded
  evidence, Subject scope, severity ceiling, and expiry; a material challenge moves it to
  `contested` through an explicit event.
- Every executable node resolves one **Execution Binding** before dispatch. A reviewer that can
  read reusable credentials is `trusted_unsafe` and cannot authorize `auto_apply`.
- A configured **Provider** dispatches a Worker only after its Task's **Provider Admission**
  succeeded, and the native account is rechecked before every private send
  ([ADR-0090](docs/adr/0090-recheck-native-task-provider-identity-before-private-invocation.md),
  [ADR-0091](docs/adr/0091-capture-explicit-task-provider-admission-costs.md)).
- **Convergence** reads only the Round's exact final Finding Set and Demand Set plus recorded graph,
  gate, closure, and budget state; it never queries ambient latest projections.
- A **Subject** is always anchored to one head **Snapshot**; a `diff` Subject additionally
  names a **Base** Snapshot and derives a **Change Set** from the pair.
- Snapshot materialization atomically publishes CAS bytes only after fixed-buffer verification,
  then clones duplicate regular files in a separate bounded phase; candidate-controlled object
  size never becomes resident allocation or a reason to park the shared infrastructure executor.
  See
  [ADR-0020](docs/adr/0020-stream-cas-materialization-and-clone-duplicates.md).
- A **Proposal** applies only to its **Proposal Base**, which is the Subject's head Snapshot for
  that Attempt and is not the diff Subject's Base.
- A **Review Selector** resolves the Authority Snapshot and optional Base once per Campaign, then
  resolves each Round's head Snapshot into that Round's immutable **Subject**. Its labels may be
  retained as metadata but never participate in identity.
- A Campaign pins one **Authority Snapshot** and one **Campaign Manifest**. Later Rounds
  may advance the Subject's head Snapshot but cannot silently change pipeline or reviewer
  authority.
- The **Ledger** is a function of the event log and its referenced immutable artifacts. Delete
  the projection, replay those sources, same answer; a missing referenced artifact is corruption,
  not an empty field.
- **Finding Sets** and **Demand Sets** are immutable views produced by deterministic reducers;
  graph edges deliver exact view IDs and nodes never consume ambient "latest" state.
- **Report Scope** is derived for each **Report** claim from its exact Round Subject. Convergence
  evaluates active claims independently: a Finding blocks when any active claim is `in` at the
  severity gate and is wholly `out` only when all active claims are `out`. **News** and Report
  Scope remain independent axes.

## Example dialogue

> **Dev:** "Round 3 is reviewing a different change than round 1 — someone pushed to main.
> Should I re-run against the new base?"
>
> **Domain expert:** "No — you're describing a **Review Selector** moving, not a **Base**
> changing. The Base was pinned by Snapshot ID when the **Campaign** opened, so every
> **Round** measures the same delta and convergence reflects your fixes and nothing else. If
> you genuinely want to review against newer upstream, that's a new Campaign, because the
> **Findings** in this one are keyed to a **Subject** that no longer exists."

## Flagged ambiguities

- **"the change"** — used in both reviewer prompts and the early pipeline definitions (today's
  `.af/pipelines/review.toml`) while the kernel captured only a whole-tree Snapshot, so it named
  something structurally unavailable. Resolved: the
  umbrella term is **Subject**, and "the change" is legitimate only under a `diff` Subject.
- **Report vs Finding** — used interchangeably in prose. Resolved above: a Report is one
  attempt's immutable claim, a Finding is the triage identity many Reports attach to.
