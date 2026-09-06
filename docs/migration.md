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

Consumers that onboarded before `.af/` existed carried `.review/pipelines/*.toml`,
`.review/review.lock`, and `.review/reviewers/`. Since `v0.8.0` that layout is no longer read for
new Campaigns ([ADR-0043](adr/0043-drop-legacy-review-authority-in-v0-8-0.md), executed by
[ADR-0045](adr/0045-one-release-train-and-a-pin-that-binds-bytes.md)); `af review plan|run` refuse
a `.review/…` pipeline path and name the command that moves the policy. Stored Campaigns whose
manifests recorded `.review/…` paths stay replayable.

- `af onboard` on a repository with `.review/` and no `.af/` previews the `.af/` it becomes:
  every pipeline with the format upgrades it needs applied (comments intact; `heavy` becomes
  `review`, the `.af/` default), every reviewer package those pipelines reference byte for byte
  as a Worker package (unreferenced packages are named and left behind), a
  project file, and a lock pinning Workers, pipelines, and — when a receipted release runs the
  command — the `af` release itself. It writes nothing.
- `af onboard --migrate --apply` writes that `.af/` atomically and only when absent, leaving
  `.review/` in place. Review the diff, commit it on the trusted base branch, delete `.review/`,
  run `af onboard` to validate, then `af review plan`.
- Scaffolding `.af/` beside `.review/` is refused so a repository never carries two authorities.

Persisted `review.kernel/*` types, events, and stored Campaign replay are untouched. Consumers
run their pinned release's `af review plan` after every pin bump; the hub does this with `make
review-plan`, and the release workflow plans `fixtures/consumers/` with every built binary.
