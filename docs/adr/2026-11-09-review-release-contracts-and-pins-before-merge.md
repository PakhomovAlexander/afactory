# ADR-2026-11-09-review-release-contracts-and-pins-before-merge: Review release contracts and pins before merge

**Status:** accepted (2026-11-09). Partially supersedes
[ADR-0045](0045-one-release-train-and-a-pin-that-binds-bytes.md) and
[ADR-0138](0138-the-repository-pins-its-newest-release.md), replacing the single human act and
post-publication direct push. Supersedes
[ADR-0152](0152-push-the-release-pin-with-a-deploy-key-that-bypasses-mains-ruleset.md), retained
as historical rationale. Signed archive verification and the pin-only boundary remain binding.

## Context

Fast and `no-perf` release PRs skip performance tests. A workspace bump can therefore merge
without an explicit compatible telemetry fixture declaration. The current `0.15.1-rc.1`
source writes `af/TaskCheckReceipt@2`, but the fixture contract did not name it. Lab parity also
mistook four Rust publication schemas for generated lab schemas because they share a prefix.

Signed release digests exist only after publication. Pushing the subsequent pin directly to
`main` bypasses its PR validation, and a deploy key with ruleset bypass grants broader write
authority than a pin needs. A PR created using `GITHUB_TOKEN` does not start its normal CI.

## Considered options

- Keep release contracts in perf tests only: fast releases would still discover drift too late.
- Ignore unknown versions or all unfamiliar schemas: hides undeclared workloads and retired lab
  schemas, weakening both contracts.
- Keep the deploy-key bypass: accepts validation only after changing protected `main`.
- Use `GITHUB_TOKEN` for the pin PR: checks would not run from its push/PR events.
- Use a repository-scoped fine-grained PAT for a normal PR: selected, accepting a second human
  merge and explicit credential provisioning.

## Decision

1. `scripts/release-contract.py` runs offline in the always-run `release-check`, included in
   `base-check` and the full local check, regardless of selected classes or `no-perf`.
   `scripts/release.sh` runs it after updating the workspace version and before committing,
   pushing or opening the release PR. It requires the exact version to select the current
   committed telemetry fixture contract. Unknown versions still fail measurement admission.
2. Lab parity checks exactly its generated schema names and contents. Only the four exact
   Rust-owned names in `contracts.RUST_OWNED_SCHEMAS` are excluded. Missing, stale and retired
   lab schemas fail; `schemas.py --write` refuses unknown/retired files without deleting them.
   Rust validation continues to own the publication schemas.
3. After publication, `scripts/pin-release.sh VERSION --pr` prepares only `.af/af.lock` on
   current `origin/main`, using the published binary's signed digest refresh. It checks every
   target digest and that only `[af]` and `[af.digests]` changed, commits, pushes only
   `release/pin-vVERSION`, and opens a normal PR to `main`. It never pushes to `main`,
   force-pushes, merges or uses a bypass. The job retains its stable performance-handoff display
   identity, `pin the release in .af/af.lock`; success means PR preparation, not a merged pin.
   The local mode without `--pr` still prepares a manual PR.
4. There is at most one branch and PR per version. All PR states are inspected; an open PR with
   the exact branch head is reused. Closed, ambiguous or divergent PRs are refused. A branch
   left by failed PR creation can be reused if it is pin-only and based on current main. A
   racing PR creator is checked once after creation fails. No second PR is created.
5. If main moves during preparation before a branch push, preparation restarts at most three
   times. After branch publication, or for an existing branch based on older main, the script
   refuses and asks for human review/rebase rather than rewriting published history. A final
   main-tip check precedes PR creation; later main advances are handled by normal PR review
   and CI. A main that already pins the version with every target digest is a no-op; a newer main pin
   is never downgraded.
6. The pin job requires the repository secret `RELEASE_PIN_TOKEN`: a fine-grained personal
   access token restricted to this repository with **Contents: read and write** and
   **Pull requests: read and write** (Metadata read is implicit). Its owner needs repository
   write access. It needs no Administration, Actions, Workflows or ruleset-bypass permission.
   Checkout uses read-only `GITHUB_TOKEN` with `persist-credentials: false`; branch writes
   and `gh` use the PAT. No fallback to a workflow token, deploy key or ambient helper is allowed.
   Missing credentials fail before installation. Failed push/PR creation refuses clearly and
   never reports main as pinned. A retained branch can be retried after credentials are repaired.

## Consequences

Publication requires the release PR merge; adopting its pin requires a second human merge after
CI. `af-pin.py --check` still accepts the newest or preceding non-yanked release while that pin
PR awaits merge, and still checks every target digest. Operators must merge the pin PR before
another release would exceed that one-release allowance. The pin PR uses `release/` so the
existing release-automation Task-report exemption applies; normal CI and PR line accounting run.

Provision and rotate `RELEASE_PIN_TOKEN` as described in `CONTRIBUTING.md`. Remove the retired
`PIN_DEPLOY_KEY` secret and write deploy key and remove deploy-key bypass from `main-protect` in
repository settings. Those settings changes are operator actions, not performed by this source
change. No release automation needs a main bypass. Revocation fails closed and leaves a published
release available to consumers while this repository awaits its pin PR merge.
