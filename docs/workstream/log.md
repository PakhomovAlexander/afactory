# Review Kernel workstream log

## 2026-08-31 — Light Campaigns become the default

The M6.3 review retrospective found that four Campaigns and seven Terra/xhigh Attempts spent
1,380,750 chargeable tokens because the agent repeatedly treated a fresh clean review as implicit
closeout after lightweight dogfood. `af review run` now defaults to `--light`: effective Campaign
authority is one clean/maximum Round, a second closed-Round dispatch is refused, and human/JSON
output says to fix Findings, run the deterministic project gate, and stop. Explicit `--heavy`
retains the pipeline's full convergence policy, must match on resume, and is reserved for a human
request. ADR-0037 records the accepted boundary.

## 2026-08-31 — M6.3 Broker Handles

Pipeline format v4 requires every reviewer to declare `credential_free`, `brokered`, or
`trusted_unsafe`; v1–v3 remain frozen. A brokered Attempt publishes its exact lease and bounded
symbolic operations in `ReviewerExecutionBound@1`, receives only an opaque handle, and keeps the
connector plus reusable credential machine-local. Every operation leaves a secret-free
`BrokerOperationCompleted@1` receipt before any response reaches the Worker. Current Codex and
Claude CLI adapters are honestly `trusted_unsafe`; safe v4 execution needs a broker-capable
adapter and machine-local connector.

The deterministic boundary fixtures cover fixed egress, raw and encoded credential reflection,
fence races, receipt failure, connector errors and panics, usage and call limits, durable terminal
handle replay, missing bindings, crash recovery, and Round supersession. Attempt admission and
budgets reconcile broker charges exactly; uncertain fences reserve the complete checked broker
authority bound or higher observed usage. Late overruns raise terminal settlement; provider
admission sees outstanding broker authority; public revocation synchronizes with in-flight calls;
and a single refusal terminally bounds durable receipt growth. The full local `make check` gate
passes.

Correctness-only Campaign `m6-3-broker-lightweight-v1` spent 178,968 and 165,410 tokens across two
Rounds and found five recovery/security defects. Confirmation Campaign
`m6-3-broker-confirm-v1` first lost a zero-token epoch to restricted npm DNS, then spent 177,647
and 219,510 tokens and found eight further defects. All thirteen have deterministic regressions
and are fixed. Campaign `m6-3-broker-final-v1` then spent 205,663 and 183,633 tokens, exhausted,
and found five more concurrency/accounting defects; all five now have deterministic regressions
and are fixed. These v2 Campaign ledgers retain old open rows when the frozen pipeline omits exact
prior-Finding dispositions. Final correctness Campaign `m6-3-broker-final-v2` spent 249,919 tokens
and found a partial percent-encoding credential bypass plus Broker authority exceeding the
pre-dispatch reservation. Recursive mixed literal/percent response scanning and config-load
authority/budget validation fix both with regressions. Per owner direction, the final full
`make check` gate is the closeout: it passes, M6 is complete, and M7.1 is next.

## 2026-08-31 — M6.2 bounded Cache Snapshots

Pipeline v3 Gates may now request the symbolic `cargo` cache kind. Machine-local policy resolves
that request to one curated root and hard byte, filesystem-entry, and copy limits; project
authority never contains the host path. Descriptor-relative no-follow traversal admits only
Cargo package archives and sparse-index files, then one preflighted reflink-or-copy method seeds
the private Gate clone. The Gate runs offline, and the cache subtree is removed before Subject
sealing.

`CacheManifest@1` freezes the admitted layout and kernel ceilings. `RunReport@5` covers every
requested Gate/cache pair with a typed path-free failure or a manifest receipt that publication
rehashes, validates, and cross-checks even for incomplete reports. Completed Gate replay is lazy
with respect to machine policy. macOS uses descriptor-native `fclonefileat`, strips
source-controlled xattrs, named forks, and ACLs before dispatch, and fixes file and directory
modes. Focused regressions and the full `make check` gate pass at final candidate `980f761`.

Codex-only Campaign `m6-2-cache-candidate-codex` spent 281,585 and 259,944 tokens across two
Rounds, exhausted, and left ten fixed Findings with zero open. Fresh Codex-only Campaign
`m6-2-cache-final-codex-v1` spent 305,220 tokens in Round 1, opened three further Findings, and
confirmed all three fixed in a 335,930-token Round 2 `Pass`. Opus/Claude packages remain available
but are not active reviewers. Resume at M6.3 Broker Handles.

## 2026-08-30 — M6.1 Gate Execution Bindings

Pipeline format v3 now requires explicit Gate provider, required isolation, and
`ephemeral-write` mode. Every root Gate materializes through its provider, admits provided
isolation before command dispatch, runs in an independent writable clone, and publishes the
exact fact in structural `RunReport@4`; v1/v2 retain their frozen local read-only contract.
Container live probes exercise the same typed `CheckRunner` route and compile locally, but the
machine had no running Docker daemon, so only the trusted-local/none path was executed here. A
dedicated CI job now runs provider containment controls, the live v3 Gate route, timeout reaping,
and host-ownership assertions; it remains pending until this product branch is published.

Disposable Campaign `m6-gate-binding-dogfood-fixed` ran project-hub's write-heavy scaffold and
update smoke checks, then two machine-configured Codex Workers, and returned Pass with 30,060
tokens, zero Findings, and zero Demands. Its report records an admitted
`trusted_local`/`none`/`ephemeral-write` binding, and the candidate checkout retained no Gate
mutations. Dogfood also exposed and fixed generated Codex manifests incorrectly repeating the
adapter-owned `exec`/sandbox/stdin flags. Focused regressions and the full local `make check` gate
pass.

Pinned Campaign `m6-1-gate-bindings-v1` spent 202,163 and 180,236 tokens across its two Rounds,
exhausted, and left five fixed Findings with zero open. Follow-up Campaign
`m6-1-gate-bindings-final-v1` spent 217,695 and 174,419 tokens, exhausted, and left another five
fixed Findings with zero open. Those reviews added replayable append-only binding observations,
digest-pinned project images, provider-usability admission, durable Gate mutation summaries, an
executed recording-runtime route, a real Docker CI target, portable environment forwarding, and
bounded timeout reaping that refuses to seal an unreaped writable bind.

Fresh final Campaign `m6-1-gate-bindings-final-v2` spent 223,179 tokens in Round 1 and 250,781 in
Round 2 and returned Pass. Its four Findings are fixed with zero open: the image retains its own
`PATH`, container writes use the caller's UID:GID, preserved unsafe sandboxes report their path,
and the Unix-specific argv assertion is Unix-gated. Final candidate `6a22aaa` passes `make check`.
Resume at M6.2 sandbox-local Cache Snapshots.

## 2026-08-28 — M5.3 Campaign enumeration

`af review campaigns` now renders deterministic text or `af/review-campaigns@1` JSON containing
each Campaign's opaque ID, validated human label, pinned Subject/authority summary, last closed
Round/verdict, and full closed-Round history. ADR-0035 separates the human-label and filesystem-ID
namespaces, verifies legacy state ownership, proves root containment, and keeps existing labels
readable without changing persisted kernel contracts. Enumeration opens existing SQLite/CAS state
without creating storage, refuses symlinked durable state, and isolates malformed entries in an
explicit `problems` projection so healthy Campaigns remain visible.

Pinned Campaign `m5-campaign-enumeration-v1` spent 145,666 tokens in Round 1 and 121,475 in Round 2,
then exhausted with seven Findings; all seven are fixed and its ledger has zero open. Fresh pinned
Campaign `m5-campaign-enumeration-final-v1` spent 191,407 tokens and returned Pass. Its four
follow-up Minors are also fixed: legacy-label symlinks are reported, ambiguity presentation matches
direct command refusal, stale legacy-state errors name their path and role, and the live roadmap now
resumes at M6.1. Focused regressions and the full `make check` gate pass.

## 2026-08-28 — M5.1/M5.2 operator reports and spend

`af review report` now renders one versioned operator projection as Markdown, explicit text, or
JSON. It exposes every Round epoch, terminal verdict, selected/fenced/failed/released/outstanding
Attempt, Provider Operation, and per-reviewer token total. The exact-Round budget query now reads
admitted `cost_tokens`, counts only the first terminal Attempt lifecycle event, and retains crash
reservations. Focused projection, durable-query, end-to-end format tests, the complete `reviewctl`
suite, and focused Clippy pass. Resume at M5.3 safe Campaign enumeration and history.
Pinned Campaign `m5-report-spend-v1` Round 1 spent 149,528 tokens and found one Major and one
Minor. Their corrections keep the Markdown spend table contiguous, render Attempt detail in a
separate section, and preserve causation-less frozen `RunReport@1` presentation with an explicit
run ordinal and absent Round authority. The focused regressions and full kernel gate pass again.
Round 2 spent 164,893 tokens, confirmed both corrections, and returned Pass. It also opened one
Minor: the Provider preflight budget seed counted only the active epoch although kernel replay
charges the entire superseded Round lineage. The query now derives the active Round number and
Campaign Manifest, includes every matching epoch, and has a two-epoch regression.

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

## 2026-08-25 — M2 verified Campaign Round 4 corrections

Round 4 first stopped incomplete after 819,042 tokens when a performance review returned truncated
JSON. Resuming the exact captured Round and Snapshot completed at 1,179,641 tokens, retained all
seventeen Round 1-3 resolutions, opened four Reports, and exhausted Campaign
`m2-rename-scope-verified`.

Generation validation now derives its Change Set contract from Subject kind: a whole-tree pipeline
cannot declare `ChangeSet@1`, while a diff pipeline must declare exactly one. Typed Reports with any
noncanonical location keep readable claim content but project their entire Scope as unknown; they
can no longer discard a bad location and project from a valid subset.

Git paths with leading or trailing whitespace use their exact percent-encoded wire spelling, so a
legal repository filename remains capturable, diffable, reportable, and materializable without
weakening live Report path validation. Unit, committed-capture, materialization, and tree-diff
regressions cover the boundary. Command reviewer input delivery now shares the captured attempt
deadline: after the parent exits, its process group is reaped and a descendant retaining stdin
cannot make the writer join unbounded.

Focused config, core, runner, source, and store tests pass with all-target Clippy warnings denied.
Full workspace formatting, Clippy, tests, doc tests, and byte-identical fixture reproduction pass
before commit. Resolve all four Reports against this commit, then open a fresh unchanged-policy
Campaign and require two clean Rounds before M3.1.

## 2026-08-25 — M2 final-verified Campaign Round 1 corrections

Fresh unchanged-policy Campaign `m2-rename-scope-final-verified` reproduced the exact authority
and Campaign Manifest digests from the preceding Campaign. Round 1 spent 826,568 tokens and opened
nine Reports: three major and six minor.

Command attempts now decide a known timeout before pipe collection, never signal a process-group
number after `try_wait` has reaped its leader, and bound lingering-descendant input delivery by the
captured deadline. The already-owned serialized command document moves directly into its writer
thread. Change Set size admission occurs inside both runner-owned constructors, including the
store-resolved Round-authority path, and `RoundAuthority` retains only the store's verified Change
Set capability rather than a runner presentation value.

Manifest path spelling now has an explicit compatibility generation. Missing generation means
the permanent legacy alphabet; new captures emit `percent_v2` only when required. Both forms
materialize, validation remains allocation-free, and identity normalization preserves the same
raw-tree digest across generations and their differing sort order. ADR-0024 records the decision.
ADR-0025 records the intentional version-2 rule that built-in Generation outputs must use typed
artifact contracts because opaque outputs cannot be dispatched.

Scope replay borrows and discards the inline Change Set patch instead of allocating it. Round
preparation builds one run-bound Ledger projection and carries that private capability through
prior-row generation, Generation advancement, gather ingest, CLI presentation, and convergence;
projection-affecting events alone invalidate it, and a cross-run projection is refused.

Focused config, runner, source, store, pipeline, and CLI suites pass with all-target Clippy
warnings denied. Full workspace formatting, Clippy, tests, doc tests, and byte-identical fixture
reproduction pass before commit. Resolve all nine Reports against this commit and run Round 2 to
retain them. Rounds 3 and 4 can still establish the required two clean Rounds if both are clean;
only another finding-bearing Round would require a fresh Campaign.

## 2026-08-25 — M2 final-verified Campaign Round 2 corrections

Round 2 reproduced the unchanged authority and Campaign Manifest, spent 912,203 tokens, retained
all nine Round 1 fixes, and opened seven Reports: two major and five minor.

Sandbox seal now encodes every discovered raw path in the baseline Manifest's generation and
constructs its final Manifest with that same generation; a legacy baseline with literal spaces and
escaped percent bytes seals byte-identically. Ordinary Manifest paths without `%` bypass decode and
re-encode validation. Replay uses streaming CAS verification when it needs integrity rather than
artifact bytes.

A normally exited command gets a fixed five-second output-drain grace. A descendant that still
holds a pipe now produces an observable unavailable result instead of fabricated empty evidence,
while a regression proves that a one-megabyte valid answer is preserved. Durable retry feedback
records fixed `parse_error` or `contract_error` classes, never reviewer-controlled diagnostics,
and command reviewers enforce the same refusal-history bound as model reviewers.

Claude and Codex append resolved inputs directly to one owned prompt buffer and move its bytes to
the writer thread. Kernel ledger and convergence reads borrow the cached run-bound projection;
gather takes that capability without deep clones, and projection rebuilding never nests the ledger
cache lock over the event-store lock.

Focused core, config, runner, source, sandbox, store, pipeline, and CLI suites pass with all-target
Clippy warnings denied. Full workspace formatting, Clippy, tests, doc tests, and byte-identical
fixture reproduction pass before commit. Resolve all seven Reports against this commit and run
Round 3. If Rounds 3 and 4 are both clean, the Campaign satisfies the two-Round clean window.

## 2026-08-25 — M2 final-verified Campaign Round 3 corrections

Round 3 spent 969,363 tokens, retained all sixteen Round 1-2 fixes, and opened five Reports: one
major and four minor.

Reviewer input resolution now owns one explicit pre-adapter lifecycle: any contract lookup, CAS
read, size check, Change Set validation, JSON parse, adapter lookup, or retry-history read failure
releases the prepared reservation and appends `AttemptReleased@1`. A regression drives an
oversized authoritative Change Set through the scheduler and proves one dispatch has one release,
zero committed spend, and no adapter invocation.

Synthetic Git tree construction checks each CAS object's stored length against the Manifest before
writing `fast-import` framing, then streams and digest-verifies the same object. A command-shaped
surplus regression is refused before Git can parse it. The scope-only Change Set reader has parity
coverage that serializes every current `ChangeSet@1` field, including the normally omitted
rename-truncation flag, and requires the borrowed reader to accept it.

Consumed `Ingest` values move their run-bound Ledger into `LedgerProjection` rather than cloning it.
Round preparation folds the Campaign events it already loaded through `LedgerProjection::from_events`;
ordinary projection rebuild delegates to the same fold after one replay.

Focused store, source, pipeline, and CLI suites pass with all-target Clippy warnings denied. Full
workspace formatting, Clippy, tests, doc tests, and byte-identical fixture reproduction pass before
commit. Resolve all five Reports against this commit and run Round 4 to retain them. Because Round
3 was not clean, open a fresh unchanged-policy Campaign afterward for two clean Rounds.

## 2026-08-25 — M2 final-verified Campaign Round 4 corrections

Round 4 spent 1,032,208 tokens, retained all 21 Round 1-3 fixes, opened five minor Reports, and
exhausted Campaign `m2-rename-scope-final-verified`.

On a cold streaming CAS write, the second read is now the authoritative publication pass. If a
worktree source changes between reads, its observed bytes are filed under their actual digest and
the capture boundary compares unequal, retries, and never misreports ordinary instability as CAS
corruption. Deterministic CAS-reader and new-untracked-file capture regressions cover both layers.

A child observed exiting near its command deadline gets 500 ms for the stdin writer to observe
closure; a blocked descendant still exceeds that grace and remains a charged timeout. Pipeline
format version is carried into kernel execution: only version 1 may interpret opaque Generation
ports named `findings` or `change_set`, restoring its name-keyed compatibility without weakening
version 2's typed contract. Load and end-to-end delivery regressions cover the boundary.

`EventStore` reads the unique Campaign opening and latest Round start through the existing
type/sequence index, retaining full event-id, payload, and artifact-reference validation without a
whole-log replay. Resume preparation passes its already-loaded event slice through round counting
and Ledger projection; only a newly opened Campaign performs the one post-open replay it needs.

Focused config, runner, store, source, pipeline, and CLI suites pass with all-target Clippy warnings
denied. Full workspace formatting, Clippy, tests, doc tests, and byte-identical fixture reproduction
pass before commit. Resolve all five Reports against this commit, then open a fresh unchanged-policy
Campaign and require two clean Rounds before M3.1.

## 2026-08-25 — M2 clean-window Campaign Round 1 gate correction

Fresh unchanged-policy Campaign `m2-rename-scope-clean-window` pins the same authority and policy.
Round 1 epoch 1 stopped at the pre-dispatch gate before any reviewer ran, so it spent zero tokens
and remains incomplete. Under full sandbox-suite load, the command-runner regression raced a
100 ms process deadline against an 80 ms sleep instead of testing the intended post-exit invariant.

The regression now asserts directly that an already-expired child deadline still grants the fixed
500 ms stdin-writer grace. The focused runner suite and full workspace formatting, Clippy, tests,
doc tests, and byte-identical fixture reproduction pass. Commit the correction, then restart the
same incomplete Round epoch; no reviewer Round was consumed.

## 2026-08-25 — M2 clean-window Campaign Round 1 corrections

Restarted Round 1 epoch 2 spent 956,628 tokens and opened three major and four minor Reports.
Version-1 pipelines again distinguish the historical Generation output `findings` from reviewer
input `prior_findings`; delivery and durable dispatch references are pinned end to end.

One subprocess supervisor now owns exact child-exit waits, process-group killing, bounded input and
output handling, and partial timeout evidence for model, command, check, and container execution.
An explicit exit policy preserves the gate contract that reaps successful background work while
reviewers refuse held pipes instead of fabricating empty evidence. Model-specific regressions cover
both a descendant holding stdin and one holding stdout.

Retry feedback carries a stable closed rejection code generated only by the kernel. Change Set and
prior-Finding limits live with the core contracts and are enforced by bounded CAS reads before
allocation. Ledger projection rebuilds remain under the cache lock, so an invalidating append
cannot race an older projection back into the cache. Synthetic Git tree construction opens each
CAS object once and uses that handle for both framing length and verified streaming.

Focused core, store, source, runner, check, sandbox, pipeline, and CLI suites pass with all-target
Clippy warnings denied. Full workspace formatting, Clippy, tests, doc tests, and byte-identical
fixture reproduction pass. Commit and resolve all seven Reports, then run Round 2; two clean Rounds
are still required before M3.1.

## 2026-08-25 — M2 clean-window Campaign Round 2 corrections

Round 2 spent 973,217 tokens, retained all seven Round 1 resolutions, and opened two major and
three minor Reports. Model and command adapters now distinguish pre-spawn unavailability from
post-spawn lifecycle failure so only work that never executed releases its reservation.

Bounded subprocess lifecycle moved from the reviewer layer into dependency-neutral leaf crate
`review-process`; runner, gate, and sandbox consumers depend on it directly while preserving their
own result semantics. Proposed ADR-0026 records the dependency boundary and rejected alternatives.
Warm Subject Scope caches reverify both the Subject and its referenced Change Set before reuse.

Ledger projections now carry the exact event-log position they cover. Reuse rejects a different
run or stale watermark, every appended event invalidates the kernel cache, and Round capture folds
all newly appended events in sequence. Regression coverage catches stale reuse after a log append,
gapped projection input, and the full campaign handoff. Sandbox seal, clone, permission, and
writable-directory walks parallelize entries inside one wide directory as well as directory tasks.
The 14-worker release measurement seals 5,000 baseline entries plus 10,000 files in one directory
in 0.154 s; unchanged 199 MiB sandboxes seal in 0.135/0.131 s.

Focused process, runner, check, sandbox, store, pipeline, and campaign-loop suites pass with
all-target Clippy warnings denied. Full workspace formatting, Clippy, tests, doc tests, and
byte-identical fixture reproduction pass. Commit and resolve all five Reports, then run Round 3;
Rounds 3 and 4 must both be clean for this Campaign to converge before M3.1.

## 2026-08-25 — M2 clean-window Campaign Round 3 corrections

Round 3 spent 1,020,400 tokens, retained all twelve prior resolutions, and opened three major and
three minor Reports. Watermarked Ledger projections now fast-forward through the exact durable log
suffix after provider or crash-recovery events, while ordinary kernel appends fold into the cached
projection instead of forcing whole-log replay. Dense ordering and ahead-of-log refusal remain.

The `review-process` leaf now owns borrowed streaming stdin and duplex protocols as well as buffered
execution. Every Git capture subprocess, including `cat-file --batch` and streaming `fast-import`,
uses its process group and five-minute default deadline. A fake wedged Git is killed at 100 ms.
Complete reviewer stdout survives a descendant holding only stderr; the held diagnostic remains
visible. Timed-out model and command partial output is named by the durable fence event.

Campaign transition validation caches the active typed Subject across one append batch and refreshes
it only when `RoundStarted@1` changes authority. The 5,000-object / 195.3 MiB release measurement
records 1.752 s cold capture, 0.646 s warm capture, and 0.844 s synthetic-tree construction.

Focused supervisor, runner, source, store, pipeline, and campaign-loop suites pass with all-target
Clippy warnings denied. Full workspace formatting, Clippy, tests, doc tests, and byte-identical
fixture reproduction pass. Commit and resolve all six Reports, then run Round 4. Because Round 3
was not clean, this Campaign cannot converge; open a fresh unchanged-policy Campaign after it
exhausts and require two clean Rounds before M3.1.

## 2026-08-25 — M2 Round 4 corrections and bounded review policy

Round 4 spent 1,053,936 tokens, retained all eighteen prior resolutions, opened three major and
four minor Reports, and exhausted `m2-rename-scope-clean-window`. Gate and Git subprocess
deadlines are now explicit Campaign authority. Run-budget exhaustion crosses the scheduler as a
typed failure class and persists `fail{exhausted}` without parsing diagnostic prose. Held stderr
makes a check explicitly unverifiable, raw patches are refused before base64/JSON amplification,
Ledger projection uses its reducer as the sole input vocabulary, and store transitions load the
Campaign plan only for events that consume it. Focused regressions and the full local formatting,
warnings-denied Clippy, tests, doc tests, and byte-identical fixture gate pass. Commit `d8f0812`
records the corrections; all seven Reports are fixed and the Campaign closes with 25 fixed
findings and zero open.

The owner explicitly replaced the standard two-specialist policy after this Campaign consumed
about four million tokens. New Campaign authority uses one Claude Opus correctness reviewer at
high effort, a 300,000-token attempt reservation, a 1,000,000-token run cap, one required clean
Round, and a two-Round maximum. Architecture-only and performance-only audits are opt-in. The
review package remains digest-locked, and ADR-0027 records the accepted coverage/cost tradeoff.
The exhausted Campaign is immutable and will not receive another old-policy Round.

## 2026-08-26 — Minimal v1/v2 before candidate dogfood

The owner superseded the compatibility-backed A0 checkpoint after its first implementation showed
that it would add a substantial temporary path before the target runtime. ADR-0030 now requires
minimal local-review v1, then sequential-implementation v2 ending at a verified internal Snapshot,
then the first real candidate dogfood. Delivery, parallelism, optional integrations, and physical
internal renaming move to v3. The A0 experiment remains isolated and does not enter this branch.

## 2026-08-26 — Minimal product v1/v2 complete

Final local `af review` now loads digest-pinned `.af` authority, defaults state outside the
repository, supports the non-interactive shorthand, and emits one typed outcome with candidate,
authority, node, Finding, exact Worker-context, and available Provider-token receipts. The frozen
`.review/` path remains available only when explicitly selected.

`af task start --kind implement` now executes one locked implementer, seals and publishes every
derived-tree byte into CAS, runs required Gates against fresh read-only materializations, then
gives a separate locked evaluator only the goal, derived Snapshot/diff, Gate evidence, authority,
and reservation. It returns typed verified/unverified results with explicit no-delivery evidence;
deterministic integration tests prove the verified path, the gate-failure path, materialization,
minimum evaluator context, and no source-checkout mutation. While closing the full gate, the
dirty-capture comparison gained metadata change stamps so a rapid change-and-restore remains
detectable even when macOS FSEvents coalesces both writes. `make check` passes in full. The first
real candidate implementation dogfood is next.

## 2026-08-26 — First v2 candidate implementation dogfood verified

Candidate commit `caab486` (`af` binary
`sha256:f0c4bc5d0ea8e5ed15ec0f683fe65f88c98488761fd8ad66e8426e62ee6b17a6`) ran Task
`task-8732de714edb4550a628` against exact source Snapshot
`sha256:9fd9d7e79c1b9c82ab4b7830b8ac579c924d3a46d0faaad7d87603b13b042854`. The real goal was the
minimal local `make dogfood` entry point. The implementer changed only `Makefile`, `README.md`, and
new executable `scripts/check-dogfood-target.sh` inside its private sandbox, producing derived
Snapshot `sha256:46fd82a719d67341d4ddd95b32fd1dbd38fa94fd1ccd1a141020e061d4c2dc8f`.

The required read-only `make check` Gate passed with result artifact
`sha256:df4552a9c040ca58b12b8dbff34be394575ba7b394306a6eb1fa98e19045701c`. The independent
evaluator received no implementer transcript, approved the Snapshot, and its verdict is artifact
`sha256:8e69f20e0504fca2009a4d7ecdd84542d504af99e8dc3abda917f7002cb32269`. Exact Worker context
totalled 3,306 rendered bytes (827 estimated tokens). Provider receipts totalled 4,501,584 input,
26,724 output, 4,301,312 cache-read, 16,340 reasoning, and 226,996 chargeable tokens. The typed
outcome is `verified`, delivery is explicitly `none`, and the source checkout remained clean. The
complete seven-event SQLite journal and its 317 CAS objects are preserved under XDG state at
`~/.local/state/af/dogfood/task-8732de714edb4550a628/`. The verified `make dogfood` change remains
an internal Snapshot; integrating it is v3 delivery work, not part of this gate. M3.1 is next.

## 2026-08-26 — M3.1 canonical identity locally verified

New Campaign manifests now select `report-derived@1`; existing manifests retain permanent
`legacy-path-title@1` replay. The canonical adapter publishes each selected flat result as a
validated `FindingReport@1` envelope with exact Attempt, Subject Snapshot, and invocation inputs.
The reducer derives new Finding IDs from semantic Report artifact IDs, attaches only by an exact
trusted occurrence key or explicit corroboration, and never collapses identity through disputes.

Every canonical ledger barrier now publishes a validated, Subject-bound `FindingSet@1`. Its
envelope names the prior Set and selected Report inputs, its kernel-operation producer binds the
reducer and policy versions, and the graph passes the exact artifact ID rather than ambient Ledger
state. Typed envelopes are directly CAS-addressable by artifact ID while their payload remains
deduplicated by content ID. Immutable disposition relation/resolution artifacts remain M3.2;
their FindingSet input lists are intentionally empty in this minimal milestone.

Focused identity, provenance, replay, schema-parity, CAS-addressing, and end-to-end pipeline tests
pass. The full pinned Rust formatting, warnings-denied Clippy, workspace test, doc-test, and
byte-identical fixture gate passes via `make check`. The pinned external correctness review was
not started because transmitting the private diff requires explicit approval; no code or model
tokens were disclosed or spent.

After approval, pinned `af v0.1.0` and then `v0.2.0` both refused the checked-in pipeline before
reviewer dispatch. The main config loader accepts `check_timeout_seconds = 3600`, but the store's
second authority-only TOML reader omitted that field. The M3.1 branch now mirrors it as a typed
optional integer, and the campaign-authority fixture includes the explicit timeout. The focused
suite and full `make check` gate pass. The released v0.2 bootstrap reviewer can use a scratch
pipeline projection that omits only this explicit value: 3600 is that release's exact built-in
default, so reviewer, budget, gate, timeout, and convergence authority remain unchanged. Both
failed preflights spent zero model tokens and disclosed no code.

The disclosure gate then refused model dispatch because the owner's earlier approval covered
`6bfaa13..f65f028`, not the expanded `6bfaa13..3e4e5c3` diff or its scratch compatibility
pipeline. External convergence therefore awaits explicit approval for that exact payload. This
refusal also spent zero model tokens and disclosed no code.

## 2026-08-26 — M3.1 external review corrections locally verified

The approved pinned Campaign `m3-1-canonical-identity-v4` used release `v0.2.0`, one Claude Opus
correctness reviewer at high effort, the exact release-compatible authority projection, and Base
`e792399`. Round 1 spent 247,296 chargeable tokens and opened seven Findings. Their corrections
make selected-result provenance name the actual reviewer node, tolerate unreadable canonical
Reports during replay, persist confirmation as corroborating evidence, distinguish unrecorded
locations, reject unknown identity policy, bound fallback CAS reads, and require explicit
FindingSet lineage.

Round 2 spent 240,532 chargeable tokens. It retained six Round 1 fixes, kept the legacy-confirm
prompt defect open, and opened five more Findings. Their corrections render policy-specific
confirmation instructions, ignore unusable confirmations without discarding other evidence,
recover canonical lineage across an exhausted Round with no emitted Set, assign identical result
digests one-to-one to reviewer nodes, preserve replay while marking unreadable Campaign authority
unavailable, and represent an unrecorded location without schema-invalid empty strings.

Commits `8205ecb` and `b547e7a` fix all twelve Campaign Findings. The final Ledger has twelve fixed
and zero open, and the full formatting, warnings-denied Clippy, workspace tests, doc tests, and
byte-identical fixture reproduction pass. The Campaign verdict remains honestly `Exhausted`
because its two-Round ceiling was reached; it did not converge. M3.1 therefore requires a fresh
pinned Campaign before the milestone can close.

## 2026-08-26 — M3.1 v5 review corrections locally verified

Fresh pinned Campaign `m3-1-canonical-identity-v5` reviewed the corrected candidate against Base
`e792399` with release `v0.2.0` and the same one-reviewer correctness policy. Round 1 spent 577,185
chargeable tokens and opened three Findings. Commit `3a97c50` binds FindingSet round numbers to
Round authority, scopes identical result provenance to the ledger's graph closure, and serializes
absent effective severity as explicit `null`.

Round 2 confirmed all three fixes, spent 291,272 chargeable tokens, and opened two Findings.
Commit `ea10d3e` makes unreadable prior FindingSet lineage fail closed on compatible untyped ledger
ports and permits a declared optional gather input to remain unwired without weakening the
leftover-artifact check. The Campaign spent 868,457 tokens in total. Its Ledger has five fixed
Findings and zero open, and the complete `make check` gate—including byte-identical fixture
reproduction—passes.

The v5 verdict remains honestly `Exhausted`: reaching the immutable two-Round ceiling is not
convergence, even when every reported Finding is subsequently fixed. M3.1 stays open until a fresh
pinned Campaign returns the required clean Round.

## 2026-08-27 — M3.1 v6 Round 1 corrections locally verified

Fresh pinned Campaign `m3-1-canonical-identity-v6` reviewed candidate `c6e6af7` against Base
`e792399` with release `v0.2.0`, one Claude Opus correctness reviewer at high effort, a 300,000
token Attempt reservation, and the one-million-token Run cap. Round 1 spent 346,086 chargeable
tokens and opened one major and three minor Findings.

Commit `517e0f8` makes canonical lineage inspect every pinned ledger output port and fail closed
when a ledger receipt yields no FindingSet, closes FindingSet validation and Campaign Manifest
schema parity, and records the deliberate M3.2 boundary for reviewer/gate Set wiring. A direct
M3.1 wiring experiment was rejected by the Campaign-loop gate: immutable Sets do not yet carry
post-publication operator resolutions, so feeding them back now resurrects fixed/rejected claims.
The `PriorFindings@1` compatibility projection remains until M3.2 makes dispositions immutable
Set inputs.

The v6 Ledger has four fixed Findings and zero open. Formatting, warnings-denied Clippy, all
workspace tests and doc tests, and byte-identical fixture reproduction pass. Round 1 is honestly
`Fail(NotConverged)`; Round 2 still must confirm the fixes and supply the required clean Round.

## 2026-08-27 — M3.1 v6 Round 2 corrections locally verified

Round 2 reviewed candidate `e4247a4` against Base `e792399`, retained all four Round 1 fixes,
spent 285,501 chargeable tokens, and opened one major and one minor Finding. Commit `93cef0d`
anchors canonical lineage to a superseded epoch's exact FindingSet when that epoch reached its
ledger barrier, and makes confirm synthesis reuse the exact previously published corroborating
Report when a canonical barrier resumes. Regression tests pin both event-sequence lineage and
byte-stable Report inputs with no duplicate event append.

Campaign v6 spent 631,587 tokens in total. Its Ledger has six fixed Findings and zero open, and
the complete formatting, warnings-denied Clippy, workspace tests, doc tests, and byte-identical
fixture gate passes via `make check`. The verdict remains honestly `Fail(Exhausted)`: fixes made
after the immutable two-Round ceiling cannot retroactively create a clean Round. M3.1 therefore
remains open until a fresh pinned Campaign returns Pass.

## 2026-08-27 — Verified local Task delivery implemented and locally green

ADR-0031 accepts the first narrow v3 slice: a verified Task may be delivered only after exact
Task-ID confirmation to a new local branch and linked worktree. Commit `fdaf37f` implements the
transition without checkout filters or remote operations. It revalidates the clean target against
the Task's committed source Snapshot, verifies every derived CAS object, reserves an atomic
ownership ref and branch, creates a no-checkout worktree, materializes and re-reads the exact
derived Manifest, and persists prepared plus terminal delivery artifacts. A separate SQLite
process lock serializes delivery while leaving prepared recovery state durable across a crash.

The deterministic pilot suite proves verified and unverified terminals, source-checkout
isolation, ignored-file delivery, explicit no-remote receipts, list/show spend and history,
idempotent exact repeat, crash reconciliation after materialization, concurrent-command refusal,
dirty-source refusal, and owned-ref rollback on local creation failure. The full `make check` gate
and `make pilot-check` pass. The pilot runbook covers checksummed private installation/update,
Provider and authority setup, operator inspection, recovery, troubleshooting, and binary rollback.
Pinned external review and one real trusted-repository pilot remain required before design-partner
handoff.

## 2026-08-27 — Real V3.1 delivery pilot passed after index correction

The release candidate delivered the existing verified v2 dogfood Task
`task-8732de714edb4550a628` from source Snapshot
`sha256:9fd9d7e79c1b9c82ab4b7830b8ac579c924d3a46d0faaad7d87603b13b042854` to derived Snapshot
`sha256:46fd82a719d67341d4ddd95b32fd1dbd38fa94fd1ccd1a141020e061d4c2dc8f`. The first real
delivery exposed that `git worktree add --no-checkout` also creates an empty per-worktree index:
the filesystem bytes were exact, but ordinary Git status presented the source as staged deleted
and the derived tree as untracked.

Commit `41c085d` preserves the filter-free boundary while populating that index from immutable
`HEAD` with plumbing-only `git read-tree`. Exact-delivery verification now also rejects an index
that differs from the source tree, and the integration suite pins ordinary unstaged presentation.
The complete `make check` gate passes, including formatting, warnings-denied Clippy, workspace
and doc tests, and byte-identical fixture reproduction.

After repairing only the disposable pilot index with that same plumbing step, the rebuilt
candidate replayed the exact request idempotently. It returned receipt
`delivery-b6ba56b39cac1793e7c96f160b82f265e4630eac6e497bfb64a1211fd5365aa6`, named the exact
source and derived Snapshots, and recorded `remote_actions: []`. Git status showed only the two
modified files and one new script produced by the verified Task; no source deletion was staged.
The delivered worktree's `make dogfood-contract` passed. Pinned external correctness review is
the only remaining V3.1 handoff gate.

## 2026-08-27 — Recovery preserves operator work

A requirement audit against ADR-0031 found that prepared-state recovery verified ownership refs,
branch, HEAD, and repository identity before `git worktree remove --force`, but did not prove that
the operator had left the delivered filesystem and index untouched. A crash followed by a human
edit could therefore make exact verification fail and then lose that edit during rollback.

Commit `09853ac` separates rollback of the still-running creation attempt from later crash
recovery. Both paths require unchanged owned refs, branch, HEAD, repository identity, and a source
index. The current attempt may remove only bytes that are an exact subset of its derived Manifest;
recovery may remove only an empty worktree and otherwise fails closed. Regression tests simulate
modified bytes, deletion of a derived file, and staged index changes after the terminal receipt is
lost; every case retains the branch, worktree, and prepared event. The focused eight-test delivery
suite and the complete `make check` gate pass.

## 2026-08-27 — M3.1 v7 passed and milestone closed

Fresh pinned Campaign `m3-1-canonical-identity-v7` reviewed candidate `9d8e0e0` against Base
`e792399` with release `v0.2.0` and one high-effort Claude Opus correctness reviewer. Round 1
spent 346,321 chargeable tokens and returned Pass with three minor Findings. Commit `bff36f4`
makes `effective_severity` presence-required, enforces coherent canonical FindingSet locations,
and binds non-reviewer gather inputs through validated graph provenance and durable outputs. The
v7 Ledger has three fixed Findings and zero open, and the full `make check` gate passes. M3.1 is
complete; resume at M3.2.

## 2026-08-27 — V3.1 external Round 1 corrections locally verified

Pinned Campaign `v3-1-client-pilot-v2` reviewed candidate `6e92d8a` against Base `9d8e0e0` with
release `v0.2.0` and the same one-reviewer policy. Round 1 spent 213,903 chargeable tokens and
opened one major and three minor Findings. Commit `f11a09f` recovers an empty unpopulated
per-worktree index without weakening operator-staging or filesystem guards, keeps sealed receipts
inspectable after operator use without re-attesting current bytes, aborts the runbook installation
on checksum failure, and records losslessly encoded ignored Snapshot paths in the delivery
receipt and operator guidance.

All four Findings are fixed and the Campaign Ledger has zero open. The integrated candidate also
contains the verified M3.1 corrections. Formatting, warnings-denied Clippy, all workspace and doc
tests, byte-identical fixture reproduction, the full nine-test pilot suite, and markdown lint pass.
A confirming external Round remains required before V3.1 closes.

## 2026-08-27 — V3.1 Round 2 exhausted; all Findings fixed

Round 2 of pinned Campaign `v3-1-client-pilot-v2` reviewed integrated candidate `f0c13e7`, carried
all four fixed Round 1 Findings, spent 167,787 chargeable tokens, and opened one major plus two
minor Findings before the immutable two-Round ceiling exhausted. It found that a crash during
materialization could leave an ambiguous partial worktree permanently prepared, ignore
classification omitted the operator's global excludes, and the runbook passed percent-encoded
receipt identifiers to `git add` as literal paths.

Commit `63ccf57` fixes all three without weakening operator-work preservation. Ambiguous partial
content becomes a failed terminal while the original branch/worktree remains untouched; a later
explicitly confirmed attempt releases only Afactory's internal ownership ref and can deliver to a
new absent target. Read-only ignore classification uses the operator's `HOME`, `XDG_CONFIG_HOME`,
and `GIT_CONFIG_GLOBAL` while every mutating Git command stays sanitized. The runbook decodes
lossless receipt paths to raw filesystem bytes before `git add -f`. Eleven focused pilot tests,
the full workspace gate, fixture reproduction, and markdown lint pass. All seven Campaign Findings
are fixed and its Ledger has zero open; a fresh pinned Campaign must return Pass before V3.1 closes.

## 2026-08-27 — V3.1 fresh review gate cache corrected

Fresh Campaign `v3-1-client-pilot-v3` captured exact candidate `05490dd` against Base `9d8e0e0`,
then stopped at `GateBlocked` before any reviewer ran or any model tokens were spent. The captured
Snapshot contained the required fixtures; the failure came from `scripts/verify.sh` reusing a
global Cargo target whose cached test binaries embedded the compile-time path of an earlier,
already-destroyed review sandbox.

Commit `94f7bad` preserves the external shared build cache but gives every workspace-artifact test
the current gate root at runtime. The full `scripts/verify.sh` gate passes against the same global
cache that produced the failure, fixture reproduction remains byte-identical, and all eleven
client-pilot tests pass. Restart the incomplete zero-token Round on this expanded exact candidate;
a fresh pinned Pass remains the V3.1 handoff gate.

## 2026-08-27 — V3.1 fresh Campaign passed and milestone closed

Round 1 epoch 2 of pinned Campaign `v3-1-client-pilot-v3` reviewed candidate `814d3d0` against
Base `9d8e0e0` with release `v0.2.0` and one high-effort Claude Opus correctness reviewer. Both
gates passed in the fresh materialized sandbox, proving the runtime workspace-root correction
against the real shared-cache boundary. The reviewer spent 279,039 chargeable tokens and returned
Pass with three minor Findings.

Commit `d061d27` synchronizes the binding `AGENTS.md` status, converts decoded ignored paths to
Git `:(literal)` byte pathspecs before staging, and derives the optional template-authority test
from the runtime workspace root. Literal staging was reproduced with bracketed and leading-colon
filenames; the focused definition suite passes 25/25, the full kernel gate and byte-identical
fixture reproduction pass, and all eleven client-pilot tests pass. All three Findings are fixed
and the Campaign Ledger has zero open. V3.1 is complete for trusted design-partner pilots;
publishing a client release remains a separate human action.

## 2026-08-27 — Private v0.3.0 trusted-pilot release published

[Product PR #11](https://github.com/PakhomovAlexander/afactory/pull/11) merged V3.1 at `main`
commit `1412847`. Its first CI run exposed ambient `XDG_CONFIG_HOME` and `GIT_CONFIG_GLOBAL`
leaking into one delivery test; commit `1d53ed1` isolates the test's Git configuration without
changing the delivery contract. Replacement CI passed, and the full `scripts/verify.sh` gate plus
all eleven pilot tests passed again from a detached worktree at the exact merge commit.

Lightweight tag `v0.3.0` points to `1412847`. The private GitHub release workflow built and
published both supported archives. Their downloaded checksum sidecars passed: macOS arm64 archive
`sha256:abbedf77fb5bf1bf07a1a6d234c0d9f91c366b9c4dfb2a5b14d5dc9d62b6314f` and Linux x86_64
archive `sha256:954b303c6749797b329213a00b9dbaceecb319c3a638e86be0c7e7afa936e816`. The extracted
macOS release binary reports `af 0.3.0`. The bounded client runbook is now ready for trusted
design-partner handoff; broader delivery and automatic publication remain outside V3.1.

## 2026-08-27 — V3.2 binary-owned review onboarding locally verified

Branch `agent/onboard-command` adds deterministic, token-free `af onboard` under ADR-0032. A new
Git repository receives a read-only preview by default; explicit `--apply` installs one absent
`.af/` directory with a version-2 Diff pipeline, required Gate, independently prompted correctness
and architecture Workers, exact pipeline/package pins, bounded budgets and convergence, and a
standalone agent runbook. Existing authority is validated without mutation; explicit
`--refresh-lock` validates current bytes and atomically replaces only the selected pipeline and
referenced Worker pins. The command never runs a Gate or model, accesses credentials, creates
Campaign state, fetches a PR, commits, pushes, comments, or overwrites authority.

Four integration tests prove preview writes nothing, apply creates and repeat-apply refuses,
tampering fails exact validation, explicit refresh repairs referenced pins while preserving an
unrelated pin, uncapped existing authority reports an honest absent budget instead of zero, and
missing Gate discovery fails closed. The candidate binary previewed the real hub and selected its
shipped `scripts/verify.sh` Gate without writing; it then created and revalidated the complete
eight-file authority in a disposable Git repository. The full
`make check` gate passes: formatting, warnings-denied Clippy, all workspace and doc tests, and
byte-identical fixture reproduction. Integration to `main` and a checksummed release remain
explicit human publication actions.

## 2026-08-27 — Private v0.4.0 review-onboarding release published

[Product PR #13](https://github.com/PakhomovAlexander/afactory/pull/13) merged V3.2 at exact
`main` commit `bb9e5a3`. PR CI and the complete exact-main CI run passed. Lightweight tag
`v0.4.0` points to that commit, and release workflow run `33102292695` completed every macOS,
Linux, and publish job successfully.

Both published checksum sidecars passed after download: macOS arm64 archive
`sha256:ab2f73f85718d7764d8cccee3964ef04d37d250dba3fd2d3885eb6f21270e258` and Linux x86_64
archive `sha256:bb90eb44296eb69ab24ce10289811db785a44dff610ba92216472288a1509cb0`. The extracted macOS
binary reports `af 0.4.0`, and `af onboard --help` exposes the shipped command. V3.2 is complete;
the canonical capability roadmap resumes at M3.2.

## 2026-08-28 — M3.2 explicit dispositions implemented and dogfooded

Commit `e930ea1` adds the additive `ReviewerResult@2` and `FindingDisposition@1` contracts. New
pipelines pass the exact prior `FindingSet@1` to each required reviewer and require exactly one
`corroborate`, `not_reproduced`, or `dispute` position per assigned Finding. Missing, duplicate,
and unassigned coverage is rejected before Attempt admission; each selected disposition is an
immutable Attempt-produced artifact bound to source, Round, Subject, and exact inputs. Reducer@2
records those artifacts in the next Finding Set, disputes contest without veto authority, and
Drops remain evidence rather than trusted fixed resolution. Permanent `ReviewerResult@1` readers
and behavior remain unchanged. Focused real-binary two-Round and incomplete-output tests plus the
full `make check` gate passed.

The candidate binary first refused `origin/main` as authority because its historic review lock
stored the pipeline's raw SHA-256 instead of the kernel's domain-separated content ID. A local,
disposable authority commit `05b2c98` changed only that pin, preserving the old one-reviewer Claude
Opus policy while keeping candidate policy untrusted. Campaign `m3-2-dispositions-v1` then reviewed
candidate `e930ea1`: Round 1 spent 286,400 chargeable tokens, rendered 118,706 context bytes
(29,677 estimated tokens), and opened one blocker, two major Findings, and one minor Finding.

Commit `1fb9569` fixes all four. V2 corroboration now emits current provenance-carrying evidence so
a persisting fixed Finding reopens; every dispatch pins the exact Finding Set; the required subset
is derived from the current Round's already-pinned filtered assignment so authority diagnostics
and operator-declined Findings are not obligations; and V2 guidance forbids duplicate flat
reports and names only the dispositions channel. Regression tests cover each boundary, including
the durable dispatch reference, and the full gate passes with byte-identical fixture replay. All
four Campaign Findings are recorded fixed. A proposed Round 2 was refused before provider egress
because the original disclosure approval did not cover the changed private payload, so it spent
no tokens; a fresh explicit approval may run that optional clean confirmation later.

## 2026-08-28 — M3.3/M4 verification follow-up

PR #18 merged M3.3 and the M4 implementation at exact `main` commit `cea50aa`; exact-main CI
passed. Campaign `m3-3-m4-verification-v1` spent 417,835 tokens in Round 1 and 396,818 in Round 2.
The first three Findings were fixed in `4d1c211`; the exhausted second Round opened three major
follow-ups. Their corrections preserve Resolution authority across later Grouping and tracked
expiry, pin required/advisory Demand policy in the exact pipeline artifact, and introduce an
explicit `EvidenceReuseAdmission@1` so reuse can clear a current Demand without mutating the
frozen `EvidenceSatisfaction@1` contract. The full local gate passed: formatting, clippy with
warnings denied, all workspace tests and doctests, and byte-identical synthetic fixture
reproduction. At that checkpoint, only the exact Campaign-ledger dispositions remained.

The owner approved the three exact fixed transitions. Campaign
`m3-3-m4-verification-v1` now has six fixed Findings and zero open; its historical final verdict
remains exhausted because immutable completed Rounds are not rewritten. Follow-up PR #19 opened
from `0361a7d`, and its initial CI passed. Integration and exact-main verification remain.

PR #19 merged at exact `main` commit `01147ce`; post-merge CI run `33185591446` passed the full
workspace Check and CLI smoke. M4 is complete and issue #15 is closed. Resume at M5 operator
visibility.

## 2026-08-31 — Private v0.5.0 M3–M6 release published

[Product PR #25](https://github.com/PakhomovAlexander/afactory/pull/25) merged default-light
Campaigns, and release PR #26 merged the workspace version at exact `main` commit `c51601a`.
PR and exact-main CI passed, including live container probes. Lightweight tag `v0.5.0` points to
that commit; private release workflow run `33432518747` completed its macOS, Linux, and publish
jobs successfully. Downloaded checksums passed for macOS arm64
`sha256:fb305ae48a30a4cfd6637a9b092af9746972f311a19971ee5b3ba4229a97f1c4` and Linux x86_64
`sha256:af7edb12fbe4c386ae295bed1874126e3579c01f4f617b49eaa5de3fe5091902`; the extracted binary
reports `af 0.5.0` and exposes light/heavy Campaign modes.

## 2026-09-01 — M7–M9 implemented, light-dogfooded, and locally verified

Branch `agent/m7-m9` completes the dependency-ordered kernel roadmap. M7 transports at most one
optional Proposal beside the unchanged flat Reviewer Result, validates its complete sealed diff,
binds its lifecycle to the selected durable Attempt and canonical Ledger, and exports exact or
explicitly stale Proposal IDs. M8 adds pipeline-v5 Slice, Slice Set, Shard Set, and Semantic
Closure contracts; a typed Scatter owns deterministic bounded shard Attempts, lossless gather,
fan-out accounting and replay, and whole-Subject closeout. M9 composes only selected, accepted,
disjoint Proposal Manifests from `auto_apply` bindings, checks an unpromoted derived Snapshot,
and atomically records attestation plus the new internal Campaign head. The next Round consumes
that exact derived Subject. Static automatic Integration is refused until a captured semantic
closure route exists.

The hub's checksummed `v0.5.0` launcher ran one correctness-only light Campaign,
`m7-m9-light-v1`, with Codex `gpt-5.6-terra` at `xhigh`; no Claude or Opus Worker ran. It spent
350,595 chargeable tokens and reported two blockers and three majors: Proposal lifecycle events
lacked durable Attempt authority; Integration was insufficiently bound to selected sealed
Manifests and opt-in; Slice Sets were not reconstructed from captured policy; fan-out spend was
lost on replay; and static Integration could terminate without Semantic Closure. Each issue now
has a fail-closed implementation and adversarial regression. Per the light-review contract no
second Campaign was dispatched; the independent final `make check` passed formatting,
warnings-denied Clippy, all workspace tests and doctests, and byte-identical fixture reproduction.

## 2026-09-01 — Private v0.6.0 M7–M9 release published

[Product PR #28](https://github.com/PakhomovAlexander/afactory/pull/28) merged M7–M9 at
`8fb500a`; both PR checks and exact-main CI passed, including live container probes. Release
[PR #29](https://github.com/PakhomovAlexander/afactory/pull/29) then bumped only workspace package
versions and merged at exact `main` commit `fb462ba`. Local `make check`, release-PR CI, and
exact-main CI run `33501053304` passed before the tag was created.

Lightweight tag `v0.6.0` points to `fb462ba`. Private release workflow run `33501704396` built and
published both supported archives. Downloaded checksum sidecars independently verified macOS
arm64 archive `sha256:d4bb4b254ba86b170a7b10407bf35b76d845c3cccd590a09f4d2032cb5a00883`
and Linux x86_64 archive
`sha256:d7c89e88335c59616eaabd5b44126fa7c589ec8373ad2ed9db514e45a4e5b568`.
The extracted macOS release binary reports `af 0.6.0`. The dependency-ordered M0–M9 roadmap is
complete; static automatic Integration without captured semantic closure remains deliberately
refused rather than silently weakened.

## 2026-09-01 — Private v0.7.0 review-safety release published

[Product PR #31](https://github.com/PakhomovAlexander/afactory/pull/31) merged the first
post-roadmap safety slice at exact `main` commit `91cc49c`. Its one pinned `v0.6.0` light Campaign,
`audit-safety-light-v1`, spent 230,395 chargeable tokens and found two defects: package arguments
could widen reviewer filesystem authority, and Provider smoke spend could leave too little Run
budget for one static Attempt per Worker. Both gained fail-closed regressions; no second Campaign
was dispatched under the light-review contract. PR CI and exact-main `make check` passed.

Release [PR #43](https://github.com/PakhomovAlexander/afactory/pull/43) bumped only workspace
package versions and merged at exact `main` commit `c9a62fb`. Local `make check`, the optimized
release build, release-PR CI with live container probes, and a fresh exact-main `make check` all
passed before tagging. Lightweight tag `v0.7.0` points to that exact commit. Private release
workflow run `33531761113` completed its create, macOS, Linux, and publish jobs successfully.
Downloaded checksum sidecars independently verified macOS arm64 archive
`sha256:964821d63fc478798af7d7e8aa709fca62ec8613fb42e357ebaef0d30a66eeaf` and Linux x86_64
archive `sha256:28bb6eabda42f1b5b0a768f1fdec0a9a8e1ff5c5a59b35b07bccc9cf9e6738fb`.
The extracted macOS binary reports `af 0.7.0`.

## 2026-09-02 — Consumer compatibility fixture (issue #45)

The hub's pinned `.review/pipelines/heavy.toml` was rejected by `v0.7.0` (`Ledger node must declare
a review.kernel/DemandSet@1 output`) for a week without notice; hub PR #11 fixed the policy and
added a token-free `make review-plan`. `fixtures/consumers/hub/` now mirrors the hub's `.review/`
policy verbatim, `crates/reviewctl/tests/consumer_compat.rs` plans it with the built `af` inside
`make check` and proves the pre-fix pipeline is still rejected, and the release workflow plans it
with the built artifact on every target before publishing. Follow-ups: #46 (`.review/` migration
path in `af onboard`), #47 (`af.lock` pins the `af` version that produced it).

## 2026-09-02 — Legacy `.review/` migration in `af onboard` (issue #46)

`af onboard` recognizes legacy `.review/` authority when no `.af/` exists: it validates every
pipeline and the lock against the current format and reports each pending upgrade (`legacy` /
`legacy-outdated`), writing nothing; `--migrate --apply` rewrites outdated pipelines in place
through `review_config::pipeline_edit::{legacy_upgrades, apply_legacy_upgrades}`, a fixed,
additive, idempotent upgrade list whose only member today adds the `DemandSet@1` Ledger output
(status `migrated`). Scaffolding `.af/` beside `.review/` is refused. Tests drive the consumer
fixture through outdated → migrated → planned by the built `af`. `docs/migration.md` records the
deprecation posture; the release that drops `.review/` stays an owner decision (migration ADR).

## 2026-09-02 — `af.lock` pins the `af` release that wrote it (issue #47)

`Lockfile` gains an optional `af_version`; `af onboard --apply` and `--refresh-lock` stamp the
running release. `af onboard`, `af review plan|run` (fresh authority), and `af task start` compare
it with the running binary: a lock pinned by a newer release is refused with a message naming
both versions, a lock pinned by an older release proceeds and prints one note pointing at
`--refresh-lock` (onboard surfaces it as a warning and reports `lock_af_version`), and a lock
without the pin stays silent — the owner chose minimum friction for consumers on pinned
launchers. Pre-0.8 binaries refuse a pinned lock through `deny_unknown_fields`, which is the
intended direction. Legacy `.review/review.lock` is never stamped; `min_af` remains the floor.

## 2026-09-02 — Owner decision: `v0.8.0` drops legacy `.review/` authority (ADR-0043)

Recorded the owner's call: the next release accepts review authority only under `.af/` and ships
`af onboard --migrate` converting `.review/` into `.af/` (#52). Stored Campaign replay keeps
resolving pinned `.review/...` paths; `review.kernel/*` types, events, and domain terms stay frozen.
AGENTS.md's rebranding rule now excludes the layout; `docs/migration.md` and the backlog carry the
release gate. The hub and the consumer fixture move to `.af/` before the release is cut.

## 2026-09-03 — Wall-clock, usage, and dispositions become visible (audit recommendation 2)

Every persisted event carries `occurred_at = 1970-01-01T00:00:00Z` by design, so nothing recorded
how long a review took; provider usage per token kind lived only inside CAS provenance; and no
report counted rejections, so precision was a feeling. A sidecar table (`attempt_wall` in
`events.sqlite`) now records, per reviewer Attempt, its start, elapsed milliseconds, and the
adapter's `TokenUsage` split — outside event identity, replay, the Ledger, and convergence, which
stay byte-for-byte deterministic. `af review report` prints the Campaign wall-clock, a Round wall
column, per-Attempt duration and usage, and a Findings-by-disposition line (also in
`af/review-report@1` as `wall_ms`, `spend[].wall_ms`, `attempts[].wall`, `findings_summary`);
`af review campaigns` carries `wall_ms` and `findings` per Campaign; `af review ledger` ends its
summary with dispositions and wall. A store written before the sidecar reads as "not recorded",
never as an error. AGENTS.md now requires every dogfood record to state wall-clock, usage, and
dispositions including rejected and wontfix. No currency: prices belong to Providers.

## 2026-09-03 — Per-node Attempt caps (issue #42, audit recommendation 3)

`BudgetScope::Node` was reserved on every dispatch but never limited, and every Worker reserved
the pipeline-wide attempt cap. `[[nodes]] budget = { attempt = N }` now declares a Worker's own
cap: it is the reservation its dispatch takes (a dynamic shard inherits its Scatter's), its own
node scope is limited to it, and `[budgets]` must exist and cover it. Planning prints each static
Worker's reservation and their sum (`reservations`, `max_simultaneous_reservation`); provider
admission and onboarding refuse against that sum instead of `attempt × workers`, with retry
headroom measured by the largest cap; the spend report names the reservation that bounded each
Attempt. Pipelines without node caps are unchanged, byte for byte. Replay now seeds committed
spend per node as well as per Run and Scatter, so a resumed Round keeps counting against node caps.

## 2026-09-03 — `af review render`: a Worker's exact input, token-free (issue #36)

Prompt composition moved out of the Claude and Codex adapters into one pure function
(`compose_model_prompt`; `compose_command_input` for command Workers), and every adapter gained
`render_input`, the bytes it would send without sending them. `af review render --node NODE` reuses
`plan`'s resolution (now `resolve_plan`) to compose that input for a first Attempt: package
instructions, output contract, and the Change Set published exactly as a run publishes it;
Campaign-bound data (Attempt authority, prior Findings, Gate decision) is listed as omitted, never
invented. Header on stderr, raw bytes on stdout, `af/review-render@1` with `--json`; no state, Gate,
Provider, or spend. Tests prove rendered bytes equal what the Claude, Codex, and command adapters
actually write to stdin. README documents the command and command Workers (sandbox cwd, cleared
env, stdin JSON, no `.git` by construction). #32 can now refuse on the exact encoded size.

## 2026-09-03 — Oversized Worker input is refused before admission (issue #32, first slice)

`render`'s composition is now `first_attempt_input`, shared by `af review render`, `af review plan`,
and `af review run`. Plan measures every packaged model Worker's first-Attempt input against the
cap its dispatch reserves (`reservations[].input_bytes|input_tokens|fits`, `pipeline.inputs_fit`)
and prints it per Worker; run composes the same input from the pinned pipeline and the Round's
validated Change Set and refuses before any Gate, Provider admission, or Worker when the input
alone exhausts the cap — nothing dispatched or charged, the refusal naming bytes, tokens, cap, and
the bounded alternatives (narrower range, larger `budget.attempt`, a Scatter node). Nothing is
truncated. The automatic bounded-strategy selection with typed closure obligations is the
remaining half of #32 and belongs with path routing (#40).

## 2026-09-03 — Campaign state has a size and a garbage collector (audit recommendation 6)

`af review campaigns` now reports each Campaign's state directory and newest store write
(`state_dir`, `last_activity_unix_ms`), and its bytes on disk with `--sizes` (`state_bytes`; the
walk over 651k files on the owner's machine takes 21 s, so it is opt-in). `af review gc --older-than DAYS
[--keep N]` lists the Campaigns that would go and how much they hold; only `--apply` removes them,
whole directories at a time, never a symlink and never a directory the enumeration could not read.
The owner's machine held 9.6 GB across 34 Campaigns with no way to see or reclaim it.
