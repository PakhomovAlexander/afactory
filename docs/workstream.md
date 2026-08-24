# Afactory Review Kernel - capability work (M0-M9)

**Status:** M0 and M1 are complete. M2 dogfood Round 3 retained 24 fixes and opened nine further
claims; fix its Ledger to convergence before resuming M3.1.
**Goal:** `af review` reviews a *change* rather than a whole tree, and every finding it produces
can be read, triaged, and closed only through explicit evidence-bearing policy.
**Log:** [`workstream/log.md`](workstream/log.md)

## Summary

A second design audit on 2026-08-20 recovered the original accepted Review Kernel design from
the originating RawTree hub and challenged the six-milestone reconstruction against it and the
current contracts. The corrected roadmap has M0–M9 and sixteen ADR records; ADR-0003 and ADR-0004
are superseded. M0 and M1 are complete. M2.1-M2.6 reached dogfood; its first three Rounds opened
33 Findings while retaining every prior correction. The repository/release
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

- [x] M0 — every event type is a Rust/schema enum member; `RunReport@2` is structural; ports
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
without rewriting the existing Finding key. Dogfood Rounds 1 and 2 produced 24 Findings whose
corrections are committed and fully verified. Round 3 retained all 24 as fixed and opened nine
further claims. Their replay/admission and sandbox-scalability corrections are locally implemented;
full verification, markdownlint, and the corrected 5,000-entry measurement pass. Commit and resolve
them in Campaign `m2-rename-scope`, then continue to convergence. Only then
resume M3.1. Continue in milestone order; do not pull Proposal or scatter work forward past
Subject, authority, isolation, and verification prerequisites.

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
