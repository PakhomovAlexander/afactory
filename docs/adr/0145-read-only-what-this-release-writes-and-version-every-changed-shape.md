# ADR-0145: Read only what this release writes, and version every changed shape

**Status:** accepted (2026-10-09). Supersedes in part
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): the compatibility obligations it started at GA.
Supersedes in part [ADR-0002](0002-event-payload-changes-bump-the-type-version.md) and
[ADR-0021](0021-keep-reviewer-result-wire-shape-flat.md): the GA scope of their version rules.
Supersedes [ADR-0110](0110-capture-sessions-in-two-phases-and-confirm-clean-rounds-cold.md) in
full.

## Context

[ADR-0113](0113-ga-reads-only-what-ga-writes.md) removed every reader, migration and fallback for
state that only a pre-GA release wrote, and promised that obligations would start at GA: from then
on, the state, configuration and contracts GA writes would be the baseline, and every later
release would keep reading them. The owner has decided against that promise. Afactory has few
users, each of whom can finish or discard in-flight work before an upgrade, and every kept reader
costs code, tests and fixtures that the current release never exercises on its own output.

Three kinds of code survived ADR-0113 only because of that promise or a promise like it:

- **A recognizer for pre-GA Campaigns.** `af review run` named a Campaign whose log held a
  `RunReport@1` to `@5`, or one of three session and Cold Closeout events, as predating the common
  Task runtime (af < 0.9). Without that check the same log already fails closed: replay stops at
  the unknown type with a message that says another af release wrote it.
- **A dormant protocol.** [ADR-0110](0110-capture-sessions-in-two-phases-and-confirm-clean-rounds-cold.md)
  captured Claude sessions in two phases and confirmed a clean warm Round with a cold Attempt.
  ADR-0113 removed the only executor that ran it and kept the protocol "as the base for running
  it on the Task host". No host ever installed it: every Task-hosted Attempt recorded the session
  layer as `host_unsupported`, and no confirmation was dispatched. The config keys
  `warm.session`, `warm.session_max_age_secs` and `convergence.cold_closeout`, the events
  `SessionSnapshotPrepared@1`, `SessionSnapshotCleaned@1` and `ColdCloseoutDispatched@1`, the
  `review.kernel/SessionSnapshot@1` artifact, the runner's `SessionLayer` and the Ledger's fold of
  a confirmation all stayed, reachable from no current execution.
- **A legacy layout entry.** The `.af/` layout declared `task-compat/`, an in-memory authority
  bundle for the retired fixed implementation format, which nothing builds any more.

`WarmSet@1` and `WarmSetSelected@1` are different: the Task host writes them on every warm
Round today, and `WarmSet@1` carries a `session_dropped` reason, `host_unsupported`, even though
no session can be captured. Removing the session layer changes their shape.

## Decision

1. **No compatibility obligation for earlier releases.** A release reads the state,
   configuration and flags it writes. It adds no reader, migration, alias, fallback or replay path
   whose only purpose is state, configuration or flags an earlier release wrote, whether that
   release was pre-GA or not. Such state may be refused: an event or artifact type this release
   does not write fails closed on the unknown type, and a committed `.af/` key it does not accept
   is refused when the file loads. Operators finish or abandon in-flight Campaigns and Tasks under
   the release that started them, then delete them, as ADR-0113 described for GA.
2. **Every changed persisted shape takes a new version, and the old one fails closed.** The rule
   of [ADR-0002](0002-event-payload-changes-bump-the-type-version.md), and of
   [ADR-0021](0021-keep-reviewer-result-wire-shape-flat.md) for reviewer results, applies to every
   release and every contract it ships, not only from GA: a typed event, artifact, record or
   output whose shape changes gets a new version, its writers, readers, Store validation, JSON
   Schema and parity fixtures change together, and the old version is neither written nor read.
   A changed payload never reuses its type identity. ADR-0113's in-place allowance for pre-GA
   contracts is spent.
3. **Current-version replay stays.** Everything this release writes it replays exactly: a
   resumed Round, a recorded Warm Set, a Task log and its accounting read back as written.
   Corrupt or unreadable state keeps failing closed with its own error; nothing in this decision
   loosens a validator, a budget, a gate or a sandbox boundary.
4. **Self-management is unchanged.** `af self` and the project pin still install, activate and
   dispatch to the pinned release from 0.8.0 on, and verify its bytes and its `SHA256SUMS`
   signature ([ADR-0044](0044-af-manages-itself-and-dispatches-to-the-pinned-release.md),
   [ADR-0045](0045-one-release-train-and-a-pin-that-binds-bytes.md), ADR-0113 clause 5). Dispatch
   is what lets a pinned project run the release that wrote its state, so it is not a
   compatibility reader.
5. **Names and invocations stay.** Persisted `review.kernel/*` and `af/*` type strings, schema IDs
   and Review Kernel terms keep their spelling (ADR-0113 clause 4), and CLI invocations keep their
   shape unless a change needs otherwise. Release notes keep listing what changed, including the
   *Authority compatibility* line `make release` requires, which states what a release accepts;
   it promises nothing about the next one.
6. **The session layer and Cold Closeout are removed.** ADR-0110 is superseded in full. The config
   keys `warm.session`, `warm.session_max_age_secs` and `convergence.cold_closeout` are refused at
   parse; the three events and `review.kernel/SessionSnapshot@1` are no longer types; the runner's
   `SessionLayer`, the resumed-Attempt delta prompt, the Claude adapter's session flags and the
   Ledger's confirmation fold are deleted. A Claude command assigns no session identity and
   resumes nothing. What stays is live: the Notes, Head Delta, Build Cache and Warm Workspace
   layers, the whole-Subject closeout reviewer of a Scatter (`closeout_for`), and the removal of
   the history the Claude CLI keeps for an Attempt's own working directory
   ([ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md)).
7. **`WarmSet@2` and `WarmSetSelected@2`.** Under clause 2, the Warm Set without its session layer
   is `review.kernel/WarmSet@2` (schema `warm-set-v2.json`) and its selection event is
   `WarmSetSelected@2`, whose layers are `notes`, `head_delta`, `build_cache` and `workspace`.
   Every producer, the Task host's replay of a recorded selection, the context manifest entry,
   the `af review report` warm-layer join, Store validation, the event schema and the parity tests
   use version 2. `WarmSetSelected@1` is an unknown event type, and a recorded selection that
   names a `WarmSet@1` artifact is refused by its type.
8. **Spent records go.** The `.af/` layout no longer declares `task-compat/`, so a tracked copy is
   reported as undeclared like any other stray tree. A design note whose work is complete is
   deleted with the generated records it carried, and git history keeps it; a link to it becomes
   plain text, the remedy ADR-0113 clause 8 gives.

ADR-0110 keeps its file, with a status line naming this ADR, instead of being deleted under
ADR-0113 clause 6: ADR-0107, ADR-0108 and ADR-0109 and the warm-layer design cite it as the
record of the session layer whose removal is the reason `WarmSet@2` exists.

## Considered options

- **Keep ADR-0113's promise and start obligations at GA.** Rejected by the owner: the cost of a
  permanent reader is paid on every change, and the users it protects can rerun their work.
- **Drop the version rule along with the compatibility promise, and change shapes in place.**
  Rejected: a stray record of an earlier shape would then parse under the current name, and a
  digest-bearing artifact would change meaning without changing identity. A new version is what
  makes the old record fail closed instead of being misread.
- **Keep `WarmSet@1` and stop writing its session fields.** Rejected: the accepted shape would
  still admit `session_artifact_id` and `session_dropped`, and the selection event would still
  admit a `session` layer, so the contract would describe a layer no host can carry.
- **Keep the session protocol for a future Task-host port.** Rejected: it was dormant through
  every release since ADR-0113, it carried its own events, artifacts, config keys, adapter flags
  and filesystem deletions, and a port would have to be re-decided against the Task runtime
  anyway. A future session layer is a new decision with new types.
- **Keep the pre-GA Campaign recognizer as a friendlier message.** Rejected: the replay error it
  replaced already names another release and says to start a new Campaign or Task.

## Consequences

- An upgrade may refuse a Campaign or Task an earlier release wrote. The refusal names the unknown
  type or key; the remedy is to finish the work under the release that started it, or delete it.
  A Round recorded with `WarmSetSelected@1` is such a Campaign.
- Every later change to a persisted shape ships its new version together with its writers,
  readers, schema and parity tests, and removes the old one; there is no transition release.
- A pipeline that sets `warm.session`, `warm.session_max_age_secs` or `convergence.cold_closeout`
  no longer loads. Removing the key restores its behavior, because no host honored it.
- The common missing-Task refusal of ADR-0084 is unchanged: a Campaign with common Task evidence
  whose Task is gone still refuses instead of reopening work on a fresh allowance.
- ADR-0113's clauses 2, 4, 5, 6 and 8 stay in force, and so do the deletions and supersessions it
  recorded. Its clause 1 now holds for state any earlier release wrote, and its closing
  consequence, that GA's output is the baseline for later releases, is withdrawn.
