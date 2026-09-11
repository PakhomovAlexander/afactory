# Afactory Review Kernel - capability work (M0-M9)

**Active increment:** [Task execution](task-execution.md), P00–P14. The owner approved
implementation on 2026-09-10. Contracts, common execution, sharing, embedding, bounded repair,
selection and generated-plan approval are implemented in the stacked PRs. Portable export and
contract tests, the software/document starter pack and heavy repair-history continuation pass the
full gate. Read-only issue capture and source conformance also pass. Ticket revision refresh,
Review compatibility and P14 release evidence are next. The completed
M0–M9 record below remains the compatibility foundation.

**Status:** M0–M9 are complete and shipped in private release `v0.6.0` from exact `main` commit
`fb462ba`. M7 adds base-bound, seal-checked Proposal
transport and exact export; M8 adds typed bounded Scatter, lossless Shard Sets, whole-Subject
closeout, and semantic closure; M9 adds opt-in deterministic Integration that checks an
unpromoted derived Snapshot before one atomic internal-head commit. Pinned `v0.5.0` light dogfood
Campaign `m7-m9-light-v1` found five authority/replay defects; all five have deterministic
regressions and are fixed. Local and exact-main gates, live container probes, both published
archives/checksums, and the extracted macOS binary passed. Static automatic Integration is
deliberately refused until a captured semantic-closure route exists. The
accepted V3.1
local-delivery slice is complete for trusted design-partner pilots: it is proven against the real
v2 dogfood Task, fresh pinned Campaign v3 returned Pass, and all three final minor Findings are
fixed with zero open. PR #11 merged and private `v0.3.0` is published. The bounded V3.2
binary-owned review-onboarding slice shipped through PR #13 in private `v0.4.0`; exact-main CI,
downloaded checksums, and the extracted macOS binary passed.
The first post-roadmap review-safety slice shipped in private release `v0.7.0` from exact `main`
commit `c9a62fb`; release CI, both downloaded archive checksums, and the extracted macOS binary
passed.
**Goal:** `af review` reviews a *change* rather than a whole tree, and every finding it produces
can be read, triaged, and closed only through explicit evidence-bearing policy; minimal v2 then
lets `af` implement a real change and return a verified internal Snapshot.
**Log:** [`workstream/log.md`](workstream/log.md)

## Summary

Campaign review is light by default and mechanically limited to one closed Round; explicit
human-requested `--heavy` retains full pipeline convergence. A light finding-bearing result says
`fix_then_gate` and refuses a second dispatch, preventing the repeated-Campaign M6.3 dogfood
mistake. ADR-0037 records the policy.

A second design audit on 2026-08-20 recovered the original accepted Review Kernel design from
the originating RawTree hub and challenged the six-milestone reconstruction against it and the
current contracts. The corrected roadmap has M0–M9 and an indexed ADR history; ADR-0003 and
ADR-0004 are superseded. M0–M2 are complete; M2's first Campaign exhausted
after four Rounds opened 45 Findings while retaining every prior correction. The repository/release
migration, Project Hub external cutover, bounded Provider Operation dogfood slice, and first v2
implementation dogfood are also complete. M2 converged under the recorded correctness-only review
policy. Product v1/v2 establish the final local review and verified implementation boundaries.
M3.1 builds canonical Report identity and immutable Finding Sets; M3.2 makes every assigned
prior-Finding disposition explicit and durable without treating reviewer silence as a Drop.

Everything decided is written down. **Do not re-derive it; read it.**

| Read this | For |
|---|---|
| [`../CONTEXT.md`](../CONTEXT.md) | Canonical vocabulary, including Snapshot vs Tree Digest, Subject vs Review Selector, explicit Drop, Demand, Fix Verification, Cache Snapshot, and Semantic Closure. |
| [`backlog.md`](backlog.md) | The implementation roadmap, M0–M9, in dependency order. |
| [`adr/`](adr/) | Durable and security-boundary decisions, including supersession history. |

## Background / current state

The kernel is a multi-crate Rust workspace with unusually disciplined tests. The backlog is
**not** a cleanup list — it is capability the design implies that the code
does not yet deliver, plus a few places where checked-in docs describe behaviour that does not
exist.

The findings that now drive the roadmap, all verified in code or the recovered design:

1. **Reviewers cannot see the change.** `Capture::committed` is `git ls-tree -r` — blobs only, no
   `.git`, no base. `ReviewerInputs` carries one field, `prior_findings`. Both reviewer prompts
   open with *"Read the change in the working directory you were given."*
2. **Canonical Report authority is now projected for new Campaigns.** M3.1 reads validated,
   enveloped Reports while retaining the frozen legacy reader for old Campaigns.
3. **Explicit dispositions are now authoritative.** New pipelines pair exact `FindingSet@1`
   inputs with `ReviewerResult@2`; every assigned Finding requires one immutable corroborate,
   `not_reproduced`, or dispute artifact. Missing, duplicate, and unassigned coverage fails closed;
   permanent `ReviewerResult@1` replay is unchanged.
4. **A Rust `Debug` impl is load-bearing for convergence.** `publish_report` persists
   `format!("{verdict:?}")`, and the round counter reads it back with
   `.starts_with("Incomplete")`. Renaming `RunVerdict::Incomplete` makes incomplete rounds start
   *closing* rounds, with no compile error.
5. **Canonical identity is policy-versioned.** New Campaigns derive a Finding from its selected
   Report artifact unless an explicit relation or exact trusted occurrence key attaches it;
   legacy Campaigns permanently retain `sha256(file + "|" + title)` replay.
6. **Campaign authority can drift.** Every Round reloads pipeline and package bytes from the live
   checkout; candidate content can change authority while retaining one Campaign identity.
7. **Evidence has no lifecycle.** Demands have no durable state and `fixed` may be asserted
   directly without current-Subject verification.
8. **The recovered phases were missing.** Dynamic scatter/gather, semantic closure, and internal
   derived-Snapshot Integration were absent even though supporting contracts and budget scope
   already exist.

## Design

See [`backlog.md`](backlog.md) and the complete [`ADR index`](adr/README.md). The most important
corrections from the second audit are:

- Report artifacts, not duplicated event payload fields, are claim-content authority.
- New Findings have path-independent identity; explicit relations or trusted occurrence keys
  attach Reports, and reversible Grouping handles ambiguity.
- Reviewer silence is not a Drop, and `fixed` requires current-Subject Fix Verification.
- Campaign authority resolves from a pinned Authority Snapshot before candidate capture.
- Scope-authority failures are counted in convergence and new conclusions persist the distinct
  `RunReport@3` reason `authority_unavailable`; frozen @1/@2 readers remain permanent.
- Safe caches are sandbox-local snapshots; ADR-0008 supersedes host passthrough ADR-0003.
- Proposals are base-bound and exported by Proposal ID; ADR-0010 supersedes ADR-0004.
- Proposal declarations are transported beside unchanged flat Reviewer Results, verified at seal,
  durably prepared with selected Attempts, and finalized after canonical Report reduction under
  ADR-0038.
- Dynamic scatter/semantic closure and internal derived-Snapshot Integration are restored as M8
  and M9 rather than silently omitted.
- Pipeline v5 keeps the authority DAG static while typed Scatter nodes own durable tagged shard
  sub-invocations and lossless Shard Sets under ADR-0039.
- Wise token use and minimum Worker context are the first two design values; Inputs carry a
  measured context manifest and context expands only through bounded recorded retrieval
  ([ADR-0028](adr/0028-prioritize-wise-token-use-and-minimum-worker-context.md)).
- Minimal v1 and v2 precede candidate dogfood and M3.1. v2 ends at a verified internal Snapshot;
  delivery and optional platform surface are v3
  ([ADR-0030](adr/0030-complete-minimal-v1-and-v2-before-dogfood.md)).

## Scope

In: this repository's Review Kernel crates, schemas, fixtures, CLI, and generic pipeline
contracts. Out: project-specific `.review/` pipelines, reviewer packages, campaign state, and
private corpora. The retired shell harness under `compat/legacy-harness/` remains only as the
executable specification that regenerates the synthetic fixture corpus.

Minimal v1/v2 are inside scope because they establish the kernel's own development gate. They add
the final `.af/` user surface without renaming frozen internal declarations or persisted contracts.

**Sequencing is deliberate.** M0 freezes append-only contracts; M1 makes current evidence usable;
M2 adds the trusted Subject; product v1 establishes final local review; v2 adds verified sequential
implementation; candidate dogfood proves that loop. M3 establishes canonical claim identity and
explicit dispositions; M4 builds snapshot-scoped
Evidence and resolution on it. M5–M7 add operator, gate, and Proposal capabilities. M8 and M9 then
add dynamic execution and internal Integration only after their identity, authority, isolation,
and verification prerequisites exist.

## Acceptance criteria

- [x] M0 — every event type is a Rust/schema enum member; structural RunReport versions are
      permanent and additive; ports
      validate type/cardinality/snapshot affinity; exact invocation inputs and output receipts are
      persisted; legacy replay remains pinned by fixtures.
- [x] M1 — `af review show` prints every attached report whole; `ledger --long` carries body and
      fix; `report --format md` emits what SKILL.md §5 asks the agent to produce.
- [x] M2 — Campaign authority and Base are pinned before candidate capture; committed and
      revalidated dirty heads produce wired diff Subjects; generic Git execution still refuses
      `diff`; both rename endpoints govern Report Scope and replay never rewrites existing legacy
      keys. Canonical path-independent Finding identity remains M3.1.
- [x] Product v1 — final local `af review` uses `.af/`, sequential execution, exact measured
      Worker Input, local state, token receipts, and a typed outcome beside green `make check`.
- [x] Product v2 — one implementer produces an internal derived Snapshot; read-only acceptance
      Gates and a separate evaluator yield a sealed verified/unverified result with no delivery.
- [x] Candidate dogfood — v2 implements one real kernel change and records complete context,
      token, Gate, evaluator, Snapshot, and outcome evidence beside green `make check`.
- [x] Product V3.1 — an explicitly confirmed verified Task can be delivered only to a new local
      branch/worktree, with durable history, rollback/recovery, and no commit, push, PR, or remote.
  - [x] Local implementation, full kernel gate, and deterministic pilot smoke.
  - [x] Real trusted-repository pilot and exact idempotent replay.
  - [x] Pinned external correctness review.
- [x] Product V3.2 — `af onboard` deterministically previews, creates, validates, explains, and
      explicitly re-locks one static multi-review authority bundle without model calls or
      publication.
  - [x] Binary implementation, focused integration tests, full `make check`, hub preview dogfood,
        and disposable-repository apply/validate dogfood.
  - [x] Integrated to exact `main` commit `bb9e5a3`; PR and main CI passed; private `v0.4.0`
        checksummed release assets were downloaded and verified.
- [x] M3 — the live reducer consumes typed Reports; new Findings have path-independent IDs;
      every assigned prior Finding has an explicit disposition; Grouping is reversible.
  - [x] M3.1 implementation and local `make check`.
  - [x] M3.1 pinned external convergence review.
  - [x] M3.2 implementation, full local gate, and lightweight candidate dogfood.
  - [x] M3.3 reversible Grouping implementation and full local gate.
- [x] M4 — required Demands block independently; Evidence is Demand/Subject-linked; `fixed` can
      result only from positive Fix Verification; non-fixed resolutions are scoped, expiring, and
      challengeable; convergence reads exact final Finding/Demand views; admitted partial reviewer
      results remain visible without acquiring Ledger or convergence authority (issue #15).
  - [x] M4.1–M4.5 implementation and issue #15 reporting merged through PR #18.
  - [x] Fix the three exhausted-Campaign follow-up Findings and pass the full local gate.
  - [x] Record their exact Campaign-ledger dispositions; six Findings are fixed, zero open.
  - [x] Integrate PR #19 and verify exact `main`.
- [x] M5 — JSON/text reports are deliberate; spend is reported per Round/reviewer; Campaigns and
      Round history are enumerable.
  - [x] M5.1/M5.2 deliberate report formats and Round/reviewer spend query.
  - [x] M5.3 safe Campaign enumeration and history; exhausted review has seven fixed Findings and
        zero open, and fresh final verification returned Pass.
- [x] M6 — every executable node uses an admitted Execution Binding; smoke tests run with bounded
      sandbox-local Cache Snapshots; safe Attempts receive revocable Broker Handles rather than
      reusable credentials.
  - [x] M6.1 explicit Gate Execution Bindings, provider admission, independent
        `Mode::EphemeralWrite` clones, and structural `RunReport@4` evidence.
  - [x] M6.2 bounded sandbox-local Cache Snapshots; full local gate and final Codex-only Campaign
        passed with thirteen fixed Findings and zero open.
  - [x] M6.3 Broker Handles, receipts, revocation, secret isolation, recovery settlement, and
        pre-dispatch budget coverage; final correctness findings fixed and `make check` green.
- [x] M7 — a Proposal unequal to the sealed diff is refused; export is by Proposal ID; stale
      export requires explicit override.
- [x] M8 — accepted SliceSets fan out losslessly under fan-out budgets; whole-Subject closeout and
      semantic-output closure prevent omitted shard output from passing.
- [x] M9 — automatic Integration is opt-in, advances only an internal derived Snapshot at one
      transactional boundary, and leaves claims pending until a later verified Round.
- [x] `make check` stays green throughout; candidate `make dogfood` becomes available only after
      v2 and never replaces the deterministic gate.

## Post-roadmap safety hardening

M0–M9 are complete and shipped in private release `v0.6.0`. The first post-roadmap safety slice
shipped in private release `v0.7.0` and implements ADR-0041 and ADR-0042: explicit review
selectors and token-free planning, empty-Diff
refusal, exact Provider admission/doctor, adapter-owned Claude isolation, strict `.af/af.toml`, and
onboarding validation for real topology, budget arithmetic, stale pins, and runner model flags. It
passed the full deterministic gate and one pinned light review; both review Findings have
regressions and are fixed. Installer/self-update and provider model-alias resolution remain
separate work.
Do not add static automatic Integration by bypassing closure: pipeline v5 refuses it until a
captured Slicer/Scatter semantic-closure route exists.

M0 and M1 are complete. M2.1-M2.6 reached dogfood. Campaigns now publish one immutable
Campaign Manifest before candidate capture, reconstruct package execution from captured CAS bytes,
publish `Subject@1` and Subject-bound Round inputs, reuse incomplete Round inputs, and require an
explicit epoch supersession to capture a changed head. The Git adapter now resolves opaque tree
ids and exposes one configuration-neutral, byte-safe typed tree diff while generic `diff` remains
refused. Committed and revalidated dirty heads now publish one exact Change Set artifact wired
into every diff reviewer. Ledger replay now derives each attached Report's `in`/`out` Scope from
its exact Round Subject; convergence excludes wholly out-of-set Findings and fails closed on
legacy `unknown` evidence. Bounded, resumable Provider Operations now probe explicit machine-local
contexts, run a real inference smoke, fence exact continuations, and charge failed work before
dispatch. Change Sets include both rename endpoints, so Reports at either path project `in`
without rewriting the existing Finding key. Dogfood Rounds 1–3 produced 33 Findings whose
corrections are committed and fully verified. Round 4 retained all 33 as fixed, opened twelve
further claims, and exhausted Campaign `m2-rename-scope`; all 45 are fixed and its Round 4
corrections are committed and verified. Fresh Campaign `m2-rename-scope-final` Round 1 spent 396,132
tokens and opened sixteen further claims; Round 2 retained those fixes and spent 473,552 tokens on
eleven further claims. Round 3 epoch 1 reached no reviewer: the gate exposed an unbounded
installed-runtime probe. Its bounded correction was committed and the epoch restarted. Round 3
epoch 2 spent 495,186 tokens and opened nine further claims. Their corrections preserve readable
claims with unknown Scope, validate unique manifest paths, and make authority-unavailable conclusions
durable through additive `RunReport@3`. Those corrections were verified, committed, and resolved.
Round 4 spent 605,986 tokens, retained all 35 prior resolutions, opened eight further claims, and
exhausted the Campaign. Fresh Campaign `m2-rename-scope-convergence` Round 1 opened fifteen claims;
their corrections were verified, committed, and resolved. Round 2 spent 615,571 tokens, retained
fourteen of those fixes, reopened one, and opened ten further claims. Its corrections atomically
publish verified CAS sources, split source and duplicate materialization into two non-nested
bounded phases, cap container execution, validate the complete ReviewerResult before admission,
and remove remaining linear hot-path repetition. Those corrections were verified, committed, and
resolved. Round 3 spent 693,947 tokens, retained all 25 fixes, and opened eight further claims. Its
corrections validate typed projection authority, distinguish round-binding failures, make container
execution deadlines caller-owned, state the event-ordering boundary honestly, use verified CAS
reflinks, seek terminal reports by an indexed type range, and prepare directories without worker
locks. The corrections pass the full kernel gate and are committed and resolved. Round 4 spent
755,609 tokens, retained all 33 fixes, opened five further claims, and exhausted the Campaign. Its
corrections feed contract refusals into retries, fold read-only permissions into template cloning,
overlap level-parallel seal discovery with hashing, and remove remaining serial materialization
allocations. The corrections pass the full kernel gate and are committed and resolved. Fresh
unchanged-policy Campaign `m2-rename-scope-clean` Round 1 spent 605,849 tokens and opened nine
claims. Their corrections make retry feedback durable under additive `AttemptInput@1`, keep
authority diagnostics out of ordinary Finding convergence while still failing closed, reverify
Subject CAS authority every Round, centralize the flat ReviewerResult validator, reject
whitespace-only claims, recognize only Git's exact rename-limit warnings, share one non-following
permission implementation, overlap clone/chmod/cleanup directory discovery with bounded work, and
count serialized Change Set bytes without retaining a second allocation. ADR-0021 and ADR-0022
record the contract decisions. Round 2 spent 755,944 tokens, retained all nine fixes, and opened
seven claims. Their corrections keep active unreadable-report authority blockers fail-closed
beyond the original clean window, reject whitespace-only legacy paths, accumulate durable retry
history across process resume, stream exact CAS comparisons, parallelize dirty-worktree hashing,
share validated Change Set authority, and share sandbox baseline manifests. The full kernel gate
and release measurements pass; the corrections are committed and resolved. Round 3 spent 839,349
tokens, retained all sixteen prior resolutions, and opened eight claims. Their corrections keep
unreadable Report authority attached to a fixed Finding, validate the exact Change Set input at
the runner boundary, separate durable retry feedback from terminal diagnostics, stream captured
files into CAS, canonicalize the repository root once per scan, prepare typed artifacts in one
verified read, reuse per-worker CAS verification scratch, and avoid duplicate changed-path
ownership. ADR-0023 records the additive `AttemptFeedback@1` boundary. The full kernel gate and
release measurement pass; those eight claims were committed and resolved. Because Round 3 was not
clean, this Campaign could not converge. Round 4 spent 828,444 tokens and retained all 24
prior resolutions, opened eight claims, and exhausted `m2-rename-scope-clean`. Their corrections
make malformed answers inspectable durable retry feedback, give unreadable Report attachments a
readable recovery transition, bind renderer authority through an exact store-owned Change Set
capability, retain only changed paths in Ledger Scope, make warm streaming CAS publication
write-free, stream synthetic-tree blobs into Git, and serialize each published Change Set once.
The 195.3 MiB capture fixture records 1.496 s cold and 0.526 s warm. The full kernel gate passes;
the eight claims are committed and resolved. Fresh unchanged-policy Campaign
`m2-rename-scope-verified` pins the same policy and opened seven claims in Round 1 after spending
777,705 tokens. Their corrections deliver complete typed Change Set content to command reviewers,
make artifact types rather than port labels authoritative throughout configuration and execution,
replace an unreachable Change Set panic with a conflict, bound the parsed Change Set cache, repair
directory modes without following a raced symlink, buffer synthetic-tree `fast-import`, and
validate frozen Finding claims without deep clones. The full kernel gate passes; the claims are
committed and resolved. The 5,000-file / 195.3 MiB release fixture records 0.861 s for synthetic
tree construction. Round 2 spent 869,129 tokens, retained all seven fixes, and opened four claims.
Their corrections make serialized command input itself decide whether stdin exists, reject padded
live Report paths across Rust and both JSON schemas, separate per-batch Change Set validation from
the bounded cross-batch memo, and centralize the store's artifact-type vocabulary in `review-core`.
The full kernel gate passes; the claims are committed and resolved. Round 3 spent 990,029 tokens,
retained all eleven fixes, and opened six claims. Their corrections reject unsupported generation
contracts at load and receipt replay,
centralize Report and Change Set path semantics, restore only requested directory mode bits,
concurrently drain command-reviewer and Git pipes, bind command deadlines to captured Campaign
policy, and index recent authority failures for convergence. The 5,000-file / 195.3 MiB release
fixture records 0.861 s synthetic-tree construction with concurrent draining. The full kernel gate
passes; the claims are committed and resolved. Round 4 retained all seventeen prior fixes, opened
four claims, and exhausted the Campaign. Their corrections derive Generation contracts from
Subject kind, fail closed on any noncanonical typed Report location, preserve whitespace-edge Git
paths, and bind command input delivery to the attempt deadline. The full kernel gate passes; all
four claims are committed and resolved.
Fresh Campaign `m2-rename-scope-final-verified` pins the same authority and manifest. Round 1 spent
826,568 tokens and opened nine claims. Their corrections version Manifest path encoding while
preserving raw-tree identity, make command deadlines safe after parent exit, enforce Change Set
bounds before adapters, keep runner presentation out of Round authority, document typed
Generation compatibility, avoid allocating discarded patches, carry one run-bound Ledger
projection, and transfer command input without copying it. ADR-0024 and ADR-0025 record the two
compatibility decisions. Round 2 spent 912,203 tokens, retained all nine fixes, and opened seven
claims. Their corrections preserve baseline Manifest encoding through sandbox seal, make a held
output pipe observable after bounded drain grace, bound and classify retry feedback without
echoing reviewer bytes, build each model prompt in one buffer, borrow cached Ledger state, stream
CAS replay verification, and avoid decoding ordinary Manifest paths. The full kernel gate passes;
the claims are committed and resolved. Round 3 spent 969,363 tokens, retained all sixteen fixes,
and opened five claims. Their corrections release every prepared reviewer attempt on pre-adapter
failure, verify CAS length before `fast-import` framing, bind the scope-only Change Set reader to
the full wire shape with parity coverage, move consumed Ledger projections, and fold the already
loaded Campaign event vector. Round 4 spent 1,032,208 tokens, retained all 21 fixes, opened five
claims, and exhausted the Campaign. Their corrections publish a moving cold CAS source under its
second-pass identity, give completed commands fixed stdin-writer grace, preserve version-1
name-keyed Generation semantics, read Round authority through indexed events, and reuse the
already-loaded Campaign log across preparation. The full kernel gate passes; resolve these claims
and open a fresh unchanged-policy Campaign for the two-Round clean window. Campaign
`m2-rename-scope-clean-window` is open with the same authority and policy. Round 1 epoch 1 stopped
at the pre-dispatch gate with zero spend; its timing-dependent command-runner regression is now
deterministic and the full kernel gate passes. Restart the incomplete epoch, then require two clean
Rounds. Restarted epoch 2 spent 956,628 tokens and opened seven claims. Their committed corrections
restore the historical version-1 reviewer input name, consolidate bounded subprocess supervision, retain
stable kernel-owned retry rejection codes, enforce Change Set bounds at the CAS reader, close the
Ledger cache repopulation race, and stream synthetic-tree objects from one verified handle. The
full kernel gate passes; all seven are resolved. Round 2 spent 973,217 tokens, retained every fix,
and opened five claims. Their verified corrections charge post-spawn failures, move shared process
supervision into a leaf crate, reverify cached Change Sets, watermark Ledger projections by log
position, and parallelize entries inside wide directories. All five are committed and resolved.
Round 3 spent 1,020,400 tokens, retained all twelve fixes, and opened six claims. Their verified
corrections fast-forward and preserve watermarked projections, bound every Git subprocess, retain
complete stdout when only stderr is held, reference timeout evidence, and cache Subject validation
per transition batch. All six are committed and resolved. Round 4 spent 1,053,936 tokens, retained
all eighteen fixes, opened seven claims, and exhausted the Campaign. Their corrections pin gate
and Git deadlines, type run-budget exhaustion, preserve held-stderr check evidence, guard raw
Change Sets before encoding, unify Ledger projection input, and avoid irrelevant authority-plan
parsing. Commit `d8f0812` passes the full local gate, and all seven claims are resolved; the
Campaign has 25 fixed findings and zero open. M2 and release `v0.2.0` are complete. Minimal v1 now
provides final `.af` local review with external state, context and Provider usage receipts, and one
typed outcome. Minimal v2 now provides sequential `af task start --kind implement`, a fully
materializable derived Snapshot, fresh read-only Gates, an independent evaluator, typed budgets,
and explicit no-delivery outcomes. Both pass `make check`. Candidate `caab486` then completed the
first real implementation dogfood: the kernel Gate passed, the evaluator approved, and Snapshot
`sha256:46fd82a719d67341d4ddd95b32fd1dbd38fa94fd1ccd1a141020e061d4c2dc8f` remained internal.
M3.1 is complete: canonical new Campaigns persist enveloped Reports, derive path-independent
Findings, and pass exact immutable `FindingSet@1` IDs across barriers; legacy replay remains
frozen. M3.2 adds `ReviewerResult@2` plus immutable `FindingDisposition@1`, exact assignment
coverage, reducer@2 replay, corroboration reopening, and permanent @1 compatibility. Campaign
`m3-2-dispositions-v1` spent 286,400 chargeable tokens over 118,706 rendered context bytes and
found one blocker, two majors, and one minor. Commits `e930ea1` and `1fb9569` implement the slice
and fix all four; the focused regressions and full local gate pass. A follow-up external Round was
not required for deterministic acceptance. ADR-0033 now records that trusted Worker authority plus
intentional Campaign execution authorizes declared input delivery without per-call confirmation;
commit `179bb71` implements the agent-facing guidance. PR #18 subsequently merged M3.3 and the M4
implementation at `cea50aa`; exact-main CI passed. Verification Campaign
`m3-3-m4-verification-v1` exhausted with three major follow-up Findings. Their local corrections
preserve per-Finding Resolution authority across Grouping, pin reviewer Demand classification,
and require explicit Evidence reuse admission; the full local gate passes. All six Campaign
Findings are fixed with zero open. Follow-up PR #19 merged at exact `main` `01147ce`, and
post-merge CI passed. M5 adds versioned Markdown/text/JSON reports, first-terminal Attempt
accounting, per-Round/per-reviewer spend, opaque contained Campaign state IDs, and deterministic
Campaign history with isolated per-entry problems. Exhausted Campaign
`m5-campaign-enumeration-v1` has seven fixed Findings and zero open; fresh Campaign
`m5-campaign-enumeration-final-v1` returned Pass, and its four follow-up Minors are fixed. M6.1
adds explicit v3 Gate authority, provider-owned sandbox materialization, pre-dispatch isolation
admission, independent ephemeral-write clones, and exact `RunReport@4` evidence while freezing
v1/v2 behavior. The disposable `m6-gate-binding-dogfood-fixed` Campaign ran project-hub's
write-heavy scaffold/update smoke checks and two Codex Workers to Pass with no findings. M6.2 adds
bounded cache materialization and receipt authority. M6.3 adds explicit v4 reviewer credential
modes and a broker that keeps reusable credentials machine-local while project authority fixes
routes, byte/call/usage bounds, leases, and receipts. Receipt commit rechecks live authority;
responses containing credential bytes are discarded; Attempt settlement covers broker usage; and
recovery or supersession fences reserve enough authority to persist only a late revoked receipt.
Late overruns raise terminal commitment, provider admission counts outstanding broker authority,
one refusal terminally bounds receipt growth, partially percent-encoded credentials are withheld,
and broker authority must fit the pre-dispatch reservation. The final correctness Campaign's two
findings have deterministic regressions and are fixed; the final `make check` gate passes. M7–M9
then completed on `agent/m7-m9`; their publication and exact-main verification remain.
ADR-0031 separately authorizes the narrow V3.1 delivery slice for trusted
design-partner pilots; it does not weaken the M3.1 convergence requirement or pull broader v3
work forward. Commit `fdaf37f` implements that slice: exact source/derived authority checks,
prepared and terminal delivery records, process serialization, owned-ref rollback/recovery,
`task list`/`task show`, and the client-pilot runbook all pass `make check` and `make pilot-check`.
The real v2 dogfood Task then exposed an empty-index presentation defect in the first delivered
worktree. Commit `41c085d` populates the per-worktree index with plumbing-only `read-tree`, and
exact replay returned delivery receipt
`delivery-b6ba56b39cac1793e7c96f160b82f265e4630eac6e497bfb64a1211fd5365aa6` with no remote
actions. The delivered `make dogfood-contract` and the full kernel gate pass. That left pinned
external review before partner handoff. A final local safety audit found that prepared-state
recovery could force-remove an operator-modified worktree when its refs and HEAD were unchanged.
Commit `09853ac` now requires the index to remain at the source tree and refuses recovery rollback
for any non-empty worktree whose bytes were not observed during the current creation attempt;
filesystem edits, deletions, and staged changes are regression-tested and preserved. External
Campaign `v3-1-client-pilot-v2` Round 1 then spent 213,903 tokens and opened one major and three
minor Findings. Commit `f11a09f` fixes all four: empty-index recovery no longer wedges, sealed
repeat remains inspectable after operator use, checksum failure aborts installation, and ignored
Snapshot paths are explicit in the receipt and runbook. Round 2 spent 167,787 tokens and opened
one major plus two minor Findings before exhausting the Campaign. Commit `63ccf57` makes ambiguous
partial-materialization recovery terminal and retryable without deleting operator work, applies
the operator's global Git excludes to the advisory receipt, and gives the runbook a byte-safe
decoder for encoded paths. All seven Findings are fixed, the Ledger has zero open, and both
`make check` and `make pilot-check` pass. Fresh Campaign `v3-1-client-pilot-v3` subsequently
returned Pass. Commit `d061d27` fixes its three minor Findings, the Ledger has zero open, and the
full gate remains green. V3.1 shipped for trusted design-partner pilots in private `v0.3.0` from
`main` commit `1412847`; resume M3 at M3.2 while pilot evidence accumulates.
After v4, the owner retired the two-specialist clean-window policy after roughly four million
tokens in that Campaign. ADR-0027 makes one high-effort correctness reviewer, one clean Round, a
two-Round ceiling, and a one-million-token run cap the policy for new Campaign authority. Do not
run another Round under either exhausted immutable Campaign.
Continue in milestone order; do not pull Proposal or scatter work forward past Subject, authority,
isolation, and verification prerequisites.

## Risks / notes

- **The original design is external provenance, not a runtime dependency.** It was recovered from
  the originating RawTree hub at `docs/workstreams/review-kernel.md`. Its missing obligations are
  now represented in M8/M9 and the ADRs here; implementation must rely on these checked-in local
  documents rather than an absolute path to another checkout.
- **ADR-0003 is superseded by ADR-0008.** Safe gates use sandbox-local Cache Snapshots rather
  than direct host passthroughs. ADR-0001's *substance* (use git's diff) remains accepted over
  computing the diff in-process; change it only through another superseding ADR.
- **File and line references in `backlog.md` will drift** as soon as M1.1 lands. Treat them as
  where-to-look, not as ground truth, and prefer grepping the symbol.
- **CI gates on markdownlint across `**/*.md`.** Run
  `npx --yes markdownlint-cli2@0.22.1 --config .markdownlint-cli2.jsonc "**/*.md"`
  before pushing. A missing blank line before `---` turns the preceding paragraph into a setext
  heading and fails the build.
- **Editing a reviewer package requires re-locking**, or the digest check fails at load:
  `cargo run -p review-config --example lock -- .review/reviewers correctness` is
  the kernel repo's shipped generator. Replace `.review/review.lock` with its stdout atomically.
- **M7 makes reviewer rounds more expensive.** A model that writes code costs more than one that
  writes prose; every pipeline's `[budgets]` caps need re-deriving when it lands.
- **Candidate dogfood cannot be its own only safety story.** `make check` remains independent;
  pinned `v0.2.0` remains the last-green reviewer while v1/v2 are built and during the first real
  candidate implementation run.
- Automatic Integration never applies patch text to a checkout: it composes M7's sealed candidate
  Manifests and promotes only a checked internal derived Snapshot ([ADR-0040](adr/0040-promote-only-checked-derived-snapshots.md)).
