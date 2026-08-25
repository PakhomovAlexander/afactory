# AGENTS.md

This repository is **Afactory**, a future multi-agent coding factory. Its current product surface
is the deterministic Review Kernel exposed as `af review ...`.

Before changing behavior, read:

- [`CONTEXT.md`](CONTEXT.md) for canonical domain vocabulary.
- [`docs/workstream.md`](docs/workstream.md) for current status and resume point.
- [`docs/backlog.md`](docs/backlog.md) for the dependency-ordered M0-M9 roadmap.
- [`docs/adr/README.md`](docs/adr/README.md) for binding design decisions.

M2.1-M2.6 are implemented; the `m2-rename-scope` dogfood Campaign must converge before M3.1.
Product rebranding must not rename
`.review/`, `review.kernel/*` artifact types, persisted events, or established Review Kernel
domain terms. Project-specific pipelines, reviewer packages, campaign state, and private corpora
belong in consuming repositories, not here.

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
