# Changelog

Every release has a section here before it is tagged: `scripts/release.sh X.Y.Z --compat "…"`
writes it from the pull requests merged since the previous release, and the release workflow
publishes the section as the release notes. The **Authority compatibility** line is mandatory:
it says whether committed `.af/` policy keeps working as is, needs `af onboard --refresh-lock`,
or needs `af onboard --migrate --apply`. Releases before 0.7.1 are described on their GitHub
release pages only.

## [Unreleased]

## [0.9.0-rc.1] - 2026-09-15

### Authority compatibility

Prerelease: new Task executions require versioned .af Task authority; legacy .review consumers require af onboard --migrate --apply. Preserve the old Store and known-good release; migrate and verify release pins before consumer cutover. Live pilot acceptance remains pending.

### Changes

- Install the toolchain's declared components in the release build jobs (#73)
- docs: backlog after the 2026-09-10 review — .review/ drop shipped, three issues remain (#74)
- Explain a logged-out Provider row in af provider status (#77)
- Cache cargo dependencies and build artifacts in CI (#78)
- Make Task execution durable across implementation and review (#75)
- Build shared Task pipelines with embedded review and reusable packages (#76)
- Prepare the repository for open source (#79)
- Retry ETXTBSY on spawn and widen one reservation test's allowance (#88)
### Authority compatibility

The Task increment is implemented and unreleased.
New Task execution requires its versioned `.af` catalog, Pipeline/Worker contracts and lock.
Every generated Execution Plan requires an authorized developer's exact-plan approval.
Local bindings do not travel with shared definitions. Existing `.review` consumers still
need the supported `af onboard --migrate --apply` path; validate migrated authority together
with the chosen released binary and its verified archive digests before switching launchers.

New Review executions use the common Task runtime. Historical paid Campaigns preserve their
captured executor, authority and accounting; they are not rewritten into new Tasks. Keep the
original Store and known-good release for historical continuation and rollback. An older
binary must not reinterpret unsupported new state, and rollback cannot refund recorded usage
or reopen completed work. Release-bound migration and rollback evidence are release gates.

### Changes

- Make Task the common durable execution model for implementation, Review and documents,
  with typed Pipeline input/output contracts, captured context and shared budget accounting.
- Compose reusable Review Pipelines inside implementation, with independent acceptance and
  bounded repairs that retain Finding and Snapshot provenance.
- Select fitting shared Pipelines; persist generated definitions and plans for developer
  review when configured generation is needed; export definitions for subsequent reuse.
- Share versioned Pipeline/Worker packages and starter workflows through Git, with local
  Provider bindings, deterministic contract checks, Jira/local source capture and explicit
  delivery to a new local worktree.
- Preserve Provider usage and recovery evidence across cancellation, writer loss and storage
  failures, and capture explicit admission allowances in Task catalog V2.
- Open the repository: `install.sh` installs from a public release with `curl` alone, verifies
  the release signature by default when `minisign` is present, and keeps `gh` only as a
  fallback; the release also ships `LICENSE` (Apache-2.0), `SECURITY.md`, `CONTRIBUTING.md`
  and a restructured `docs/` (architecture, tasks, migration, non-goals, design notes).

Specialist PR reviews, the separately budgeted live pilot, supported boundary checks and the
checksummed consumer cutover remain required before release completion. See
[Task execution status](docs/task-execution.md).

## [0.8.0] - 2026-09-08

### Authority compatibility

Requires `af onboard --migrate --apply` for a consumer still on `.review/`: legacy authority is
no longer read for new Campaigns (ADR-0043). A repository already on `.af/` keeps working, but
re-pin it with `af onboard --refresh-lock` so the lock records the per-target archive digest this
release binds to. A `0.7.1` default cannot read a lock written by `0.8.0` (its `[af]` table is an
unknown field to the older parser), so it neither dispatches to `0.8.0` nor plans: run
`af self update` first on such a machine.

### Changes

- Route pipelines by changed paths; switch oversized Diffs to a bounded pipeline (#64)
- Show Campaign state on disk and reclaim it with af review gc (#66)
- One release train, a pin that binds bytes, and .review/ retired (ADR-0045) (#68)
- Commit the minisign release public key: every release from 0.8.0 ships a signed `SHA256SUMS`

## [0.7.1] - 2026-09-03

### Authority compatibility

Unchanged; `.af/af.lock` now records the `af` release that wrote it (`af onboard --refresh-lock`
re-pins). Legacy `.review/` policy is still read and upgraded in place by `af onboard --migrate`.

### Changes

- Plan consumer policies with every built af (#48)
- Validate and migrate legacy .review/ policy in af onboard (#49)
- Refresh the hub consumer fixture: one correctness reviewer (#51)
- Make wall-clock, provider usage, and dispositions visible per review (#54)
- Let a Worker node declare its own Attempt cap (#60)
- Render a Worker's exact input token-free (#61)
- Refuse a Worker input that exhausts its Attempt cap before admission (#62)
- Make af self-managed: clap tree, scoped help, completions, af self, dispatch, config ladder (#63)

## [0.7.0] - 2026-09-01

### Authority compatibility

The Ledger node must declare a `review.kernel/DemandSet@1` output; `af onboard --migrate --apply`
adds it to a legacy pipeline.

### Changes

- Harden review planning and provider admission (#31)
