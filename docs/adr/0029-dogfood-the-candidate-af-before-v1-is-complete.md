# Dogfood the candidate `af` before v1 is complete

**Status:** superseded by [ADR-0030](0030-complete-minimal-v1-and-v2-before-dogfood.md)

Released `v0.2.0` already reviews each milestone under ADR-0027, but that loop cannot validate the
candidate binary or the new token/context rules while the target architecture is being built. The
complete v1 migration is too large to remain the first useful feedback boundary.

Before M3.1, add a dogfood walking-skeleton gate. `make dogfood` builds the candidate `af` and uses
its non-interactive `af review` path to review a real change in this repository, alongside the
independent `make check` gate. The checkpoint deliberately reuses the repository's existing
`.review/` policy and proven v0.2.0 Store/pipeline/provider internals through narrow adapters: it
adds no new `.review/` key or `review.kernel/*` persisted contract. The later accepted breaking
migration moves declarations and names together when the complete v1 foundation is ready.

The first walking-skeleton change is reviewed by the pinned last-green `v0.2.0`. After the
checkpoint, each material architecture slice runs candidate dogfood and records candidate identity,
Subject, context size, token usage, findings, and typed outcome. If the candidate cannot start, the
last-green binary reviews the change and the candidate failure is retained as evidence.

## Considered options

- **Finish v1 before candidate dogfood.** Rejected because interface, context, and orchestration
  mistakes would accumulate behind a long migration with no realistic feedback.
- **Keep using only released v0.2.0.** Rejected because it reviews the source change but does not
  exercise the candidate coordinator or its new accounting rules.
- **Dogfood a compatibility-backed walking skeleton, then ratchet it (chosen).** Produces useful
  feedback immediately while final interfaces replace adapters slice by slice.

## Consequences

- The walking skeleton and its `make dogfood` acceptance precede the complete v1 migration and
  M3.1. Store and worker-protocol redesigns, crate renaming, `.af/`, TUI, and `implement` are not
  prerequisites for the first candidate review.
- Exit requires one real kernel change to complete candidate build, `make check`, candidate review,
  Finding disposition, and a final structured outcome with token/context measurements. A fixture
  demo alone does not count.
- Temporary adapters stay behind final interfaces, add no new legacy persisted contract, and have
  explicit removal work before v1. The existing frozen contracts remain unchanged while present.
- Candidate dogfood is not part of ordinary deterministic `make check`: it is an explicit,
  budgeted external-model gate. ADR-0027 still governs its standard reviewer and Round caps.
