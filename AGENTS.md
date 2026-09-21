# AGENTS.md

This repository is **Afactory**: the `af` CLI, a multi-agent coding factory whose first capability
is the deterministic Review Kernel behind `af review`, and whose current increment adds
implementation Tasks behind `af task`. Reviewers and implementers only ever mutate a sandbox; the
kernel integrates; humans publish.

Before changing behaviour, read [`CONTEXT.md`](CONTEXT.md) for the canonical vocabulary and
[`docs/adr/README.md`](docs/adr/README.md) for the binding decisions; [`docs/README.md`](docs/README.md)
maps the rest of the documentation. Use the pinned Rust toolchain (`rust-toolchain.toml`) and keep
`make check` green. Never weaken a contract, fixture, gate, budget, or sandbox boundary to make a
test or review pass. Project-specific pipelines, reviewer packages, Campaign state, and captured
review corpora belong in consuming repositories, not here.

- Product rebranding must not rename `review.kernel/*` artifact types, persisted events, or
  established Review Kernel domain terms until a separate accepted migration ADR supersedes this
  rule. Releases are cut only through `make release` and the release workflow; a lock pins the
  release's bytes, not just its version
  ([ADR-0045](docs/adr/0045-one-release-train-and-a-pin-that-binds-bytes.md)).
- Compatibility obligations start at GA: never add a reader, migration, fallback or replay path
  for state, configuration or flags that only a pre-GA (0.x) release wrote; such state is
  unsupported and is discarded on upgrade
  ([ADR-0113](docs/adr/0113-ga-reads-only-what-ga-writes.md)). Self-management still installs and
  dispatches to pinned releases from 0.8.0 on.

## Invariants

- The next increment makes Task the common execution abstraction. Every Pipeline has a public
  input/output contract, including embedded and generated Pipelines; newly generated plans
  require developer review and exact-plan approval before execution. The implementation status
  and compatibility checkpoints are recorded in [docs/task-execution.md](docs/task-execution.md).
  Captured Review operations use common Task Attempts, preserve canonical domain receipts and
  require the Review verdict for acceptance
  ([ADR-0077](docs/adr/0077-run-captured-review-operations-under-common-task-attempts.md)).
- Task-backed Review conclusions carry exact cumulative accounting and a checked Task log
  prefix; report snapshots are never summed as independent spend
  ([ADR-0078](docs/adr/0078-bind-review-conclusions-to-exact-task-accounting.md)).
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
- Review Campaigns are light by default: one closed Round, then fix concrete Findings and run the
  deterministic project gate. Do not start a follow-up Campaign. Use `--heavy` only when a human
  explicitly requests convergence review; the selected effective convergence authority is pinned
  and cannot change on resume
  ([ADR-0037](docs/adr/0037-default-campaigns-to-one-round-light-review.md)).
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
- Default Campaign state is addressed by a domain-separated opaque ID derived from its validated
  label, and resolution must remain beneath the configured root. Enumeration also lists an
  explicit `--state` directory named by its label; one Campaign under both names and symlinked
  enumeration fail closed ([ADR-0035](docs/adr/0035-address-campaign-state-by-opaque-id.md)).
- Every milestone receives external `af review`, but the standard dogfood policy uses one
  high-effort correctness reviewer, one required clean round, and at most two rounds; architecture
  or performance audits are explicit exceptions
  ([ADR-0027](docs/adr/0027-use-one-correctness-reviewer-per-milestone.md)). Review records state
  the Campaign's wall-clock, per-Attempt provider usage, and Finding dispositions exactly as
  `af review report` prints them.
- Proposal declarations travel beside, never inside, the persisted flat Reviewer Result. The
  kernel verifies one declaration against the complete sealed sandbox diff, durably prepares it
  with the selected Attempt, and publishes `PatchProposal@1` only after canonical Report IDs exist
  ([ADR-0038](docs/adr/0038-transport-proposals-beside-reviewer-results.md)).
- Dynamic shards do not rewrite the planned DAG. Pipeline v5 persists a complete Slice Set before
  a typed Scatter owns tagged sub-invocations, and carries every shard outcome through a lossless
  Shard Set to whole-Subject closeout and semantic closure
  ([ADR-0039](docs/adr/0039-own-dynamic-shards-inside-a-typed-scatter-node.md)).
- Automatic Integration composes only selected, sealed, disjoint Proposal Manifests. It checks an
  unpromoted `Capture::Derived` Snapshot and advances only the internal Campaign head plus
  pending-verification claims in one transaction; branch and PR publication stay outside the
  kernel ([ADR-0040](docs/adr/0040-promote-only-checked-derived-snapshots.md)).
- Diff review resolves policy, Base, and candidate selectors independently. An empty typed Change
  Set is refused before Gates, Provider operations, or Workers; it is never reported as a clean
  review ([ADR-0041](docs/adr/0041-make-review-selectors-explicit-and-refuse-empty-diffs.md)).
- Every packaged model Worker has an explicit admitted Provider binding. Runner adapters own
  their security flags; candidate project settings and Hooks cannot widen reviewer authority
  ([ADR-0042](docs/adr/0042-require-provider-bindings-and-isolate-claude-reviewers.md)).
