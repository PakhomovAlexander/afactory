# Review Kernel — architecture decisions

Decisions about the kernel's own design. These are distinct from
[`../../../../docs/adr/`](../../../../docs/adr/), which is a generated hub's own decision log —
the kernel ships *into* such a hub, so its decisions travel with the code rather than with the
project using it.

Same rules as the hub's: one decision per file, numbered, **immutable once accepted**. A changed
decision becomes a new ADR marked *superseded by* the old one, with links both ways. Record the
options you rejected and why — that is the part future-you needs.

## Index

- [0001 — Compute the Change Set with `git diff`, reachable only through a typed
  method](0001-tree-diff-behind-a-typed-method.md)
- [0002 — A payload shape change bumps the event type
  version](0002-event-payload-changes-bump-the-type-version.md)
- [0003 — Gate checks may reach host caches through a root-allowlisted
  passthrough](0003-gate-caches-pass-through-to-the-host.md)
- [0004 — Reviewers author patch proposals; the kernel verifies, git
  applies](0004-reviewers-author-verified-patch-proposals.md)
- [0005 — Report artifacts are authoritative for finding
  projections](0005-report-artifacts-are-projection-authority.md)
- [0006 — Finding identity is independent of path and
  title](0006-finding-identity-is-path-independent.md)
- [0007 — Demands are independent blocking
  obligations](0007-demands-are-independent-blocking-obligations.md)
- [0008 — Safe caches are sandbox-local
  snapshots](0008-safe-caches-are-sandbox-local-snapshots.md)
- [0009 — Campaign authority is resolved before candidate
  capture](0009-campaign-authority-is-base-pinned.md)
- [0010 — Proposals are exported by ID and remain bound to their base
  Snapshot](0010-proposals-are-exported-by-id-and-base-bound.md)
- [0011 — Silence is not a Drop](0011-silence-is-not-a-drop.md)
- [0012 — Fixed requires current-Subject
  verification](0012-fixed-requires-current-subject-verification.md)
- [0013 — Scope is evaluated per active Report
  claim](0013-scope-is-evaluated-per-active-claim.md)
- [0014 — Non-fixed resolutions are scoped and
  challengeable](0014-non-fixed-resolutions-are-challengeable.md)
- [0015 — Safe attempts receive handles, not reusable
  secrets](0015-safe-attempts-receive-handles-not-secrets.md)
- [0016 — Provider preflight is a fenced, charged
  operation](0016-provider-preflight-is-a-fenced-operation.md)
- [0017 — Record rename truncation and continue the diff
  Subject](0017-record-rename-truncation-and-continue.md)
- [0018 — Share one bounded infrastructure
  executor](0018-share-one-bounded-infrastructure-executor.md)
- [0019 — Report authority failures
  explicitly](0019-report-authority-failures-explicitly.md)
- [0020 — Stream CAS materialization and clone duplicate
  files](0020-stream-cas-materialization-and-clone-duplicates.md)
- [0021 — Keep the ReviewerResult wire shape
  flat](0021-keep-reviewer-result-wire-shape-flat.md)
- [0022 — Persist retry feedback as an Attempt
  input](0022-persist-retry-feedback-as-attempt-input.md)
- [0023 — Separate retry feedback from terminal
  diagnostics](0023-separate-retry-feedback-from-terminal-diagnostics.md)
- [0024 — Version manifest path encoding without changing Snapshot content
  identity](0024-version-manifest-path-encoding.md)
- [0025 — Require typed Generation outputs in pipeline version
  2](0025-require-typed-generation-outputs-in-version-2.md)
- [0026 — Share process supervision through a leaf
  crate](0026-share-process-supervision-through-a-leaf-crate.md)
- [0027 — Use one correctness reviewer per
  milestone](0027-use-one-correctness-reviewer-per-milestone.md)
- [0028 — Prioritize wise token use and minimum Worker
  context](0028-prioritize-wise-token-use-and-minimum-worker-context.md)
- [0029 — Dogfood the candidate `af` before v1 is
  complete](0029-dogfood-the-candidate-af-before-v1-is-complete.md)
- [0030 — Complete minimal v1 and v2 before candidate
  dogfood](0030-complete-minimal-v1-and-v2-before-dogfood.md)
- [0031 — Deliver verified Tasks only to new local
  worktrees](0031-deliver-verified-tasks-to-new-local-worktrees.md)
- [0032 — Generate review authority with
  `af onboard`](0032-generate-review-authority-with-af-onboard.md)
- [0033 — Treat configured Workers as authorization for declared input
  delivery](0033-configured-workers-authorize-declared-input-delivery.md)
- [0034 — Surface partial results without granting Ledger
  authority](0034-surface-partial-results-without-ledger-authority.md)
- [0035 — Address Campaign state by opaque
  ID](0035-address-campaign-state-by-opaque-id.md)
- [0036 — Resolve Gate caches through machine-local bounded
  policy](0036-resolve-gate-caches-through-machine-local-bounded-policy.md)
- [0037 — Default Campaigns to one-Round light
  review](0037-default-campaigns-to-one-round-light-review.md)
- [0038 — Transport Proposal declarations beside Reviewer
  Results](0038-transport-proposals-beside-reviewer-results.md)
- [0039 — Own dynamic shards inside a typed Scatter
  node](0039-own-dynamic-shards-inside-a-typed-scatter-node.md)
- [0040 — Promote only checked derived
  Snapshots](0040-promote-only-checked-derived-snapshots.md)
- [0041 — Make review selectors explicit and refuse empty
  Diffs](0041-make-review-selectors-explicit-and-refuse-empty-diffs.md)
- [0042 — Require Provider bindings and isolate Claude
  reviewers](0042-require-provider-bindings-and-isolate-claude-reviewers.md)
