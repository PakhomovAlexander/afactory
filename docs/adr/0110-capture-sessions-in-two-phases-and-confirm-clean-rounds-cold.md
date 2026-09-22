# ADR-0110: Capture Claude sessions in two phases and confirm a clean warm Round cold

Date: 2026-09-18
Status: Accepted; superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): the
Kernel reviewer path that ran the session protocol and dispatched Cold Closeout, with the
Campaign `Attempt*@1` records a confirmation wrote. Until the Task host runs them, no executor
captures a session or dispatches a confirmation.

Implements package P4 of [`docs/design/worker-warm-layers.md`](../design/worker-warm-layers.md) on
top of [ADR-0107](0107-carry-worker-notes-and-head-deltas-as-declared-warm-layers.md) (the Warm Set
this extends and the Notes every gate falls back to),
[ADR-0109](0109-rebase-warm-workspaces-at-stable-roots-with-digest-verification.md) (the workspace
preparation that precedes a Round's first dispatch),
[ADR-0028](0028-prioritize-wise-token-use-and-minimum-worker-context.md) (bounded reservations and
an exact context manifest per Attempt),
[ADR-0042](0042-require-provider-bindings-and-isolate-claude-reviewers.md) (adapter-owned provider
flags) and [ADR-0002](0002-event-payload-changes-bump-the-type-version.md) (new events, not widened
payloads).

## Context

A Round N+1 reviewer re-sends its whole prefix because every Attempt is a new conversation: the
package instructions, the Change Set and its patch, all of it, at full input price, minutes after
the previous Round read the same bytes. The pinned `claude` 2.1.273 CLI accepts `--session-id`,
`--resume` and `--fork-session` in print mode, so the transcript of one Attempt can be carried to
the next Attempt of the same node and continued instead of restated.

Two things make that dangerous rather than merely useful. The transcript is a file in the
operator's own harness directory, which today's Attempts leave behind; and a warm reviewer that
has already reasoned about this change may confirm its earlier view rather than re-derive it, so a
Round that closes clean on warm results alone is worth less than it looks.

## Decision

- **The session identity is the kernel's, derived from the Attempt.** A node whose warm policy
  asks for the layer gets `--session-id <identity>` where the identity is its Round-scoped Attempt
  ID followed by a fixed suffix, in the canonical 8-4-4-4-12 shape. Derived, never random, so
  replay names the same session and a kernel that crashed after a capture can still name the exact
  file it owes a deletion. The flags precede the package's model flags and are pinned by fixture
  exactly as the security flags are.
- **Capture is two durable phases under the Attempt epoch.** At seal the adapter reads the bounded
  transcript, the bytes and a `review.kernel/SessionSnapshot@1` describing them enter the CAS, and
  `SessionSnapshotPrepared@1` records the artifact, the bytes, the estimated tokens and the exact
  source identity. Only then is the harness copy deleted, and `SessionSnapshotCleaned@1` records
  `deleted`, `already_absent` or a refusal with what stood there instead. A capture larger than
  the bound stores nothing and still deletes, so an over-bound session leaves no ambient
  transcript either.
- **What is stored carries no host path and no credential.** A transcript is provider bytes about
  a sandbox that no longer exists, so before it enters the CAS the Attempt's sandbox path and the
  harness directory are replaced by fixed placeholders, and a materialization puts the current
  Attempt's own paths back. A transcript carrying a credential shape — an API key prefix, a
  private-key header, a token prefix — is refused whole rather than redacted: a refused capture
  costs one cold Round, and no partial redaction can promise what it removed. Both refusals still
  delete the harness copy.
- **The granted root is trusted; everything below it is not.** The store is opened at the path the
  operator granted, because a grant is a path the operator chose and refusing it for a symlinked
  home would refuse ordinary machines. Below that root — the projects directory, each project
  directory, the transcript — every component is opened descriptor-relative with `O_NOFOLLOW`, and
  the directory a transcript was validated in stays open from the search through the unlink, so
  the file removed is the file that was checked and a directory replaced in between cannot
  redirect the deletion. Materialization creates its directories the same way.
- **Recovery finishes the cleanup without a provider call.** Before a Round's first Attempt is
  reserved, the kernel sweeps the node: every session identity its Attempts could have written is
  derivable from an Attempt ID the log already records, so the sweep deletes each one — the
  working copy a resume materialized, a failed Attempt's transcript, and the harness copy of any
  capture whose cleanup never completed — and appends the missing cleanup-completed record for
  exactly the captures the log prepared. No process starts; a deletion is a filesystem operation
  against an identity the log holds.
- **A source is selectable only when it is admitted *and* cleaned.** Warm Set selection carries a
  Session Snapshot only from the previous closed Round's admitted Attempt of the same node, and
  only when that capture's cleanup completed. A crash between the phases therefore leaves neither
  an orphaned CAS object nor a transcript anyone would resume: the object stays exactly as the log
  describes it, and the layer is dropped as `cleanup_incomplete` until the sweep finishes.
- **Four gates, and every failure is a recorded drop.** The adapter must host sessions
  (`provider_unsupported`) and the frontend must run the protocol (`host_unsupported`); the source
  Attempt must be younger than `warm.session.max_age` (`too_old`); the transcript, *the delta
  prompt this exact Attempt would send*, and a fixed answer allowance must fit the reservation
  together (`over_reservation`); and this Round must be able to say what moved, so a Head Delta
  dropped over its bound drops the session with it (`head_delta_dropped`) rather than continuing
  a fork that cannot be told what changed. The delta is measured, not assumed: the prospective
  inputs are rendered exactly as the resumed Attempt would render them, and the three terms are
  summed with a checked addition, so an overflow drops rather than admits. A missing source, an
  absent capture and a transcript the CAS can no longer verify are drops too. Every one of them
  falls back to Notes alone, which is why a node that asks for the session layer must also keep
  `notes = true`. No gate failure refuses a Round.
- **Every Attempt takes its transcript with it.** The two-phase capture claims the session of an
  Attempt whose output was admitted. Every other ending — a timeout, a malformed answer, a refused
  result, a panic, an exhausted retry — deletes that Attempt's own transcript as the Attempt ends,
  before its retry is dispatched, and says so on the operator's terminal when the deletion is
  refused. The sweep before a Round's first dispatch remains the crash-recovery path, not the
  ordinary one.
- **The resume is forked, and the prompt is the delta.** The carried transcript is re-materialized
  under the *source* session's identity into the resuming Attempt's own harness directory and
  resumed with `--resume <source> --fork-session`, so the captured object is read and never
  mutated and everything this Attempt says lands in a session of its own. Its prompt omits the
  package instructions and the Change Set patch the fork already holds, and carries what changed:
  this Attempt's authority, its siblings' refusals, the current prior Finding Set, Delta Marking
  against the head the session inspected, the resolved non-Change-Set ports, and the Notes
  contract. The output contract is restated in full, because it is what the kernel parses. The
  manifest lists the transcript with its bytes and estimated tokens and the delta with its
  rendered size, so a report can weigh one against the other, and the adapter already records
  cache-read tokens separately from input tokens.
- **Codex is excluded by construction.** The session layer is reached through an adapter method
  whose default returns nothing. Codex keeps `--ephemeral` and implements nothing, so it drops the
  layer as `provider_unsupported` without being named in the protocol: its pinned CLI has no
  `--session-id`, its `exec resume` and `exec fork` reject the adapter's `-C` and `-s` flags, and
  its authentication directory is the directory a kernel-owned session home would replace.
- **Cold Closeout is compiled, conditional and protected.** `[convergence] cold_closeout` names,
  at load time, the exact warm reviewers that owe a cold confirmation; the load refuses a policy
  that names no warm reviewer, and a budget that cannot admit the extra Attempt beside the
  Round's Workers. Each named node reserves its confirmation *before its warm Attempt is
  reserved*, which is what protects it:
  the node's own retries meet a cap that is already holding it. It carries no warm layer of any
  kind, makes no Proposal and leaves no Notes.
- **The Round decides, not one reviewer.** A confirmation is dispatched at the Ledger, where every
  warm result of the Round is known, and is skipped exactly when some warm result already carries
  a claim at or above the convergence gate, because the Round then blocks whatever a second
  opinion says. The rule stays one-sided: a prior-Round claim that reduction may or may not close
  is not read, so a confirmation is dispatched when in doubt and never skipped on a guess, because
  skipping it wrongly is what would let convergence close on warm results alone. A node with no
  admitted result gives the reservation back.
- **A confirmation is an Attempt, with an Attempt's record.** It runs the same durable lifecycle
  as the Attempt it confirms, under the node's own closeout slot: `AttemptDispatched@1` is durable
  *before* the provider is called, so a crash between them leaves an Attempt the next kernel run
  fences and charges rather than an invisible spend, and a terminal `AttemptAdmitted@1` or
  `AttemptFailed@1` carries its result, provenance, wall time and usage. `ColdCloseoutDispatched@1`
  then names the exact warm Attempt and result it closes over, and a Round already holding that
  record for that warm Attempt dispatches nothing again.
- **Both results reduce, as two stages of one reviewer.** The Ledger folds the confirmation under
  the node's closeout slot, so warm and cold are distinct sources rather than one source delivered
  twice — which is what canonical reduction requires, and what keeps two byte-identical answers
  from being mistaken for each other. Its Demand requirement is the reviewer's, and its exact
  invocation inputs are the warm Attempt's. **A confirmation that produced no admissible result
  stops the Round**: the Ledger refuses to reduce, the Round is incomplete, and no warm result
  closes a Round whose required confirmation never answered.
- **Both defaults are off.** `session` defaults to `off` and `cold_closeout` to `none`, so a
  pipeline written before this package dispatches exactly the Attempts it did before and renders
  byte-identical inputs. The design's `one_required` default waits for the dogfood baseline the
  design requires; a policy that admits it is one line.

## Considered options

- Keep the provider's own session ID and record it: rejected, the kernel could not name the file
  it owes a deletion after a crash, and a provider-chosen identity is not replayable.
- One event for capture-and-delete: rejected, it is precisely the crash between the two that must
  be recoverable, and a single record cannot distinguish "captured, still on disk" from "gone".
- Record the harness path so recovery can find the transcript: rejected, a host path never enters
  a durable record (ADR-0109), and the sandbox that produced the transcript no longer exists.
  Searching the harness directory for the kernel's own identity needs neither.
- Resume without a fork: rejected, it mutates the captured transcript in place, so the artifact
  the manifest names stops describing what the model read.
- Drop the session layer silently when re-materialization fails at the sandbox: rejected, the
  Round's Warm Set already declared the layer. A failure there fails the Attempt; only selection,
  which records its reason, may drop a layer.
- Schedule Cold Closeout as a Round-level rule: rejected, a Round is only known to be the closing
  Round after its results are reduced, which is after the scheduler could have dispatched
  anything.
- Give the confirmation its own graph node: rejected for this package, a conditional node needs a
  branch the review scheduler does not have; the confirmation is compiled into the node it
  confirms, with its own Attempt identity and its own durable record.
- Reserve the confirmation when it is needed: rejected, by then the node's retries may have spent
  the cap, which is the one thing "protected" has to prevent.

## Consequences

- Three new event types join the closed vocabulary and `WarmSet@1` gains the session layer with
  its drop reasons, which ADR-0107 permits until the release that first ships it. Existing logs
  replay unchanged, and a pipeline with neither `session` nor `cold_closeout` is byte-identical in
  every event, artifact and fixture to one written before this package.
- The kernel now deletes files in the operator's harness directory. It deletes only sessions whose
  identity it derived from its own Attempt IDs, opens every component `O_NOFOLLOW`, refuses
  anything that is not the regular file a transcript is, and removes the emptied project directory
  behind it. This is stricter than today, where Claude session state is simply left in the user's
  home.
- A resumed Attempt's saving is only real when cache-read tokens show it. The layer ships off; the
  dogfood comparison the design requires — net token and wall-time savings over cold and
  notes-only Attempts at several ages — is not part of this package and its Demand stays open.
- The session layer runs on the Kernel's own reviewer path. A Task-hosted Review Attempt records
  `host_unsupported` and runs on Notes, which is a recorded drop rather than a silent one; giving
  that frontend the protocol is separate work.
- A Round with a confirmation costs one extra Attempt of the same reviewer whenever it would
  otherwise close clean. That is the price of not closing on a warm result alone, and the budget
  refuses the policy up front when it cannot pay it.
