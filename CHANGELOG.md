# Changelog

Every release has a section here before it is tagged: `scripts/release.sh X.Y.Z --compat "…"`
writes it from the pull requests merged since the previous release, and the release workflow
publishes the section as the release notes. The **Authority compatibility** line is mandatory:
it says whether committed `.af/` policy keeps working as is, needs `af onboard --refresh-lock`,
or needs `af onboard --migrate --apply`. Releases before 0.7.1 are described on their GitHub
release pages only.

## [Unreleased]

### Changes

- GA reads only what GA writes
  ([ADR-0113](docs/adr/0113-ga-reads-only-what-ga-writes.md)). Review Campaigns and Tasks that a
  0.x release wrote are not read, replayed or migrated: that covers everything under
  `$XDG_STATE_HOME/af/review/` and `$XDG_STATE_HOME/af/task/`, and any directory passed with
  `--state` or `--state-root`. Before upgrading, finish or abandon in-flight Campaigns and Tasks
  with the release that started them, then delete that state. Committed `.af/` files are read only
  in the shapes this release writes. A key or shorthand that only an earlier release wrote is
  refused, so edit it out or regenerate the file.

## [0.9.0-rc.6] - 2026-09-21

### Authority compatibility

Prerelease: committed .af authority keeps working as is and needs no migration. The sha2 0.11, rusqlite 0.40 and jsonschema 0.56 updates change no stored spelling: Attempt IDs, sha256: artifact addresses, receipt prefixes and local state directory names are byte-for-byte what earlier releases wrote, and review_core::hex::encode pins that encoding under test. The machine-local Provider registry stays version 1 — af provider setup and af provider recover run in the invoking release instead of dispatching through a repository pin, and a replacement is published by one atomic exchange, so a supported pinned reader sees either the complete old registry or the committed new one (ADR-0111). The one change that can stop a working machine: Provider admission now revalidates auth directories, so an existing symlinked, foreign-owned, or group/world-writable auth directory or registry is refused until its ownership and permissions are tightened. Refresh a consumer pin explicitly with af onboard --refresh-lock after verifying the release and its archive digests, and keep the previous release for rollback.

### Changes

- release: v0.9.0-rc.4 (#98)
- build(deps): bump actions/checkout from 4.2.2 to 7.0.1 (#82)
- build(deps): bump actions/upload-artifact from 4.6.2 to 7.0.1 (#81)
- build(deps): bump actions/download-artifact from 4.3.0 to 8.0.1 (#80)
- Take the pending dependency updates, adapting digest and SQLite identities (#99)
- build(deps): bump clap_mangen from 0.2.33 to 0.3.3 (#87)
- build(deps): bump rusqlite from 0.37.0 to 0.40.2 (#86)
- build(deps): bump sha2 from 0.10.9 to 0.11.0 (#85)
- Bump nix from 0.30.1 to 0.31.3 (#84)
- build(deps): bump jsonschema from 0.26.2 to 0.56.0 (#83)
- Make fresh Provider onboarding copyable (#90): make fresh Provider bootstrap one command with
  `af provider setup`, keep `af provider add` for already-authenticated contexts, isolate login
  environment and auth-directory ownership, make setup serialize by canonical auth context,
  distinguish ambient discovery labels from selectable registry IDs, preserve every Gate and
  executing release in onboarding's copyable apply command, make registry publication serialized,
  conditional, durable and atomic across pinned releases, secure its directories and files
  independently of `umask`, reject unpublishable auth paths before login, bind security-sensitive
  operations to stable directory handles, provide hash-validating fail-closed recovery with
  `af provider recover`, and make the README's Codex-only quickstart complete.
- Skip the last non-UTF-8 path test where the filesystem refuses such a name (#106)
- release: v0.9.0-rc.5 (#100) — tagged but never published: its macOS check leg failed, so
  everything it carried ships here instead.

## [0.9.0-rc.5] - 2026-09-20

Tagged but never published — the release workflow's macOS check leg failed before the
publish step. Everything this version carried ships in 0.9.0-rc.6.

## [0.9.0-rc.4] - 2026-09-20

### Authority compatibility

Prerelease: committed .af authority keeps working as is. Every warm layer is opt-in per reviewer node (warm = { notes, build_cache, workspace, session }); a pipeline that declares none behaves exactly as before, and the Store, the run events and the pinned authority mirrors gain additive records only, with no migration. The session layer is Claude-only and defaults to off: Codex reviewers and Task-hosted Review Attempts record a drop reason and run on Notes instead. A candidate-built build cache is an explicitly unsafe artifact, never a Cache Snapshot, and is refused under the safe policy at load, at selection and at capture. WarmSetSelected@1 first ships here, so its vocabulary closes with this release (ADR-0107). Refresh a consumer pin explicitly with af onboard --refresh-lock after verifying the release and its archive digests, and keep the previous release for rollback. No cold-versus-warm savings Evidence exists yet, so the design review's build-minute and forked-resume Demands stay open and both session and workspace policy defaults ship off.

### Changes

- release: v0.9.0-rc.2 (#93)
- Add self-optimizer economics, experiments and light optimization (#94)
- release: v0.9.0-rc.3 (#95)
- Reduce release latency with shared validation and concurrent builds (#96)
- Start Workers warm from declared layers and confirm clean Rounds cold (#97)
### Changes

- Add the first Worker warm layer: a reviewer node with `warm = { notes = true }` asks each
  admitted Attempt for bounded Worker Notes, carries them to the next Round's Attempt of the same
  node as `review.kernel/WorkerNotes@1`, marks every path against the previous head in a
  `review.kernel/HeadDelta@1`, and records the selection as `review.kernel/WarmSet@1` with
  `WarmSetSelected@1` before dispatch. Notes are parsed beside the flat Reviewer Result, dropped
  with a recorded `WorkerNotesRecorded@1` reason when malformed or over `notes_max_bytes`, and
  rendered as data with their own context manifest entries; `af review report` shows the layers
  and rendered input per Attempt. Task Workers may declare optional `af/WorkerNotes@1` ports that
  the compiler wires only within one slot. With `warm` absent nothing changes
  ([ADR-0107](docs/adr/0107-carry-worker-notes-and-head-deltas-as-declared-warm-layers.md)).
- Carry the Gate's build to Worker sandboxes as the second warm layer: a `trusted_local` Gate
  that declares `build_caches = ["cargo_target"]` builds into the reserved `.af-cache` root with
  `CARGO_TARGET_DIR` pointed at it, captures the result after its checks pass as an explicitly
  unsafe `review.kernel/BuildCache@1` (regular files only, no-follow traversal, entry, depth,
  path and byte limits, fixed modes, stripped xattrs and ACLs, producer and head provenance),
  and records `BuildCacheCaptured@1` with the artifact or a refusal reason. A reviewer node with
  `warm = { build_cache = ["cargo_target"] }` receives a per-Attempt clone from the CAS through
  its `WarmSet@1`, the bytes are removed before seal so the sealed diff equals a cold run's, and
  the safe policy refuses the declaration at load and the handoff before any dispatch. The
  registry-only `cargo` Cache Snapshot and the self-optimizer cache path are unchanged
  ([ADR-0108](docs/adr/0108-carry-gate-build-caches-as-explicitly-unsafe-warm-layers.md)).
- Add the Warm Workspace as the third warm layer: a reviewer node with
  `warm = { workspace = "rebase" }` keeps one stable template root per Campaign under
  `$XDG_CACHE_HOME/af/workspaces`, named in its `WarmSet@1` by an opaque workspace identity
  rather than a host path. On a new head the kernel applies the tree diff to a copy-on-write
  clone of the previous template, scans the result and swaps it in only when its manifest digest
  equals the head's Tree Digest; any other outcome falls back to a full materialization from the
  CAS with the reason recorded. An unchanged head materializes nothing and is verified by a read-only scan before it is reused; the root's marker is trusted only against the Campaign log's last `WorkspaceRebased@1`, and a marker the log never recorded rebuilds the head as `unrecorded_preparation`. Preparation failures carry no host path, cold pipelines touch no cache configuration, and the event records the preparation time. `WorkspaceRebased@1`
  records the previous and current head, the basis, the fallback reason, the verified digest and
  the entries touched before the Warm Set is recorded, and the `workspace` layer joins
  `WarmSetSelected@1` when the template was carried. Per-Attempt sandboxes remain fresh clones,
  so a warm Attempt's sealed diff is what the reviewer wrote; nodes without the policy and
  pipelines written before it are unchanged
  ([ADR-0109](docs/adr/0109-rebase-warm-workspaces-at-stable-roots-with-digest-verification.md)).
- Add the Session Snapshot as the fourth warm layer, for Claude reviewers only, and the compiled
  Cold Closeout beside it. A node with `warm = { session = "if_recent" }` runs each Attempt under
  a `--session-id` the kernel derives from the Attempt ID; at seal the bounded transcript enters
  the CAS as `review.kernel/SessionSnapshot@1`, `SessionSnapshotPrepared@1` records it with the
  bytes, the estimated tokens and a path-free source identity, the harness copy is deleted, and
  `SessionSnapshotCleaned@1` closes the protocol. What is stored carries no host path and no
  credential: the sandbox and harness paths become reversible placeholders a materialization puts
  back, and a transcript carrying a credential shape is refused whole. Every component below the
  granted harness root is opened `O_NOFOLLOW`, and the directory a transcript was validated in
  stays open through its unlink. An Attempt that ends any way but an admitted capture deletes its
  own transcript before its retry. A sweep before the
  Round's first Attempt finishes any cleanup a crash interrupted and removes every transcript the
  node's Attempts could have left, without a provider call. The next Round re-materializes the
  transcript and resumes it with `--resume --fork-session`, sending only the delta prompt — no
  package instructions, no Change Set patch — with both the transcript and the delta listed in the
  Attempt's context manifest. Provider support, `warm.session.max_age` and fitting the reservation
  beside the delta are gates whose every failure is a recorded `WarmSet@1` drop back to Notes
  alone, including a Head Delta dropped over its bound; Codex implements nothing and keeps
  `--ephemeral`. `[convergence] cold_closeout` compiles a conditional cold Attempt of a warm
  reviewer, reserved before its warm Attempt so a retry cannot consume it, dispatched at the
  Ledger only when every warm result of the Round would otherwise close it clean, run through the
  ordinary Attempt lifecycle under its own closeout slot, and folded through
  `ColdCloseoutDispatched@1` as a stage of its own before the convergence decision. A confirmation
  that produced no admissible result leaves the Round incomplete.
  Both policies default to off, so a pipeline written before this package is unchanged
  ([ADR-0110](docs/adr/0110-capture-sessions-in-two-phases-and-confirm-clean-rounds-cold.md)).

## [0.9.0-rc.3] - 2026-09-17

### Authority compatibility

Prerelease: existing .af authority remains supported. Self-optimization is opt-in and requires a reviewed project optimizer catalog, bounded policy and the RC3 binary; generated experiments still require exact signed developer approval. Refresh a consumer pin explicitly with the supported onboard refresh-lock path after verifying the release. New optimizer state and Task inspection/transition formats require a compatible binary; preserve the prior Store and known-good release for rollback. Heavy redesign, live paid savings demonstrations, longitudinal adoption and stable-release migration/pilot gates remain pending. This release does not switch consumer pins or claim stable readiness.

### Changes

- Add project-history capture and reporting with explicit token, elapsed-time and cache evidence (#94).
- Run bounded optimization experiments with signed approvals, independent evaluation and exact accounting (#94).
- Add light optimization for Worker instructions and safe Cargo cache selection, with payoff gates and durable adoption evidence (#94).
- Exercise candidate model contexts and delivered cache settings through real Task paths; coordinate the completion-order test explicitly to avoid scheduling flakes (#94).
## [0.9.0-rc.2] - 2026-09-15

### Authority compatibility

Prerelease: `af task start` now captures and previews without dispatching Workers, including
with `--json`. Review the plan, then run with `--confirm-plan` and its full captured ID.
Automation must explicitly opt into `--execute` on start or on the first run of a plan.
Admitted Tasks can resume and finished Tasks replay as before. This CLI confirmation never
replaces signed developer approval for a generated plan. Existing `.af` authority remains
supported; legacy `.review` migration and exact release pin verification still apply.

### Changes

- Render captured Task plans as compact ASCII flows or expanded `task explain --tree` views,
  with embedded calls, actual Worker models/efforts/accounts, inputs, outputs, effects and limits.
- Stop new Tasks before execution and refuse stale plan confirmations, including a plan change
  before the writer lease is acquired. Explicit automation retains existing runtime admission.
- Keep JSON inspection schemas unchanged, sanitize terminal display text, and document the
  preview/approval workflow for Claude and Codex.

The live pilot and complete consumer migration/rollback acceptance remain pending. This
candidate does not declare stable-release readiness or change the active consumer's pin.

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
- Preserve prerelease versions in migrated authority so the candidate can read its own output.
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
