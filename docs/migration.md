# Afactory repository migration

## Decisions

- Product and repository: **Afactory** (`PakhomovAlexander/afactory`).
- Executable: **`af`**.
- Current namespace: **`af review ...`**.
- Review Kernel vocabulary, `.review/`, schemas, events, and campaign state remain stable.
- Project Hub consumes a pinned private release; it does not vendor this Rust workspace.

## Sequence

1. Preserve the extracted history and make fixtures self-contained.
2. Establish `af review`, CI, and private draft releases.
3. Release `v0.1.0` with checksummed Linux and macOS binaries.
4. Add a pinned launcher and parity checks to Project Hub.
5. Remove the embedded kernel only after parity succeeds.
6. Resume capability work at M2.5.

## Private distribution

Local consumers use existing `gh` authentication. Trusted CI uses a read-only token. Release
artifacts are cached outside consuming repositories and verified against a committed lock. No
credential is written into a project, pipeline, reviewer package, or release artifact.

## Legacy `.review/` policies

Consumers that onboarded before `.af/` existed still carry `.review/pipelines/*.toml`,
`.review/review.lock`, and `.review/reviewers/`. That layout stays accepted: `af review plan|run`
read it through the same authority layer, and `af onboard` now recognizes it.

- `af onboard` on a repository with `.review/` and no `.af/` validates every pipeline and the
  lock against this release's pipeline format and names each pending upgrade
  (status `legacy` when nothing is pending, `legacy-outdated` otherwise). It writes nothing.
- `af onboard --migrate --apply` rewrites each outdated pipeline in place. Upgrades are
  additive and idempotent — today the only one adds the `review.kernel/DemandSet@1` Ledger
  output that M4 made mandatory — and never touch reviewer packages, budgets, convergence,
  checks, or edges. The lock pins only reviewer packages, so it does not change. Review and
  commit the diff on the trusted base branch, then `af review plan`.
- Scaffolding `.af/` beside `.review/` is refused so a repository never carries two
  authorities. Moving to `.af/` is a deliberate step: remove `.review/`, then `af onboard --apply`.

Deprecation: [ADR-0043](adr/0043-drop-legacy-review-authority-in-v0-8-0.md) drops `.review/`
authority in `v0.8.0`; that release ships `af onboard --migrate` converting a `.review/`
repository into `.af/` ([#52](https://github.com/PakhomovAlexander/afactory/issues/52)). Persisted
`review.kernel/*` types, events, and stored Campaign replay are untouched. Until then, consumers
run their pinned release's `af review plan` after every pin bump; the hub does this with `make
review-plan`, and the release workflow plans `fixtures/consumers/` with every built binary.
