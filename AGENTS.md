# AGENTS.md

This repository is **Afactory**, a future multi-agent coding factory. Its current product surface
is the deterministic Review Kernel exposed as `af review ...`.

Before changing behavior, read:

- [`CONTEXT.md`](CONTEXT.md) for canonical domain vocabulary.
- [`docs/workstream.md`](docs/workstream.md) for current status and resume point.
- [`docs/backlog.md`](docs/backlog.md) for the dependency-ordered M0-M9 roadmap.
- [`docs/adr/README.md`](docs/adr/README.md) for binding design decisions.

M2.5 is complete; bounded, resumable provider authentication operations come before M2.6.
Product rebranding must not rename
`.review/`, `review.kernel/*` artifact types, persisted events, or established Review Kernel
domain terms. Project-specific pipelines, reviewer packages, campaign state, and private corpora
belong in consuming repositories, not here.

Use the pinned Rust toolchain and keep `make check` green. Never weaken a contract, fixture, gate,
budget, or sandbox boundary to make a test or review pass.
