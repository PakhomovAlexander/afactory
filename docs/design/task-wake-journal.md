# Task wake journal and delivery

The internal journal is now connected to opt-in registration-before-detach, public owner grants,
a bounded foreground driver and a durable local receiver. See the exact
[operator setup, status and recovery commands](../task-wake.md), and the proposed
[delivery decision](../adr/2026-11-13-drive-opt-in-local-task-wakes.md).
This is candidate implementation, with deterministic stand-in acceptance; final Pi/Codex live
certification remains pending and Claude automatic wake remains unavailable.

Checked extraction retains the established 4096-row pages, source/CAS validation, checkpoint
provenance, exact opening identities, maintenance-lease episode rules and atomic cursor/outbox
transaction. No notification digest participates in CAS reachability. The receiver uses that same
FULL-synchronous SQLite boundary for exact bounded packets, separate receipt stages and native
intent. Sender epochs, finite attempts and backoff persist. Original-session native uncertainty
is sticky across restart, route replacement, expiry and body compaction.

Expired unextracted subscriptions now close durably, including under collection serialization,
so they cannot retain the source indefinitely. A live unextracted subscription still blocks
collection. Materialized notifications disclose historical source unavailability after actual
collection. Bodies compact after the finite replay horizon; bounded uncertain session tombstones
remain, and exhausting their capacity refuses admission rather than authorizing resend.

The accepted research Document is
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`,
source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`.
Its common Store/outbox/bridge recommendation and conservative ambiguous-send policy are followed.
The production Pi bridge pulls from the Store instead of maintaining a second JSON journal.
Later host-start/report correlation is intentionally not inferred from void APIs or unrelated turns.

Bounded foreground drains persist independent extraction and native-route traversal positions in
one accounted Store row. A caught-up subscription or empty/unavailable route consumes its visit
but cannot permanently starve later targets across restarts. Positions grant no source or delivery
authority. Pi attachment generations invalidate outstanding callbacks on disconnect, shutdown and
replacement; a stale callback leaves committed native intent uncertain. See the proposed
[fair progress and attachment decision](../adr/2026-11-13-fence-wake-attachments-and-persist-fair-drain-progress.md).

Authenticated Pi polling now owns a separate `af/task-wake-route-traversal@1` row per route,
independent from the foreground positions and every other Pi route. The Store derives the complete
bounded registration set for that route, checks its exact authority and retained selected ID, and
commits the next visit before work. Rows count toward existing capacity and expire with the route;
maintenance removes revoked or compacted hints without changing receipts or uncertainty. See the
[route isolation decision](../adr/2026-11-14-isolate-pi-wake-route-traversal.md).

## Pre-native currentness repair report

The proposed [complete currentness decision](../adr/2026-11-15-revalidate-all-pre-native-wake-contention.md)
repairs #937 and #938 without changing stored or public shapes. Only `WakeReconcileChanged`
identifies a known logical race. The journal catches it across the entire page operation,
including checkpoint restore, then takes a fresh Store writer transaction. It checks current
source/CAS, exact subscription/opening identity, expiry/closure, retained checkpoint integrity
and current bounded private recorders before returning `WakeReconcileDeferred`. Generic conflicts,
corruption, persistence errors and native uncertainty never enter that branch. Pi reauthenticates
its route and installation before returning the existing empty response.

| Boundary | Handling | Deterministic evidence |
| --- | --- | --- |
| Inner projection after final-state capture | Typed change reaches the journal before any stale page can commit | `wake_cli_pi_poll_defers_checked_observation_and_checkpoint_contention` renews a real checked lease at the inner barrier; `wake_cli_pi_poll_defers_public_run_recorder_contention` commits public completion there |
| Outer source/database observation | Same validated decision, including verification of current artifacts | The same tests retain the original after-projection heartbeat and completion interleavings |
| Checkpoint restore and page cursor CAS | Typed replay changes propagate through the journal; invalid shape, provenance or retained digest remains terminal | Competing authenticated poll commits a checkpoint while the paused page expects none; existing checkpoint provenance and long-history/restart fixtures remain |
| First and final transactional recorder checks | Re-read exact Task snapshot, validate every process and joined cancellation/checked lease, then signal change; rollback the whole candidate page | `wake_cli_pi_poll_defers_both_atomic_recorder_checks_before_run_lease` uses public `runs::begin` before its lease, testing both checks |
| Public cancellation during page reconciliation | Validate current cancellation/process/checked writer and epoch before deferral | `wake_cli_pi_poll_defers_public_cancellation_recorder_contention` cancels a real running Worker, then resumes to checked completion on the same route; `wake_cli_pi_poll_public_cancellation_cannot_mask_corruption` adds corrupt/foreign/nonprivate cancellation, checkpoint damage and route revocation |
| Concurrent valid recorder activity plus invalid evidence | Refuse before transport/native intent; no stale checkpoint or outbox admission | `wake_cli_pi_poll_inner_contention_cannot_mask_invalid_current_evidence` and its outer/commit counterparts combine a public completed run with damaged history/checkpoint (including missing checkpoint with retained progress), mismatched Task columns, corrupt/foreign/nonprivate recorder, missing process, invalid lease, closure/expiry, changed identity or revoked route at all three projection/commit seams |
| Native admission and callback lifecycle | Existing uncertainty and finite attachment fences remain authoritative | Preserved native no-resend and recording Pi extension tests; no new native call or retry policy |

The #937 public regression was first run against the unchanged implementation and failed with
`Wake recorder changed at commit`. The positive cases require the exact successful empty Pi
response, no early checkpoint/envelopes/transport/native fence, continued same-route polling and
later authoritative completion. Cancellation may retain an earlier historical stopped observation;
its metadata asks the host to inspect current evidence rather than assert a stale final state.
No foreground drain, reconnect, extra host authority or larger poll allowance is needed.

## Clock and publication boundaries

The proposed [clock and recorder publication decision](../adr/2026-11-15-capture-coherent-wake-clocks-and-publish-single-link-recorders.md)
continues the cumulative candidate for #942–944 without changing persisted contracts.

| Public boundary | Validation and ordering | Evidence |
| --- | --- | --- |
| Observation clock capture → journal inspection | Capture initial clock, expose a deterministic barrier, then inspect exact retained registration/checkpoint | `wake_cli_pi_poll_clock_capture_precedes_new_checkpoint`: a second authenticated CLI poll commits its genuinely newer checkpoint inside this window |
| Checkpoint restore → source capture | Retained digest, schema, configuration, checked prefix and run facts stay required; standalone explicit clocks stay strict | Existing checkpoint provenance, paged replay and corruption regressions |
| Checked final-state capture → coherent live clock | Sample live clock after checked prefix capture; reject rollback, expiry and invalid clock range | `wake_cli_pi_poll_clock_capture_precedes_checked_append`: a real public run commits completion at a controlled later policy time before source capture |
| Event policy time → inner currentness | Validate every candidate event against coherent clock before marking a checked race; no generic future-time catch-all | Explicit-clock future-history negatives and concurrent-invalid-evidence matrices |
| Inner → outer source/database currentness | Propagate only typed logical change to fresh serialized validation | Preserved inner/outer heartbeat, recorder and corruption tests |
| Fresh serialized current evidence → bounded empty Pi response | Validate current source/CAS, scope, expiry/closure, retained checkpoint, all current timestamps and selected private recorders | Clock-race regressions require no stale checkpoint/envelopes/transport/native intent and later completion on the same route |
| Page cursor CAS and both transactional recorder checks | Preserve source/subscription authority validation and recorder checks before and after writes, rolling back stale pages | Preserved competing-poll and both-recorder-check public regressions |
| Initial process/cancellation recorder visibility | Complete private file is synchronized, then absent-only renamed on Linux; every visible destination has one link | Publication barrier exercises genuine public run/cancel writers while a poll reads the new recorder; foreign extra links remain refused |
| Native intent and receipts | Existing durable intent, original-session ordering and uncertainty fences remain independent | Preserved no-resend, attachment-generation and probe tests; no live host certification claimed |

The observation fixture uses a clock file only below the existing debug barrier root. Public Task
writers use the existing debug fixed-clock seam. Neither mechanism exists in release behavior;
barriers determine writer order, with no clock-tick sleep or retry deciding a race.

The three baseline public-CLI regressions were run before the repairs: checked append failed with
`Wake observation precedes checked history`; the competing poll failed with
`Invalid wake checkpoint scope or clock`; recorder publication failed with
`Wake authority requires a canonical owner-private path without links` (the visible file had two
links). The publication regression now also holds a real process writer at visibility while
both transactional recorder checks run. Cancellation visibility, foreign extra links, collision
contention, explicit-clock refusal, rollback/zero/future clocks and concurrent scope/digest-valid
checkpoint faults have separate assertions. No stale page or new native intent is allowed on a
deferred poll. A later native intent remains a no-resend fence.

## Retained rebuild and clock monotonicity

The proposed [retained-state and clock decision](../adr/2026-11-15-validate-retained-wakes-before-rebuild-and-fence-every-clock.md)
repairs #948 and #949 without changing persisted contracts or host support claims.

| Boundary | Required validation | Deterministic evidence |
| --- | --- | --- |
| Serialized initial inspection → restore or replay | Actual subscription/configuration/opening, registration prefix, checked source/CAS, retained checkpoint boundary/provenance and stored progress; independent of current facts digest | `wake_cli_pi_poll_validates_retained_checkpoint_before_cancellation_rebuild` retains an old checkpoint, publishes genuine public cancellation, then tests foreign scope, boundary, progress, source sequence and provenance damage |
| Matching facts → restore; new facts → opening replay | Only valid retained state reaches either choice; legitimate new cancellation rebuilds ordered historical observations and completion | The same fixture's valid case resumes to authoritative completion on the same bounded attachment; native intent still fences resend |
| Page commit → retained replacement | Same current integrity validation under the writer transaction, plus exact expected-checkpoint and source fences; both recorder checks remain | `wake_cli_pi_poll_cancellation_rebuild_revalidates_replacement_at_commit` substitutes retained checkpoint/progress after successful initial validation; existing competing-poll and concurrent-invalid-evidence matrices remain |
| Initial → post-source → serialized fresh clock | Every sampled live clock must be nondecreasing; current event/checkpoint times, safe integer range and expiry still checked | `wake_cli_pi_poll_serialized_clock_is_monotonic_across_all_samples` uses relative samples 100/102/101 and 100/102/99 with all history older than 99; equal/increasing samples defer and later complete |
| Typed change → bounded empty Pi response | Shared serialized integrity validation, fresh monotonic clock and current private recorder validation; no stale progress, packet, transport or native fence | Both new fixtures assert unchanged retained state/no new delivery effects on refusal; preserved inner/outer/recorder currentness regressions |

The initial integrity inspection does not compare checkpoint time with the captured live clock.
A valid competing poll can therefore retain a newer checkpoint after initial clock capture and
still take the existing narrow serialized deferral path. Normal success, restoration, opening
replay and deferral all retain source/CAS and provenance checks; native uncertainty never enters
this pre-native retry decision. These changes follow the accepted research recommendation above.

Before these repairs, the new cancellation fixture produced a native wake packet from a
checksum-valid foreign-scope old checkpoint. The three-sample clock fixture returned a successful
empty response for 100/102/101. Both failures were reproduced before changing the runtime; the
repaired fixtures also exercise unchanged retained corrupt progress, checked-boundary damage,
invalid provenance and a valid cancellation rebuild. No live host call or certification is claimed.

## Retained history integrity

The proposed [retained-history decision](../adr/2026-11-15-verify-retained-wake-history-references.md)
repairs #952 without changing the closed `WakeCheckpointV1` shape.

| Fields or boundary | Validation and use |
| --- | --- |
| `through` | Exact sequence/event pair in checked canonical history; matches stored extraction progress and lies within the current checked source |
| `lease_boundary` | Exact sequence/event pair of the latest Opened, LeaseTaken, LeaseRenewed or LeaseReleased at or before `through`; a real event of another role or an earlier valid lease refuses |
| `prefix_digest` | Exact rolling hash of original RunEvent bytes from opening through `through`, seeded by the registered Task identity; shared canonical hash helpers with projection, using unchanged 4096-row/16 MiB pages |
| `schema`, `configuration_digest` | Closed current shape and exact registered configuration; subscription, Store incarnation, opening, registration prefix, source/CAS and retained control provenance remain independently checked |
| `run_facts_digest` | Valid digest selects restore only for identical authenticated facts; changed facts select replay after source integrity validation |
| `prior_semantic` | Closed bounded lifecycle identity/state; prior-facts-derived semantics are discarded on changed facts and rebuilt from opening, never used to authorize continuation on that path |
| `observations` | Shape and subscription allowance checked; opening replay rebuilds prior-facts-derived counts, while actual retained distinct envelopes still enforce the cumulative allowance |
| `observed_unix_ms`, `clock_observed` | Valid bounded time/boolean shape; existing live/explicit-clock monotonicity, source-time, expiry and restore provenance checks remain. Opening replay rebuilds clock-dependent semantics |
| Initial inspection, replacement, deferral | Same actual retained object and metadata validated under Store writer serialization before restore/replay or bounded empty response; refusal changes no retained progress or delivery effects |

The authenticated public cancellation fixture covers lease event-ID mismatch, an exact real event
of the wrong role, stale opening lease and altered prefix fingerprint at all three boundaries.
A genuine maintenance writer forces the deferral case after source capture. Existing through,
progress, source-sequence and provenance controls remain, including byte-identical checkpoint
text with damaged metadata. The valid older-checkpoint case rebuilds after cancellation and later
retains ordered authoritative completion on the same attachment; native uncertainty fences resend.
The accepted research source and recommendation above remain unchanged. This candidate adds no
live host evidence or new execution authority.

Before the runtime repair, all four new authenticated CLI regressions failed by returning a native
wake packet from their checksum-consistent corrupt checkpoint after genuine cancellation. After
repair, the focused retained-history/rebuild selection passed all six tests, including each of the
four faults at inspection, replacement and serialized deferral. The unit prefix/suffix fixture and
extended long-history fixture separately cover source-boundary and multi-page fingerprint checks.
