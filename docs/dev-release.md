# Repository release automation

`./dev release` is developer tooling, separate from the installed `af` product. It invokes no
models or AF implementation Tasks. `make release` uses the same implementation. It prepares a
release of exact fetched main, regardless of dirty files in the invoking checkout.

## Setup and authorization

Install Git, Python 3, the pinned Rust toolchain with nextest, Node/npx and minisign. Authenticate
`gh` as an approved repository writer. GitHub access needs normal Contents and Pull requests write,
plus read access to repository secret **names** for observable configuration preflight. No secret
values are read. Configure `MINISIGN_SECRET_KEY` and the scoped `RELEASE_PIN_TOKEN` on the protected
runner as described in [contributing](../CONTRIBUTING.md). Only the signing secret is a publication prerequisite. Missing pin credentials are reported
independently as `CREDENTIAL_MISSING`; the RC can still become ready. Preflight
cannot establish whether a key is usable.
The selected release label (`release:fast` or `release:full`) must already exist. Required checks and branch protection apply;
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
release workflow. `rc` and `resume` drive the same authorized run through checks, merge and signed publication in
one foreground invocation. They reconcile at most once every 30 seconds while waiting, with a
six-hour default time limit including subprocess commands. `--timeout SECONDS` accepts 1–86400;
`--poll-interval SECONDS` accepts 1–300. There is no busy loop. The repository lease remains held
while driving, so concurrent callers cannot dispatch another effect. `--step` explicitly performs
one reconciliation and may return a waiting status; it is intended for diagnostics and tests.

Timeout exits 124; Ctrl-C or SIGTERM exits 130. Both retain a waiting receipt with a next action,
exact authorization and release identity, rather than marking a normal wait failed. Resume after
an interruption or time limit with `./dev release resume`; successful exact validation receipts
are reused. A timeout during an effect can leave its result ambiguous: resume reconciles remote
state before retrying. Real failed checks, conflicts or prerequisites exit 1 and retain failure
details. Failed release build/check/resolve/publication jobs stop the driver even if no tag or
release exists; a completed workflow without a successful publication join also fails. An explicit
operator workflow rerun can recover without changing release identity. Pin-only job failure stays
independent of publication readiness.

`plan` and `status` publish nothing. Read-only watch refreshes retained local receipts and never
drives work.
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

## Exact PR validation and expected base prerequisite

The source-bound `scripts/release-policy.json` declares CI independently of branch-required
statuses. Its workflow files and resolver are digest-bound; a workflow change needs policy review.
The controller waits for the latest exact-head `pull_request` CI run associated with this PR and
approved main base. It downloads that run's `validation-plan-ATTEMPT` artifact, verifies the advertised
archive SHA256, and validates the plan through the existing resolver and source-bound catalog. The
retained event plan must name this release version, context, authorized profile and exact classes;
current PR labels cannot establish what an earlier run executed. An opened fast run while a full
label event has yet to arrive remains waiting, as does a missing or not-yet-retained attempt plan.
A cancelled run without a retained plan cannot establish its profile and keeps waiting; once its
plan identifies the authorized profile, cancellation or failure rejects the checks.

REST run and job `head_sha` identify the branch candidate. The retained plan's `sha` identifies the
tested PR merge commit: its ordered parents must be approved main and candidate, and its tree must
be the candidate tree. The workflow ID and canonical path must both match; bare and ref-qualified
run paths are supported. Empty PR associations cannot authorize an open release check.

The complete attempt roster includes resolve, Linux/macOS base, every selected class on both
platforms, container probes, both telemetry smokes, lint and both platform check aggregates. The
existing catalog and aggregate contract determine exact hosted success/skip outcomes. Fast telemetry
smokes must be skipped as declared exemptions; they are never measured passes. Full telemetry and
all selected class legs must succeed. Missing, duplicate, unexpected or foreign-attempt jobs fail
closed. Paginated collections must agree with their advertised total count and have unique IDs;
partial pages cannot establish completion. Additional formally required statuses must also pass.
Reruns use their latest attempt, rechecked after observation, never earlier successful attempts.
Before accepting evidence, the controller re-observes the complete matching run collection and
waits if the selected run or attempt is no longer latest. This client observation is not atomic
with merging; server-side strict checks remain required.
Receipts retain the exact resolved plan, artifact identity/digest and complete job outcomes.
The [attempt-specific jobs endpoint](https://docs.github.com/en/rest/actions/workflow-jobs#list-jobs-for-a-workflow-run-attempt)
binds jobs to the attempt; the documented job shape need not contain `run_attempt`. When supplied,
that field must agree too.

The developer release policy is `af.dev-release-policy/3`; the previous subset/skip-list policy is
refused. Resolver and catalog execution bytes must match the captured source. Existing CI plan,
class receipt and aggregate contracts remain unchanged.

### API evidence provenance

The Task supplied root-operator read-only observations from 2026-11-13 01:25 UTC. PR 857's branch
head was `7948d6bef943f2c190a7dac2d4689763a357dcc5`, while its merge SHA was
`40da982555874a09c030ac3725e851f14ec5fddd`. Run 43337337193 was returned by a branch-head query
with workflow ID 339965080, a bare CI path and the branch `head_sha`. Its base was another work
branch, so it demonstrates API shape only and cannot authorize a release from main. Closed-PR run
43295959631 had an empty PR association. Its `validation-plan-1` artifact 13979815551 had ZIP digest
`f793b8bec18f9d998a0f733b49e2dfe4f7f3b76081ee043b0c93370ac38e810a` and retained a fast plan
with merge SHA `4bb8df44782ffb50f50b68693b8e285356dd3118`. Release run 43318826301 had workflow
ID 339974639 and a bare release path. Claims that REST always uses the merge SHA or always
qualifies the path are unsupported by those observations.

These are supplied operator observations, not live calls made by the implementation tests. The
hermetic CLI fixtures model separate branch/merge SHAs, bare/ref-qualified paths, retained plan ZIPs,
late/full label evidence, reruns and partial collections. GitHub's
[workflow-run API](https://docs.github.com/en/rest/actions/workflow-runs) describes REST identities;
the [event reference](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows)
describes job `GITHUB_SHA`. Neither substitutes for exact candidate/base/plan binding.

Before any expensive work or publication effect, `plan` and `rc` require an observable active
repository main ruleset with `required_status_checks`, a nonempty required check list,
`strict_required_status_checks_policy: true`, and no bypass actors. An administrator must select
**Require status checks to pass** and **Require branches to be up to date before merging** for
`main`, include the stable `validation / lint` check (and any other repository-required checks),
and leave the bypass list empty. The CLI only reads this setup; it never configures it. Missing
setup produces `expected_base_prerequisite` with these instructions, including in read-only plan.
The target repository's previously observed rules without required statuses do not satisfy this
prerequisite. The owner must approve any administrator setup separately.

GitHub's strict check rule enforces freshness at merge time. The client rechecks fetched main and
the strict rule immediately before requesting merge. `--match-head-commit` protects only the head;
it is never a base compare-and-swap. Post-merge tree and ancestry checks remain mandatory.
See GitHub's [active branch rules API](https://docs.github.com/en/rest/repos/rules#get-rules-for-a-branch)
and [workflow attempts API](https://docs.github.com/en/rest/actions/workflow-jobs#list-jobs-for-a-workflow-run-attempt).

Publication saves one immutable signed bundle named by tag and source before its first release
asset upload. Every rerun restores that bundle, including original provenance and signature,
then verifies signed digests through the publisher. Bundles are retained for 90 days; expired,
missing-after-upload or ambiguous bundles require operator recovery of the original artifact.
Rebuilt archives never replace partially uploaded bytes. Signed provenance identifies the original
signing run; a later successful protected publication run may publish that same bundle. Pin-only
credentials and failures remain independent of readiness.
