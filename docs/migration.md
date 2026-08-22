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
