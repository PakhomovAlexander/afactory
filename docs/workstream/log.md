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
