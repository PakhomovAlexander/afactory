# AGENTS.md

This repository is **Afactory**, a future multi-agent coding factory. Its current product surface
is the deterministic Review Kernel exposed as `af review ...`.

Before changing behavior, read:

- [`CONTEXT.md`](CONTEXT.md) for canonical domain vocabulary.
- [`docs/workstream.md`](docs/workstream.md) for current status and resume point.
- [`docs/backlog.md`](docs/backlog.md) for the dependency-ordered M0-M9 roadmap.
- [`docs/adr/README.md`](docs/adr/README.md) for binding design decisions.

M2.1-M2.6, minimal product v1/v2, and the first candidate implementation dogfood are complete and
verified. M3.1 and M3.2 are complete; M3.2's lightweight dogfood Findings are fixed and the full
gate passes. V3.1 local delivery is
implemented, proven against a real trusted repository, and passed a fresh pinned correctness
Campaign; its reported minor corrections are implemented and verified. New Campaigns use the
bounded correctness-review policy in ADR-0027 rather than extending the retired two-specialist
clean-window Campaign.
V3.2 `af onboard` shipped in private release `v0.4.0` from exact `main` commit `bb9e5a3`; its
supported release archives and checksum sidecars were downloaded and verified.
Product rebranding must not rename
`.review/`, `review.kernel/*` artifact types, persisted events, or established Review Kernel
domain terms until a separate accepted migration ADR supersedes this rule. v1 adds the final
user-facing `af` and `.af/` surface without physically renaming those internals. Project-specific pipelines,
reviewer packages, campaign state, and private corpora belong in consuming repositories, not here.
Use the pinned Rust toolchain and keep `make check` green. Never weaken a contract, fixture, gate,
budget, or sandbox boundary to make a test or review pass.

## Invariants

- A rename-limit warning does not erase a complete diff Subject: preserve the full Add/Delete
  path set, record truncated rename linkage, and keep the fixed limit in the diff-policy identity
  ([ADR-0017](docs/adr/0017-record-rename-truncation-and-continue.md)).
- Retry-only reviewer input is durable invocation authority: publish it as an Attempt input before
  dispatch, and publish produced feedback separately from terminal diagnostics; never derive a
  retry prompt from process memory or diagnostic prose, or mutate a frozen dispatch event
  ([ADR-0022](docs/adr/0022-persist-retry-feedback-as-attempt-input.md),
  [ADR-0023](docs/adr/0023-separate-retry-feedback-from-terminal-diagnostics.md)).
- Manifest path spellings declare their encoding generation; legacy artifacts remain readable,
  and representation upgrades do not change raw-tree content identity
  ([ADR-0024](docs/adr/0024-version-manifest-path-encoding.md)).
- Built-in Generation outputs are explicitly typed even in pipeline version 2; never restore an
  opaque output shape that the executor cannot dispatch
  ([ADR-0025](docs/adr/0025-require-typed-generation-outputs-in-version-2.md)).
- Process supervision shared across architectural layers lives in the dependency-neutral
  `review-process` leaf; source capture, gates, and sandbox providers do not depend on reviewer
  adapters or fork deadline, process-group, stdin, and pipe-drain semantics per consumer
  ([ADR-0026](docs/adr/0026-share-process-supervision-through-a-leaf-crate.md)).
- Wise token use and minimum Worker context are the first two design values. Every model call has
  a bounded reservation and named informational purpose; every Attempt carries an exact context
  manifest. Parent transcripts, whole Ledgers, repository dumps, unrelated documents, and other
  Workers' private reasoning are absent by default; bounded Tool retrieval is recorded
  ([ADR-0028](docs/adr/0028-prioritize-wise-token-use-and-minimum-worker-context.md)).
- Complete minimal v1 and v2 before candidate dogfood. v1 is final local review; v2 is sequential
  implementation ending at a verified internal Snapshot with no working-tree, branch, or PR
  delivery. Scale and optional integrations are v3; `make check` remains independent
  ([ADR-0030](docs/adr/0030-complete-minimal-v1-and-v2-before-dogfood.md)).
- V3.1 delivery accepts only a verified Task whose target is clean and exactly matches its source
  Snapshot. It creates only a new local branch/worktree after explicit Task-ID confirmation,
  persists recovery state, and never commits, pushes, opens a PR, invokes a remote, or overwrites
  an existing branch or path
  ([ADR-0031](docs/adr/0031-deliver-verified-tasks-to-new-local-worktrees.md)).
- `af onboard` is deterministic and token-free. It may atomically create only an absent `.af/`
  authority bundle; it never overwrites existing policy, invents or hand-types lock digests,
  executes Gates, accesses credentials, or publishes repository changes. Emitted authority is
  project-owned and becomes trusted only after review and commit on an Authority Snapshot
  ([ADR-0032](docs/adr/0032-generate-review-authority-with-af-onboard.md)).
- Trusting configured Worker authority and intentionally running `af review run` or `af task
  start` authorizes delivery of each Worker's exact declared inputs for every retry and later
  Round or stage in that Campaign or Task. Do not ask for per-call confirmation; undeclared
  context, changed bindings, delivery, publication, and remote side effects remain unauthorized
  ([ADR-0033](docs/adr/0033-configured-workers-authorize-declared-input-delivery.md)).
- Admitted reviewer results may be shown as recorded, not gathered evidence when required sibling
  output is missing, but they never become a partial Ledger, satisfy Semantic Closure, or support
  convergence ([ADR-0034](docs/adr/0034-surface-partial-results-without-ledger-authority.md)).
- Every milestone receives external `af review`, but the standard dogfood policy uses one
  high-effort correctness reviewer, one required clean round, and at most two rounds; architecture
  or performance audits are explicit exceptions
  ([ADR-0027](docs/adr/0027-use-one-correctness-reviewer-per-milestone.md)).
