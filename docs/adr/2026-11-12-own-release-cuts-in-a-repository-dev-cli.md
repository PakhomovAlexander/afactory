# ADR-2026-11-12-own-release-cuts-in-a-repository-dev-cli: Own release cuts in the developer CLI

**Status:** proposed (2026-11-12). Partially supersedes the preparation entry point of
[ADR-0045](0045-one-release-train-and-a-pin-that-binds-bytes.md) and the manual version declaration
of [the release contract decision](2026-11-09-review-release-contracts-and-pins-before-merge.md).
Protected checks, signing, platform smoke and separate pin adoption remain unchanged.

## Context

Release preparation previously inspected the caller before fetching main, admitted versions by
editing Python, and could not reconcile a lost branch/PR creation response. Execution authority
is not release source. Dirty caller files must contribute no release bytes.

## Considered options

- Add a product `af release` command: rejected; repository developer operations do not belong in
  the installed kernel CLI.
- Use model-driven release Tasks: rejected; ordinary release orchestration is deterministic.
- Maintain another shell executor beside a new CLI: rejected; divergent paths lose guardrails.
- Retarget an approved plan when main moves: rejected; approval and evidence name exact bytes.

## Decision

`./dev` is a small repository-owned dispatcher. Its first command is `release`, with `plan`,
`rc`, `status` and `resume`. `make release` and the historical shell entry point delegate to it.
One explicit `rc` authorizes normal release PR creation, required checks, normal squash merge,
and the existing protected tag/build/sign/publication workflow. There is no bypass or local
installation. Planning grants no publication authority; resume retains captured authorization.
Default `rc` and `resume` internally reconcile and advance to signed readiness, a real failure,
operator interruption or a bounded time limit. Waiting uses a 30-second poll interval and six-hour
invocation limit by default, configurable within explicit bounds. The lease spans the driver;
interruptions and timeouts preserve recoverable waiting receipts and exact effect intents. Status
and watch remain read-only; an explicit `--step` supports single reconciliation diagnostics.
Failed release jobs terminate waiting even before artifacts exist; downstream pin-only failures
remain independent. Matching validation receipts are reused on every reconciliation.

An owned private state directory outside source holds one immutable versioned plan, a bare Git
repository and an isolated candidate worktree. A nonblocking repository lease serializes callers
sharing that state. The plan names fetched main, repository, source features, version reservation,
compatibility metadata, exact validation policy and executor identity. Remote tags and releases
participate in RC selection. Explicit versions and source selectors cannot override a reserved
identity. Foreign branches, PRs, tags and asset bytes are refused.

Source/path admission precedes worktree creation. Source contract and harness checks precede
metadata edits. Preparation updates workspace and lock, collects fragments, generates an exact
version-to-current-fixture declaration and derives harness pins together. The declaration binds
reviewed schema/fixture bytes and refuses drift; it is neither a wildcard nor a version range.
Ant/Scenario persisted-state breakage is recorded in structured reviewed compatibility metadata.

Effect intents precede branch, PR, merge and publication observations. Restart reads remote state
before retry. Interrupted unsealed local preparation is reconstructed only in its registered owned
worktree; sealed candidate bytes and exact successful check identity are retained. Main movement
before merge fails closed; after merge, source ancestry and the complete candidate tree must match.
No evidence transfers to different source bytes.

Publication reconciles draft inventory and existing asset bytes without clobbering. Signed checksums
include a versioned provenance document binding source, repository, version and publication run.
The controller verifies signatures, every platform archive digest, provenance, tag and the exact
protected publication job. A pin job failure leaves publication readiness independent of pin
adoption. A normal pin-only PR still requires its own policy and human merge.

PR validation is independent of formally required status configuration. Policy version 3 binds
workflow/resolver/catalog source bytes and immutable workflow identity. The exact latest run's
attempt plan artifact is digest-checked and validated with the existing class resolver. Its tested
merge commit binds approved base, candidate and tree; REST head SHA remains the branch identity.
Historical event labels come from that retained plan, never current labels. Profile mismatches wait
for matching evidence. Every platform/class and hosted conditional job must be present; the existing
aggregate contract owns intentional skip outcomes. Paginated totals and unique identities establish
collection completeness, and an attempt recheck prevents accepting stale reruns. Successful existing
CI aggregates still own exact class receipts. Missing evidence waits; partial or rejected evidence
never authorizes merge. Bare and ref-qualified canonical workflow paths both require matching
workflow IDs, including signed-bundle recovery. The strict expected-base prerequisite remains.

## Consequences

Transport is not exactly once. Durable identity and reconciliation make retries safe, and conflicts
require operator investigation. Local visibility of secret names proves configuration presence;
only the protected runner can prove actual signing readiness. State JSON is bounded to 256 KiB;
there is one retained candidate and one downloaded artifact set per run. Operators retain unfinished
runs, and remove completed run directories and their checkout-local build products explicitly.
See [operations](../dev-release.md) for setup, examples and recovery limits.
