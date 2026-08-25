# Afactory Review Kernel - capability work (M0-M9)

**Status:** M0 and M1 are complete. Five M2 dogfood Campaigns exhausted with all 179 Findings
fixed. Fresh unchanged-policy Campaign `m2-rename-scope-final-verified` Round 1 opened nine further
Findings; their corrections are fully verified. They must be resolved and retained before a new
two-Round clean window can establish M2 convergence and allow M3.1.
**Goal:** `af review` reviews a *change* rather than a whole tree, and every finding it produces
can be read, triaged, and closed only through explicit evidence-bearing policy.
**Log:** [`workstream/log.md`](workstream/log.md)

## Summary

A second design audit on 2026-08-20 recovered the original accepted Review Kernel design from
the originating RawTree hub and challenged the six-milestone reconstruction against it and the
current contracts. The corrected roadmap has M0–M9 and nineteen ADR records; ADR-0003 and ADR-0004
are superseded. M0 and M1 are complete. M2.1-M2.6 reached dogfood; its first Campaign exhausted
after four Rounds opened 45 Findings while retaining every prior correction. The repository/release
migration, Project Hub external cutover, and bounded Provider Operation dogfood slice are also
complete. The M2 dogfood Campaign must converge before work resumes at M3.1.

Everything decided is written down. **Do not re-derive it; read it.**

| Read this | For |
|---|---|
| [`../CONTEXT.md`](../CONTEXT.md) | Canonical vocabulary, including Snapshot vs Tree Digest, Subject vs Review Selector, explicit Drop, Demand, Fix Verification, Cache Snapshot, and Semantic Closure. |
| [`backlog.md`](backlog.md) | The implementation roadmap, M0–M9, in dependency order. |
| [`adr/`](adr/) | Durable and security-boundary decisions, including supersession history. |

## Background / current state

The kernel is ~15.3k lines of Rust across 13 crates, with zero TODOs and unusually disciplined
tests. The backlog is **not** a cleanup list — it is capability the design implies that the code
does not yet deliver, plus a few places where checked-in docs describe behaviour that does not
exist.

The findings that now drive the roadmap, all verified in code or the recovered design:

1. **Reviewers cannot see the change.** `Capture::committed` is `git ls-tree -r` — blobs only, no
   `.git`, no base. `ReviewerInputs` carries one field, `prior_findings`. Both reviewer prompts
   open with *"Read the change in the working directory you were given."*
2. **Canonical Report content is hidden by the projection.** `fix` exists in the referenced CAS
   Report, but Ledger replay ignores that authority and no command prints it.
3. **Reviewer semantics are discarded.** Disputes and benchmark demands are parsed and dropped;
   omission of a prior Finding is treated as a Drop even though no explicit disposition exists.
4. **A Rust `Debug` impl is load-bearing for convergence.** `publish_report` persists
   `format!("{verdict:?}")`, and the round counter reads it back with
   `.starts_with("Incomplete")`. Renaming `RunVerdict::Incomplete` makes incomplete rounds start
   *closing* rounds, with no compile error.
5. **Legacy grouping is used as identity.** The typed Report contract says path/title is only a
   hint, but the live Ledger still keys canonical state by `sha256(file + "|" + title)`.
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
- Dynamic scatter/semantic closure and internal derived-Snapshot Integration are restored as M8
  and M9 rather than silently omitted.

## Scope

In: this repository's Review Kernel crates, schemas, fixtures, CLI, and generic pipeline
contracts. Out: project-specific `.review/` pipelines, reviewer packages, campaign state, and
private corpora. The retired shell harness under `compat/legacy-harness/` remains only as the
executable specification that regenerates the synthetic fixture corpus.

**Sequencing is deliberate.** M0 freezes append-only contracts; M1 makes current evidence usable;
M2 adds the trusted Subject; M3 establishes canonical claim identity and explicit dispositions;
M4 builds snapshot-scoped Evidence and resolution on it. M5–M7 add operator, gate, and Proposal
capabilities. M8 and M9 then add dynamic execution and internal Integration only after their
identity, authority, isolation, and verification prerequisites exist.

## Acceptance criteria

- [x] M0 — every event type is a Rust/schema enum member; structural RunReport versions are
      permanent and additive; ports
      validate type/cardinality/snapshot affinity; exact invocation inputs and output receipts are
      persisted; legacy replay remains pinned by fixtures.
- [x] M1 — `af review show` prints every attached report whole; `ledger --long` carries body and
      fix; `report --format md` emits what SKILL.md §5 asks the agent to produce.
- [ ] M2 — Campaign authority and Base are pinned before candidate capture; committed and
      revalidated dirty heads produce wired diff Subjects; generic Git execution still refuses
      `diff`; both rename endpoints govern Report Scope and replay never rewrites existing legacy
      keys. Canonical path-independent Finding identity remains M3.1.
- [ ] M3 — the live reducer consumes typed Reports; new Findings have path-independent IDs;
      every assigned prior Finding has an explicit disposition; Grouping is reversible.
- [ ] M4 — required Demands block independently; Evidence is Demand/Subject-linked; `fixed` can
      result only from positive Fix Verification; non-fixed resolutions are scoped, expiring, and
      challengeable; convergence reads exact final Finding/Demand views.
- [ ] M5 — JSON/text reports are deliberate; spend is reported per Round/reviewer; Campaigns and
      Round history are enumerable.
- [ ] M6 — every executable node uses an admitted Execution Binding; smoke tests run with bounded
      sandbox-local Cache Snapshots; safe Attempts receive revocable Broker Handles rather than
      reusable credentials.
- [ ] M7 — a Proposal unequal to the sealed diff is refused; export is by Proposal ID; stale
      export requires explicit override.
- [ ] M8 — accepted SliceSets fan out losslessly under fan-out budgets; whole-Subject closeout and
      semantic-output closure prevent omitted shard output from passing.
- [ ] M9 — automatic Integration is opt-in, advances only an internal derived Snapshot at one
      transactional boundary, and leaves claims pending until a later verified Round.
- [ ] `make review-kernel` and `make review-kernel-fixtures` stay green throughout.

## Open work (resume here)

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
retained all eleven fixes, and opened six
claims. Their corrections reject unsupported generation contracts at load and receipt replay,
centralize Report and Change Set path semantics, restore only requested directory mode bits,
concurrently drain command-reviewer and Git pipes, bind command deadlines to captured Campaign
policy, and index recent authority failures for convergence. The 5,000-file / 195.3 MiB release
fixture records 0.861 s synthetic-tree construction with concurrent draining. The full kernel gate
passes; the claims are committed and resolved. Run Round 4 to retain them, then start a fresh
unchanged-policy Campaign because this one can no longer establish two clean Rounds.
Fresh Campaign `m2-rename-scope-final-verified` pins the same authority and manifest. Round 1 spent
826,568 tokens and opened nine claims. Their corrections version Manifest path encoding while
preserving raw-tree identity, make command deadlines safe after parent exit, enforce Change Set
bounds before adapters, keep runner presentation out of Round authority, document typed
Generation compatibility, avoid allocating discarded patches, carry one run-bound Ledger
projection, and transfer command input without copying it. ADR-0024 and ADR-0025 record the two
compatibility decisions. The full kernel gate passes; resolve these claims and run the next Round.
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
  `cargo run -p review-config --example lock -- .review/reviewers architecture performance` is
  the kernel repo's shipped generator. Replace `.review/review.lock` with its stdout atomically.
- **M7 makes reviewer rounds more expensive.** A model that writes code costs more than one that
  writes prose; every pipeline's `[budgets]` caps need re-deriving when it lands.
