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

This is a prerelease for Task integration and migration validation. Live pilot acceptance
and complete consumer migration/rollback verification remain pending; this candidate does
not establish stable-release readiness or replace the known-good consumer installation.

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
- Preserve nested Jira requirements, resolve explicit relative refresh files from the caller's
  directory, and retain plans when only unselected Jira fields or update timestamps change.
- Make identical authenticated plan-revocation requests idempotent and refuse revocation of
  decisions that were never approved.
- Reduce repeated captured Review validation, heartbeat and receipt-query work while retaining
  fresh artifact, lease, approval and usage checks; stream raw usage transcripts with bounded memory.
- Open the repository: `install.sh` installs from a public release with `curl` alone, verifies
  the release signature by default when `minisign` is present, and keeps `gh` only as a
  fallback; the release also ships `LICENSE` (Apache-2.0), `SECURITY.md`, `CONTRIBUTING.md`
  and a restructured `docs/` (architecture, tasks, migration, non-goals, design notes).

- Account for every model reported by native Claude Task usage; preserve auxiliary charges
  and refuse unapproved auxiliary activity instead of accepting an Opus selector alone.
- Use native JSON schemas and structured output for typed Claude Task replies while retaining
  independent Kernel output validation and the existing legacy text transport.
- Retry transient executable-busy process starts and correct concurrent Linux test allowances.
- Explain logged-out Provider status and install declared toolchain components in release jobs.

### Validation boundary

The release workflow checks the tagged source on Linux and macOS, runs consumer fixtures,
and signs archive checksums. These deterministic checks do not replace specialist reviews,
live Task measurements or actual consumer migration and rollback rehearsals. See
[Task execution](docs/task-execution.md) for the contracts; candidate publication does not
claim those remaining acceptance gates have passed.

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
