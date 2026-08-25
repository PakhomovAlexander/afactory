# Review Kernel workstream log

## 2026-08-20 — M0 and M1

Closed the append-only event vocabulary, structural run reports, typed invocation/output ports,
and canonical report projection. Added complete operator rendering through `show`, long ledger,
and markdown report commands.

## 2026-08-20 — M2.1 capability-negotiation slice

Added versioned pipeline Subject configuration and digest-pinned reviewer capability declarations.
Legacy pipeline and package formats remain whole-tree-only. Unsupported diff execution, inline
diff reviewers, empty reviewer sets, unsafe registry names, symlink/non-regular package entries,
and lossy package paths fail closed. Shipped reviewers remain whole-tree-only until M2.2–M2.4
provide pinned authority, Base, Subject, and Change Set artifacts. Runtime `Subject@1` publication
remains the first M2.2 integration slice; this entry does not claim it landed.

## 2026-08-20 — M2.2 immutable authority bootstrap

Added `CampaignManifest@1`, `CampaignOpened@1`, `Subject@1`, `RoundStarted@1`, and explicit
`RoundInputSuperseded@1` epochs. A new Campaign captures its trusted Authority Snapshot before
candidate capture and publishes exact pipeline, lock, package, policy, convergence, budget,
focus, and genesis-root identities. Continuation reconstructs reviewer packages from captured CAS
bytes and never re-reads live package paths. Incomplete Rounds reuse exact Subject and input-set
IDs; capturing a changed head requires `--restart-round`. Node invocations, output receipts,
attempt lifecycle events, and run conclusions are causally and artifact-bound to their Round,
Campaign Manifest, Authority Snapshot, and Subject.

The next slice is M2.3's typed, configuration-neutral Git tree diff, followed by M2.4 Change Set
publication and port wiring. Diff Campaigns pin Authority/Base now but still fail before dispatch.

## 2026-08-21 — M2.3 typed Git tree diff

Added opaque, resolved Git tree ids and one private, single-call-site path for a completely fixed
tree-to-tree diff. The generic Git runner still rejects `diff`; the typed path fixes algorithm,
rename threshold, binary/full-index output, prefixes, quoting, locale-sensitive behavior, external
drivers, text conversion, and NUL-delimited path parsing. Raw records become byte-safe typed path
changes while patch bytes remain available for M2.4's Change Set artifact. Adversarial coverage
proves candidate-selected textconv code does not execute and hostile diff configuration cannot
change the result.

## 2026-08-24 — M2.5 Report Scope

Made Report Scope a deterministic Ledger projection of each immutable Report location and its
exact Round Subject/Change Set. Attached claims now retain independent `in`/`out` values,
whole-tree and change-wide Reports are always `in`, and pre-Subject legacy evidence renders
`unknown` and fails closed. Convergence uses the highest non-out attached-claim severity and
scope-aware News instead of the Finding's arrival-dependent adopted severity. No Report, event,
or artifact contract changed.

## 2026-08-24 — bounded Provider Operations prerequisite

Added explicit machine-local reviewer-to-Provider bindings and a pre-dispatch operation that
durably records structural authentication and bounded real-inference smoke transitions. Exact
operation epochs fence stale continuation; failed, timed-out, and abandoned work is charged;
transient failures receive at most one automatic retry; repeated normalized failures open the
circuit. Persistent state is a closed, versioned schema containing only IDs, classification,
fingerprint, timing, spend, next action, and a non-secret continuation handle. Provider labels
remain outside pinned Campaign authority. M2.6 is next.

## 2026-08-24 — M2.6 rename-aware Report Scope

Closed the acceptance gap across the existing M2.3-M2.5 production path: the typed Git diff
publishes both sides of a rename in `ChangeSet@1.changed_paths`, Change Set validation requires
both endpoints, and Ledger replay consequently projects Reports against either path as `in`.
Acceptance coverage also proves an unrelated path remains `out` and replay does not rewrite an
existing legacy Finding key. No persisted contract changed. `make check` passed, including
fixture reproduction; the M2 dogfood Campaign is the gate before M3.1.

## 2026-08-24 — M2 dogfood Round 1

Campaign `m2-rename-scope` ran the pinned external `af 0.1.0` against committed M2 head and
closed Round 1 with fourteen Findings at 317,395 tokens. The Round exposed a blocker where
losslessly encoded Change Set paths did not match ordinary percent-bearing Report paths, plus
major claims about discarded secondary Report locations, reviewer prose contradicting the diff
Subject, an overclaimed legacy-identity test, and serial whole-tree sandbox hashing. Minor claims
identified redundant Change Set allocations, reads, validation passes, a silent Git rename-limit
fallback, and missing round-authority diagnostics. M2 was reopened; no M3 work starts until the
same Campaign converges.

## 2026-08-24 — M2 dogfood Round 1 corrections

Corrected all fourteen accepted Round 1 claims without changing persisted contracts or policy.
Report Scope now shares the source adapter's lossless path codec, considers every typed Report
location, and records mismatched Round authority. Live Report artifacts use `FindingReport@1`
while the permanent projection reader retains the earlier flat M1 shape. Reviewer packages now
describe the exact diff Subject and are content-locked at version 1.3.0. Change Set rendering and
admission reuse parsed and borrowed values, Git fails closed when its fixed rename limit truncates
detection, and sandbox sealing hashes baseline files through bounded parallel 64 KiB streams.

`scripts/verify.sh` and markdownlint passed. The manual 5,000-file / 199 MiB sealing measurement
completed in 0.589 seconds; the over-limit Git integration test exercised 1,001 delete and 1,001
add candidates and observed the fail-closed diagnostic. Campaign Round 2 remains the convergence
gate before M3.1.

## 2026-08-24 — M2 dogfood Round 2 corrections

Round 2 spent 367,930 tokens, retained all fourteen prior resolutions as fixed, and opened ten
Reports representing nine obligations. Canonical repository-relative validation now has one core
implementation shared by Change Sets and Reports; malformed live paths are refused, while an
unreadable persisted Report becomes diagnostic `unknown` blocker evidence instead of bricking
replay. `FindingReport@1` has one semantic validator and a schema/reader conformance corpus.
Multi-location projection displays the location that actually established Scope, and repeated
Round Subject resolution reuses cached, path-only authority.

[ADR-0017](adr/0017-record-rename-truncation-and-continue.md) records the choice to preserve a
complete diff Subject when Git truncates rename linkage: `ChangeSet@1` carries the durable flag
and the policy identity carries the limit. Template materialization deduplicates CAS verification
and parallelizes distinct content, template cloning is bounded-parallel, seal hash buffers are
reused per worker, and the production worker budget is divided by scheduler concurrency. The
manual 5,000-file / 199 MiB production-budget measurement recorded 1.138 s materialization,
0.243 s clone, and 2.512 s seal. `scripts/verify.sh` and markdownlint pass on the complete Round 2
correction tree. Round 3 remains the convergence gate before M3.1.

## 2026-08-24 — M2 dogfood Round 3 corrections

Round 3 spent 366,517 tokens, retained all 24 prior resolutions as fixed, and opened nine Reports.
Frozen M1 flat Reports remain readable under their original admission rules; unreadable typed
Reports now attach diagnostics without replacing readable claim content, and typed Subject/Report
authority failures block convergence. Reviewer-result admission applies the same semantic path
contract before selection and retries only the offending reviewer. The temporary multi-location
bridge key is order-independent pending M3 identity.

Filesystem phases now share one process-wide worker permit pool: a lone materialize, clone, seal,
permission, or CAS durability phase can use every available core while overlapping phases share
the same capacity. Symlink-parent containment uses ancestor hash lookup, and both read-only file
permission changes and cleanup restoration fan out without skipping hostile reviewer mutations. The
corrected manual fixture has 4,500 distinct blobs and 500 symlinks. For 5,000 entries / 199 MiB it
recorded 0.999 s materialization; writable clone+permissions 0.681 s and seal 0.656 s; read-only
clone+permissions 1.078 s and seal 0.650 s. `scripts/verify.sh` and markdownlint pass on the complete
Round 3 correction tree.

## 2026-08-24 — M2 dogfood Round 4 corrections

Round 4 spent 443,431 tokens, retained all 33 prior resolutions as fixed, opened twelve Reports,
and exhausted Campaign `m2-rename-scope`. Frozen flat non-canonical paths now project fail-closed
`unknown`; first unreadable Reports materialize actionable blocker Findings, and authority failures
age out only after the clean window. Prior Findings never echo a path live admission refuses. The
rename limit has one executable constant guarded against its durable policy identity, Report
authority deduplication is constant-time, and canonical base64 admission validates without decoding
the patch.

Worker permits moved from the contracts crate into configurable, re-entrant `review-parallel` and
are acquired per item so concurrent phases interleave. CAS `fsync` uses an independent I/O fan-out.
Repeated-content materialization caches one verified read while distributing occurrences. Sandbox
seal restores only directories that are actually non-writable and parallelizes known-file teardown
with a recursive safety fallback. For 5,000 entries / 199 MiB, the current measurement recorded
1.644 s distinct-content materialization, 1.507 s repeated-content materialization, 0.687/1.143 s
writable/read-only clone+permissions, 0.699/0.694 s seal, and 0.131/0.130 s teardown. Affected tests
and measurements pass; `scripts/verify.sh` and markdownlint pass over the full workspace. Because the
first Campaign is durably exhausted, convergence now requires a fresh Campaign under the same
unchanged policy, not a larger round cap or weaker gate.

## 2026-08-24 — M2 fresh dogfood Campaign Round 1 corrections

Fresh Campaign `m2-rename-scope-final` Round 1 spent 396,132 tokens and opened sixteen Reports,
including the same unbounded repeated-content cache diagnosis from both reviewers. Repeated
materialization now holds one duplicated digest at a time while scheduling that digest's
occurrences independently; symlink containment skips the ancestor scan when no symlink exists and
never climbs above the materialization root. The permanent ReviewerResult reader and schema name
both legacy-flat and typed Report shapes, validate either without deep-cloning JSON, and retain the
raw answer as a failed-attempt artifact reference when live admission refuses it.

The Ledger keeps a bridge identity path separate from its Scope-selected presentation location,
and a readable Report replaces an unreadable first-Report placeholder regardless of severity.
Allocation-free canonical-base64 validation is cross-checked against the configured decoder over
20,000 generated inputs. Sandbox scan restores owner traversal permissions before `read_dir`, drops
its retained per-file cleanup plan, re-walks only during teardown, removes directories in reverse
DFS order, and moves baseline path/digest data rather than cloning it.

ADR-0018 replaces transferable, per-item worker permits and per-phase thread creation with one
process-owned bounded executor initialized by `reviewctl`; nested and overlapping phases share its
fourteen workers. The 5,000-entry / 199 MiB measurement recorded 2.126 s distinct-content and 2.448
s repeated-content materialization, 0.712/1.121 s writable/read-only clone+permissions, 0.982/0.987
s seal, and 0.197/0.231 s teardown. The current drop-time re-walk remains slightly faster than the
0.203/0.270 s serial baseline without retaining file paths for the sealed sandbox's lifetime. A
separate 128-distinct-pairs / 256 MiB output workload materialized in 5.567 s; `/usr/bin/time -l`
reported 238,354,432 bytes maximum command RSS and a 37,421,608-byte Darwin peak memory footprint.
Affected tests, full-workspace clippy, `scripts/verify.sh`, and byte-identical fixture reproduction
pass.

## 2026-08-24 — M2 fresh dogfood Campaign Round 2 corrections

Fresh Campaign Round 2 spent 473,552 tokens, retained all sixteen Round 1 resolutions, and opened
eleven Reports. Authority recovery now treats a readable claim as new content: it reopens any
resolution made against the synthetic unreadable-artifact placeholder and restores the claim's
identity path and paired line. Prior-Finding rows likewise keep identity path/line together and
encode change-wide identity as a null row path whose required reviewer output is an empty `file`.
The legacy ReviewerResult schema is aligned with durable reader bounds through a shared
conformance corpus. Subject Scope cache entries and active scopes share immutable authority with
`Arc` rather than deep-cloning changed-path sets.

Manifest byte and streaming identity now share the source adapter's single digest API. Seal scans
repair only missing read/traverse permission on a best-effort basis, while teardown separately
restores write access. Baseline membership uses one bit per manifest position rather than copied
path strings. Clone, read-only permission, known-file teardown, and baseline hashing drain bounded
task batches during their walks. Materialization loads at most one distinct digest per executor
worker, then fans all occurrences in that bounded window across the shared executor.

The 128-distinct-pairs / 256 MiB workload improved from 5.567 s to 1.332 s while
`/usr/bin/time -l` reported 52,723,712 bytes maximum command RSS and a 35,930,616-byte Darwin peak
memory footprint. The 5,000-entry / 199 MiB measurement recorded 2.008 s distinct-content and
0.992 s repeated-content materialization, 0.670/1.037 s writable/read-only clone+permissions,
0.680/0.693 s seal, and 0.127/0.130 s teardown. Focused contract, ledger, source, sandbox, cleanup,
and prompt tests pass. Full workspace clippy, tests, byte-identical fixture reproduction, and
markdownlint pass before the Round 2 correction commit.

## 2026-08-24 — M2 fresh dogfood Round 3 pre-dispatch gate correction

Round 3 epoch 1 reached no reviewer and spent no reviewer tokens. Its required `scripts/verify.sh`
check blocked for 53 minutes in `/usr/local/bin/docker info`: container capability detection had no
deadline, so an installed client with a wedged daemon could hold the review gate forever. A stack
sample showed the scheduler waiting on the check while the test process waited on Docker. The
incomplete run was interrupted; the exact orphaned Docker leaf was terminated and its verification
subtree then exited naturally.

Container runtime probes now have a five-second deadline, run in their own process group, capture
output through temporary files rather than pipes descendants can retain, and kill surviving group
members on either timeout or wrapper exit. Timeout is `Unusable`, preserving the fail-closed
isolation claim. A synthetic wedged-runtime test returns in 0.11 s; the real wedged host Docker
probe returns in 5.08 s. Full verification now passes naturally without a Docker environment
override, including the real-host detection test and byte-identical fixture reproduction. Restart
the incomplete Round 3 against the corrected snapshot; it has not established a clean Round.

## 2026-08-24 — M2 fresh dogfood Round 3 corrections

Round 3 epoch 2 restarted against the bounded-probe snapshot, spent 495,186 tokens, and opened nine
Reports. The legacy-shaped reader now follows its published v1 schema for whitespace content and
literal paths while the frozen shell importer retains its historical trimmed-title projection.
Typed Report replay deserializes borrowed JSON, preserves readable claim content when locations are
noncanonical, records unknown Scope, and no longer replaces the claim with an unreadable synthetic
blocker. Manifest admission rejects duplicate paths before any parallel write can race.

Materialization now sorts compact entry indexes into digest groups and submits one executor pass in
which each task admits the authoritative CAS object size, reads it, and writes all occurrences. A
64 MiB condition-variable budget bounds resident content independently of worker count; an
oversized object runs alone. The warmed 5,000-entry / 199 MiB measurement recorded 1.628 s
distinct-content and 1.882 s repeated-content materialization, improving the Round 2 distinct
measurement of 2.008 s. The 128-distinct-pairs / 256 MiB workload recorded 1.978 s and 53,248,000
bytes maximum command RSS with a 36,454,904-byte Darwin peak footprint.

Scope-authority failures are now counted in the active clean window, printed by the run command,
and persisted distinctly as `authority_unavailable` in additive `RunReport@3`; permanent @1/@2
readers remain unchanged under ADR-0002. ADR-0019 records the decision. Focused schema, migration,
store, pipeline, CLI, source, and sandbox tests pass. Round 4 remains useful dogfood, but because
Round 3 was not clean, this Campaign can no longer produce the required two clean Rounds within its
four-Round policy; convergence will require another fresh Campaign under the same policy.

## 2026-08-24 — M2 fresh dogfood Round 4 corrections

Round 4 spent 605,986 tokens, retained all 35 prior resolutions, opened eight Reports, and
exhausted Campaign `m2-rename-scope-final`. Two reviewers independently identified the same
resident-budget/nested-executor deadlock, including one blocker: a worker could hold content bytes,
enter a nested fan-out, steal a sibling task, and wait behind its own permit. Ordinary digest
groups now drain in one byte-budgeted outer fan-out with serial per-group occurrences. Heavily
repeated groups are skipped by that pass and only then use nested parallel writes from the caller
thread, when no sibling can still wait on the same budget.

`Manifest::new` now returns a typed error; validation uses the canonical sorted invariant to reject
duplicate or out-of-order adjacent paths in O(n), and materialization checks symlink ancestors in
the same serial validation pass without re-rendering every path. The live ReviewerResult reducer
normalizes both contract arms to `FindingReport@1` and preserves every typed location through
ledger admission. Finding Report line bounds now match the readers' `u32` domain with shared
boundary corpus cases. A blocked gate remains the immediate durable cause even when background
Scope authority diagnostics also exist.

The shipped 5,000-entry / 199 MiB release measurement recorded 0.488 s distinct-content and 0.612 s
repeated-content materialization. The 128-distinct-pairs / 256 MiB workload recorded 0.085 s and
53,329,920 bytes maximum command RSS with a 36,536,824-byte Darwin peak footprint. Focused contract,
source, store, and pipeline regressions pass. A fresh unchanged-policy Campaign must establish two
clean Rounds after these final corrections are fully verified and resolved.

## 2026-08-24 — M2 convergence Campaign Round 1 corrections

Fresh Campaign `m2-rename-scope-convergence` Round 1 spent 561,837 tokens and opened fifteen
Reports. ReviewerResult@1 now has one wire shape—the flat report every adapter emits—while live
ingestion remains the producer of typed FindingReport@1 artifacts. Its schema matches the
permanent reader's optional line and confidence fields. RunReport version membership is
centralized, and `authority_unavailable` is emitted only when missing authority is the sole
non-exhaustion cause. Prior prompts omit synthetic authority diagnostics and never pair a null
file with a line.

Materialization no longer reads candidate-sized regular files into memory or parks shared
executor workers. One task per digest streams a verified occurrence through a 64 KiB buffer and
reflinks or copies duplicates; symlink targets are the only whole objects retained and are
refused above 16 KiB before allocation. Path parents
are prepared component by component at the write boundary, and declared symlink ancestry is
preflighted before concurrent work. ADR-0020 records this boundary. Change Sets regain their
render-time byte bound and full validation, unfamiliar Git diff warnings fail closed as rename
truncation, sealing uses binary search over the already sorted baseline, reviewer output parsing
moves rather than clones JSON, and CAS fsync fan-out has an explicit sixteen-worker ceiling.
The release measurements recorded 0.943 s for 5,000 distinct entries / 199 MiB, 0.510 s for the
same output with repeated content, and 0.467 s for 128 distinct duplicated 1 MiB blobs / 256 MiB.

## 2026-08-24 — M2 convergence Campaign Round 2 corrections

Round 2 spent 615,571 tokens, retained fourteen Round 1 resolutions, reopened the CPU-derived CAS
fsync fan-out, and opened ten further Reports. CAS durability now always uses at most sixteen I/O
workers independent of reported CPU count. Reviewer results pass the durable store's complete
validator before any result artifact or receipt is admitted, so invalid demands and disputes are
charged and retried by the same path as invalid Reports. Container execution now shares the
probe's process-group supervision and has a fixed fifteen-minute deadline. Run-report append
checks narrow their SQL scan to the stable type family before typed structural classification;
deserialized manifest lookup remains correct even before callers validate canonical ordering, and
reviewer output conversion moves its serialized object rather than cloning it.

CAS materialization writes unverified bytes only to a sibling temporary file and atomically
renames the target into view after fixed-buffer digest verification. Entry grouping is linear and
first-occurrence preserving. Distinct verified sources and regular-file duplicates run in two
non-nested bounded executor phases, allowing one repeated digest to use the whole worker budget.
Symlink manifests decode paths once, ordinary source paths are retained once per digest, and a
shared prepared-directory set avoids restating every ancestor for every entry. ADR-0020 and the
materialization invariant now record those boundaries.

The final release measurements recorded 1.776 s for 5,000 distinct entries / 199 MiB and 0.845 s
for the same output with repeated content. The 128-distinct-pairs / 256 MiB workload recorded
0.876 s, 51,707,904 bytes maximum command RSS, and a 34,914,808-byte Darwin peak footprint. The
extra source-publication cost buys the invariant that corrupt bytes never exist at a declared
target path. Focused contract, source, store, sandbox, and pipeline regressions pass. Full
workspace clippy, tests, and byte-identical fixture reproduction pass before the correction
commit. Because Round 2 was not clean, this Campaign cannot establish two clean Rounds within its
four-Round policy.

## 2026-08-25 — M2 convergence Campaign Round 3 corrections

Round 3 spent 693,947 tokens, retained all 25 prior resolutions, and opened eight Reports. Typed
FindingReport projection now validates every semantic field while retaining the established
fail-closed exception that readable content with a noncanonical frozen path has unknown Scope.
Round/Subject binding mismatches have their own authority kind and operator diagnostic instead of
claiming a readable Subject artifact is unavailable. ReviewerResult admission has one validator,
and the event-buffer documentation now states the real boundary: successful node batches are
internally canonical, while dispatch and terminal failure order records concurrent completion
because each must be durable before external execution or retry.

Container execution receives its deadline from the caller while provider probing retains its own
five-second capability deadline. Terminal-report detection uses a sargable `RunReport@` type range;
an `EXPLAIN QUERY PLAN` regression proves SQLite seeks the complete
`(run_id, causation_id, type, sequence)` index prefix. Materialization decodes paths once and
prepares newly encountered parent components in one sorted serial prologue, so both executor phases
write without a process-wide directory mutex.

On COW filesystems, each CAS source is reflinked into a sibling temporary path, hashed there through
the fixed 64 KiB buffer, and atomically published only after verification; unsupported filesystems
retain the Round 2 streaming-copy fallback. The release measurement recorded 1.269 s for 5,000
distinct entries / 199 MiB and 0.865 s for the same output with repeated content. The
128-distinct-pairs / 256 MiB workload improved from the streaming fallback's 0.876 s to 0.094 s,
with 53,182,464 bytes maximum command RSS and a 36,405,776-byte Darwin peak footprint. Focused
store, source, sandbox, pipeline, and CLI tests pass. Full workspace formatting, clippy, tests,
doc tests, and byte-identical fixture reproduction pass before commit.

## 2026-08-25 — M2 convergence Campaign Round 4 corrections

Round 4 spent 755,609 tokens, retained all 33 prior resolutions, opened five Reports, and exhausted
Campaign `m2-rename-scope-convergence`. Two reviewers independently found that a result refused by
the complete admission gate was retried with an identical prompt. Reviewer inputs now carry a
bounded JSON refusal history under an explicit data-not-instructions heading, and the retry test
proves the second attempt receives the canonical-path failure while the first prompt stays
unchanged.

Read-only template cloning now determines executable bits during its existing discovery walk,
applies final file modes in the reflink batch, and reuses the discovered directory list for the
child-before-parent mode pass. Seal discovery runs one directory level across the shared executor
while the previous level's baseline candidates hash on the same bounded workers. It deliberately
retains non-following metadata lookup: avoiding an lstat is not worth making a symlink race escape
the sandbox boundary. Sorted manifest construction and mutation lists keep completion order out of
durable artifacts.

Materialization validates canonical encoding against the already-decoded bytes without allocating
a re-encoded String. Its serial directory prologue keeps one absolute parent path, pops only the
suffix after consecutive parents diverge, and pushes only newly encountered components. The release
5,000-entry / 199 MiB fixture recorded 1.282 s materialization, 0.652/1.049 s writable/read-only
clone plus permissions, 0.138/0.130 s seal, and 0.154/0.161 s teardown. The prior Round 2 seal
measurement was 0.680/0.693 s. A 5,000-entry baseline plus 10,000 added files sealed in 0.175 s.
Read-only clone time stayed near its prior 1.037 s because the required per-file chmod calls, not
the removed second discovery walk, dominate. Focused runner, pipeline, parallel, source, and sandbox
tests pass. Full workspace formatting, all-target clippy, tests, doc tests, and byte-identical
fixture reproduction pass before commit.

## 2026-08-25 — M2 clean Campaign Round 1 corrections

Fresh unchanged-policy Campaign `m2-rename-scope-clean` Round 1 spent 605,849 tokens and opened
nine Reports. An unreadable Report placeholder no longer counts as both an ordinary open/new
Finding and an authority diagnostic: it remains fail-closed through the authority-failure window,
so a run with no other blocker can durably conclude `authority_unavailable`. Subject Scope replay
reverifies the content-addressed Subject on every Round rather than trusting a prior in-process
parse. Git marks rename linkage truncated only for its exact inexact/exhaustive rename-search
warnings; unrelated successful-command warnings no longer change the Subject contract.

`ReviewerResult@1` now has one semantic validator in `review-core`, used by both persistence and
pipeline admission. Its schema and both conformance corpora reject whitespace-only title, body,
and fix values. ADR-0021 records the permanent flat wire shape. Retry refusal history is now a
bounded CAS artifact named by additive `AttemptInput@1`, atomically published with dispatch and
replayed as exact invocation input. ADR-0022 records why the frozen `AttemptDispatched@1` payload
was not changed. The Change Set prompt bound counts streaming JSON bytes without retaining a
second candidate-sized buffer.

Sandbox clone, direct chmod, sealing, and cleanup share non-following mode helpers. Clone,
permission, and teardown discovery progress level by level while the previous level's file work
uses the same bounded executor; no phase introduces an independent worker pool. The release
5,000-entry / 199 MiB fixture recorded 1.276 s materialization, 0.690/1.102 s writable/read-only
clone plus permissions, 0.133/0.126 s seal, and 0.148/0.119 s teardown. A 5,000-entry baseline plus
10,000 added files sealed in 0.145 s. Focused regressions pass. Full workspace formatting,
all-target clippy, tests, doc tests, and byte-identical fixture reproduction pass before commit.

## 2026-08-25 — M2 clean Campaign Round 2 corrections

Round 2 spent 755,944 tokens, retained all nine Round 1 resolutions, and opened seven Reports.
An active unreadable-report diagnostic now remains an authority blocker after its originating
failure falls outside the ordinary clean window, without double-counting it while that failure is
still recent. Live flat Reports reject whitespace-only paths before their frozen trimmed identity
could alias change-wide scope; frozen projections preserve the readable claim with unknown Scope
and fail closed. Retry refusal history is seeded from durable `AttemptInput@1` state after resume,
and a process-restart regression proves later refusals append instead of truncating that history.

Idempotent CAS puts now compare the existing object to caller bytes through a fixed 64 KiB buffer,
including exact EOF, rather than allocating and hashing a second whole object. Dirty-worktree
fingerprinting runs path work on the shared bounded executor and streams non-publishing regular
files through one worker-local 64 KiB buffer. The admitted publishing pass still revalidates every
path and publishes the exact bytes it read. A release measurement captured 5,000 distinct 40 KiB
files (195.3 MiB) in 1.522 s.

Diff authority is parsed once and shared through the Round, reviewer input, and renderer. Event
admission caches validated Change Sets by content ID but streams and verifies current CAS bytes on
every reference; a corruption-after-cache regression proves process history never substitutes for
integrity authority. Sandbox templates and instances share one immutable `Arc<Manifest>` rather
than cloning all entries per node. The 5,000-entry / 199 MiB sandbox measurement recorded 1.092 s
materialization, 0.831/1.096 s writable/read-only clone plus permissions, 0.136/0.127 s seal, and
0.150/0.142 s teardown. A 5,000-entry baseline plus 10,000 added files sealed in 0.146 s. Full
workspace formatting, all-target clippy, tests, doc tests, and byte-identical fixture reproduction
pass before commit. Rounds 3 and 4 must both be clean for this four-Round Campaign to converge.

## 2026-08-25 — M2 clean Campaign Round 3 corrections

Round 3 spent 839,349 tokens, retained all sixteen Round 1-2 resolutions, and opened eight Reports.
An unreadable Report now remains explicit authority even when its existing Finding was previously
fixed, so it cannot age out or be hidden by the older claim status. Runner Change Set inputs are
private and constructible only from validated bytes or an exact canonical prevalidated value;
production rendering therefore cannot bypass digest, length, schema, or semantic validation.

ADR-0023 separates reviewer feedback from terminal diagnostics through additive
`AttemptFeedback@1`. Contract refusals and retryable timeouts publish their exact accumulated
feedback atomically with the matching terminal event; replay never converts generic error prose
into prompt instructions. `AttemptInput@1` remains the exact input consumed at dispatch. Event
admission validates both artifacts and enforces the feedback event's same-attempt atomicity.

Dirty capture streams regular files directly into temporary CAS objects through one worker-local
buffer, including the publishing pass, instead of allocating one whole file per worker. A release
measurement captured 5,000 distinct 40 KiB files (195.3 MiB) in 1.460 s. Each scan canonicalizes
the repository root once, and CAS verification reuses thread-local 64 KiB scratch. Event append
preparation verifies each referenced artifact once and retains exact typed JSON for semantic
validation; a warmed-cache corruption regression proves cached Change Set semantics never replace
current-byte integrity. Diff Subjects retain a single shared Change Set rather than a second
changed-path collection. Focused regressions and all-target clippy pass. Full workspace formatting,
tests, doc tests, and byte-identical fixture reproduction pass before commit. Round 4 must retain
the corrections; because Round 3 was not clean, a fresh unchanged-policy Campaign must then
establish two clean Rounds before M3.1.

## 2026-08-25 — M2 clean Campaign Round 4 corrections

Round 4 spent 828,444 tokens, retained all 24 prior resolutions, opened eight Reports, and
exhausted Campaign `m2-rename-scope-clean`. Malformed reviewer output now carries its raw CAS
artifact and enters the same bounded durable-feedback correction loop as a semantically invalid
answer; generic execution failures still never become prompt instructions under ADR-0023. A
readable Report clears named unreadable attachments and records `AuthorityRecovered`, so the
fail-closed blocker has a durable recovery path whether the original Finding was a placeholder or
an existing claim.

Full Round authority now uses a private store-owned `ResolvedChangeSet` capability that binds the
verified CAS identity, parsed contract, and exact stored length without demanding byte-identical
re-serialization. A schema-valid explicit `rename_detection_truncated: false` regression proves
the stored bytes remain authority. Ledger Scope has its own resolver and retains only the moved
changed-path set, dropping each Round's potentially multi-megabyte base64 patch after validation.

The dirty capture publishing pass reuses the first pass's digest authority. Cold objects stream
once into a temporary in the expected shard; warm objects hash and verify without any temporary
write. The release 5,000-file / 195.3 MiB fixture recorded 1.496 s cold and 0.526 s warm. Synthetic
Git tree construction streams verified CAS objects directly into `fast-import` through fixed
scratch instead of allocating each file. Change Set publication builds one JSON value, one
canonical byte buffer, bounds that exact representation, and stores those bytes directly.
Focused source, store, runner, adapter, pipeline, and CLI suites pass. Full workspace formatting,
all-target clippy, tests, doc tests, and byte-identical fixture reproduction pass before commit. A
fresh unchanged-policy Campaign must establish two clean Rounds before M3.1.

## 2026-08-25 — M2 verified Campaign Round 1 corrections

Fresh unchanged-policy Campaign `m2-rename-scope-verified` pins the same authority and manifest as
the exhausted clean Campaign. Round 1 spent 777,705 tokens and opened seven Reports. Command
reviewer serialization now carries every typed artifact's ID, contract type, and complete value;
validated Change Sets serialize from their single parsed authority rather than disappearing from
the command document or retaining a second JSON tree. Configuration validation, generation,
reviewer dispatch, invocation receipts, and prompt rendering all discriminate Change Sets and
Prior Findings by artifact type. Renamed-port regressions prove neither contract is load-bearing
on `change_set` or `prior_findings` labels.

An impossible missing prepared Change Set is now a durable store conflict instead of a panic. The
parsed Change Set cache retains only the current Round authority, while every reference still
reverifies current CAS bytes. Directory permission recovery uses atomic no-follow `fchmodat`, so a
reviewer cannot race the earlier metadata check into chmodding a symlink target; unreadable real
directories still recover and seal. Synthetic-tree `fast-import` uses a fixed 256 KiB buffered
writer. Frozen Ledger projection validates Finding claim fields by borrow and checks positive line
bounds separately, eliminating the prior deep clone without weakening historical path handling.
The release 5,000-file / 195.3 MiB fixture recorded 1.463 s cold capture, 0.519 s warm capture, and
0.861 s synthetic-tree construction.

Focused regressions pass. Full workspace formatting, all-target clippy, tests, doc tests, and
byte-identical fixture reproduction pass before commit. Resolve all seven Reports against this
commit, then run Rounds 2 and 3; both must be clean for the Campaign to converge before M3.1.

## 2026-08-25 — M2 verified Campaign Round 2 corrections

Round 2 spent 869,129 tokens, retained all seven Round 1 resolutions, and opened four Reports. A
command reviewer now receives stdin whenever the serialized `ReviewerInputs` document is nonempty;
empty optional fields are omitted, so the document itself rather than a duplicated field predicate
decides delivery. A command regression proves refusal history is delivered even when no prior
Finding or typed artifact is present.

Live legacy and typed Reports now refuse leading or trailing whitespace in locations instead of
normalizing it or projecting it out of diff Scope. Both permanent Rust readers, both JSON schemas,
and both conformance corpora enforce the same rule; the byte-exact Git path predicate remains
unchanged. Frozen historical projection continues to preserve noncanonical claims with unknown
Scope.

Publication preparation now carries every validated Change Set in a batch independently. The
one-entry EventStore map remains only a bounded cross-batch parse memo and is never transaction
validation authority; a two-Change-Set regression prevents order-dependent eviction failures.
`review-core::contract` now owns `RefusalHistory@1`, and `review-store` imports constants for every
artifact type it discriminates rather than duplicating wire strings.

Focused core, runner, store, pipeline, schema-parity, campaign-transition, replay, and scope suites
pass with all-target Clippy warnings denied. Full workspace formatting, Clippy, tests, doc tests,
and byte-identical fixture reproduction pass before commit. Resolve all four Reports against this
commit, then run Rounds 3 and 4; both must be clean for convergence before M3.1.

## 2026-08-25 — M2 verified Campaign Round 3 corrections

Round 3 spent 990,029 tokens, retained all eleven Round 1-2 resolutions, and opened six Reports.
The definition loader now rejects every Generation output outside the two built-in artifact types
and requires an explicit `PriorFindings@1` output before any Campaign, capture, or gate work. Typed
configuration regressions replace the last accepted opaque Generation fixture. Durable Generation
receipt replay also refuses unknown output types instead of skipping Round-authority validation.

`is_valid_repo_path` now owns the complete semantic rule shared by Change Sets and live Report
locations, including leading/trailing whitespace. Change Set, Finding Report, and Reviewer Result
schemas and conformance corpora agree with both permanent Rust readers. A repository path that
cannot be reported with its exact spelling therefore fails closed during Change Set construction,
before paid review, rather than deterministically refusing a correct reviewer answer.

No-follow directory recovery preserves existing permission bits and adds exactly the requested
mode floor; sealing an unreadable directory restores owner read/traverse without granting write.
Command reviewers write stdin and drain stdout/stderr concurrently, run in their own process group,
and use the exact timeout captured in the Campaign Manifest. Regressions force simultaneous 1 MiB
stdin/stderr pressure and kill a hung command at a 100 ms deadline. Both isolated Git input sites
drain stdout/stderr while writing, including the buffered synthetic-tree stream. The release
5,000-file / 195.3 MiB fixture recorded 1.513 s cold capture, 0.545 s warm capture, and 0.861 s
synthetic-tree construction.

Convergence indexes recent Scope-authority failure IDs once in a sorted set instead of scanning
the complete recent-failure list for every unreadable Report. Focused config, core, pipeline,
runner, sandbox, source, store, and CLI suites pass with all-target Clippy warnings denied. Full
workspace formatting, Clippy, tests, doc tests, and byte-identical fixture reproduction pass before
commit. Resolve all six Reports against this commit and run Round 4 to retain them. Because Round 3
was not clean, open a fresh unchanged-policy Campaign afterward for the two-Round clean window.
