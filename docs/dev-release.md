# Repository release automation

`./dev release` is developer tooling, separate from the installed `af` product. It invokes no
models or AF implementation Tasks. `make release` uses the same implementation. It prepares a
release of exact fetched main, regardless of dirty files in the invoking checkout.

## Setup and authorization

Install Git, Python 3, the pinned Rust toolchain with nextest, Node/npx and minisign. Authenticate
`gh` as an approved repository writer. GitHub access needs normal Contents and Pull requests write,
plus read access to repository secret **names** for observable configuration preflight. No secret
values are read. Configure `MINISIGN_SECRET_KEY` and the scoped `RELEASE_PIN_TOKEN` on the protected
runner as described in [contributing](../CONTRIBUTING.md). Preflight checks their presence; it
cannot establish whether a key is usable or whether branch policy will permit a later merge.
The release label `release:fast` must already exist. Required checks and branch protection apply;
the CLI has no admin or ruleset bypass mode.

Use an owned mode-0700 state directory outside any source checkout. The default is
`$XDG_STATE_HOME/afactory-release/OWNER--REPO` (falling back to `~/.local/state`). Share that directory
between concurrent callers on one machine. A run retains its reservation and never automatically
selects another version when resumed. Use `rc --new` for a subsequent release after the existing release is ready, or a distinct
state directory. `--new` archives the terminal receipt (up to sixteen retained) and reclaims the
completed candidate/assets; retain unfinished runs until reconciliation completes. There is no background daemon.

```sh
./dev release plan --repo PakhomovAlexander/afactory --format json
./dev release rc --repo PakhomovAlexander/afactory
./dev release status --repo PakhomovAlexander/afactory --format markdown
./dev release status --repo PakhomovAlexander/afactory --watch
./dev release resume --repo PakhomovAlexander/afactory
```

`rc` is explicit bounded authorization to prepare/push one release branch, create its normal PR,
wait for required checks, normally squash-merge that exact head and observe the existing protected
release workflow. `plan` and `status` publish nothing. A pending check returns a waiting status;
run `resume` later. Read-only watch refreshes retained local receipts and never drives work.
JSON includes the immutable plan and receipts; Markdown shows stages, elapsed seconds, next actions
and URLs. `--source SHA` optionally asserts the exact fetched main. `--profile full` selects full validation (fast is the default).
`--version X.Y.Z-rc.N` asserts
an exact unused RC on the current base; neither selector retargets an existing run.

The optional `--remote /absolute/path/to/local.git` supports disposable local Git transports;
GitHub identity still requires explicit `--repo`. Production defaults to matching credential-free
GitHub HTTPS. State excludes credentials and raw transport diagnostics.

## Source and compatibility

The controller and structured `scripts/release-policy.json` must match fetched source. Required
feature paths and tokens enforce current Ant/Scenario inclusion before materialization. The policy
states reviewed compatibility and exact validation commands. Missing compatibility information is a
planning prerequisite, never model-generated advice. The current metadata states the breaking
persisted Ant/Scenario shapes: finish/discard older runs and state before upgrade. There is no
migration or old-state replay promise. Authority lock refresh and pin adoption remain explicit.

`scripts/release-contract.json` names exactly one version and reviewed schema/telemetry-fixture
digest. Preparation changes its version and captured source SHA only after validating the same
capabilities. New compatible RCs need no Python list edit. Changed or unknown capabilities require
reviewing the declaration and actual schemas/fixtures together; never copy a digest to suppress an
incompatibility. Source and prepared harness checks are executable receipts, including fast releases.

Preparation updates the workspace manifest, workspace lock, collected changelog fragments, exact
contract declaration and derived harness pins together. Only declared metadata paths can change.
The isolated candidate has checkout-local `target/`; dirty caller files are not copied. Validation
reuse requires the same candidate, commands, Python/platform and Rust/Cargo identity. Platform gates,
archive onboard/plan smoke and signature verification remain owned by `release.yml`.

## Recovery and readiness

State records effect intents and observations atomically before/after effects. A failed network
reply can mean an effect succeeded. `resume` first reconciles existing branch, all-state PR, merge,
tag and release inventory; it does not promise exactly-once transport. Never delete a run merely
because its last command failed. Interrupted unsealed preparation can be reconstructed from pinned
source in the owned registered worktree. Foreign state is refused. Publication reruns retain matching
assets and verify an existing signature instead of overwriting assets.

Main moving before merge produces `stale_main`: the approved source stays fixed. Review the new
source and use a separate plan/state directory after investigating any already-published branch/PR;
there is no automatic rebase. A merged release must have the candidate's exact tree and source
ancestry. The tag resolver also checks the approved first parent and metadata-only diff before
creating a tag, so a main advance racing merge cannot silently publish different source. Failed checks retain their failure details; repair the cause under normal policy before
resuming. A different branch head, tag, PR base, signed source or archive digest requires operator
reconciliation, never force-push or clobber.

A non-draft release is ready only after every signed platform digest, signed source/version
provenance and exact-source protected publication job verify. Missing/draft assets mean waiting or
failure, not success. The publication job joins all platform check/build jobs. Its pin job can fail
without erasing a ready-to-test RC. JSON reports `published` separately from `pin`; no local install,
pin merge or silent adoption occurs. Review the normal pin-only PR independently.

The JSON receipt is bounded to 256 KiB. Each run retains one mirror, one candidate with build
products and one downloaded release asset set. Once the release and pin have been independently
reviewed, archive the receipt and remove the completed owned run directory to reclaim its build
products. Do not remove a directory while a controller holds its lease.

## Hermetic development checks

```sh
python3 -I scripts/test-dev-release.py
python3 -I scripts/test-release-contract.py
python3 -I scripts/test-release-resolve.py
```

Tests use disposable local Git remotes and a fake GitHub transport; they never mutate live GitHub,
read secrets, run paid models or install a release. They run in the normal fast gate's always-on
release checks. The architecture decision is
[ADR-2026-11-12-own-release-cuts-in-a-repository-dev-cli](adr/2026-11-12-own-release-cuts-in-a-repository-dev-cli.md).
