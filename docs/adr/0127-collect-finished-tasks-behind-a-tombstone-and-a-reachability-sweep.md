# ADR-0127: Collect finished Tasks behind a tombstone and a reachability sweep

Status: accepted, 2026-09-28. Amends
[ADR-0123](0123-warm-task-checks-through-a-toolchain-keyed-bounded-cache.md): the Warm Check
Cache gets a second, hard byte bound, and only that bound ends a running check.

Implements package R5 of [`docs/design/research-pipelines.md`](../design/research-pipelines.md)
under that plan's §2 ("old records stay readable", "nothing is weakened").

## Context

A Task Store only grows. Every Task keeps its revision, plan, execution records, Attempt
transcripts (`raw_artifact_ids`), results, delivery records and every Snapshot it captured or
derived in one content-addressed store (`state/cas`), and nothing ever removes an object. A
campaign of research Tasks leaves gigabytes behind, and an operator cannot even see which Task
holds what.

`af review gc` exists for Review Campaign state: each Campaign has its own directory, so it
removes whole directories. A Task Store is not like that. All Tasks of a project share one event
log and one CAS, and objects are shared freely: two Tasks over one commit share every source
blob, a bound input (ADR-0117, ADR-0126) is another Task's artifact, and a captured Review's
Campaign records reach the same Snapshots its Task does.

The CAS's contract is also one-way. Every event may reference only an object already durable
(`crates/review-store/src/cas.rs`), replay verifies every referenced object again, and a missing
object is corruption. So an object can be removed only when no record that replay will read
reaches it any more, and a Task whose objects are gone must never be replayed as if they should
be there.

## Options

- **Delete a Task's events, then its objects.** Rejected. The log is append-only and its dense
  per-run sequence is its ordering authority; a Task that disappears from the log takes its
  spend, its outcome and its ID with it, and nothing stops a later Task from reusing the ID and
  inheriting its references. A crash between the two deletions leaves either dangling objects or
  a log that no longer says what happened.
- **Keep a reference count in the CAS.** Rejected. Every put, every event and every record that
  names an artifact inside its payload would have to maintain it, in two stores that commit
  separately; one missed increment removes an object a live record needs, and a crash between a
  put and its count leaks or double-frees. A count is a second authority beside the log, and
  the log already says exactly what is referenced.
- **`af review gc` semantics: remove whole directories.** Rejected. A Task is not a directory:
  removing a Store removes every Task in it, and removing any part of a shared CAS by owner is
  the reference-count problem again.

## Decision

### Collection is one versioned transition

`af task gc --state DIR --older-than DAYS --keep N` previews; `--apply` collects. It collects
finished Tasks beyond the newest `N` finished ones whose last event is at least `DAYS` days old.
A Task that is `running`, has not finished, holds a live writer lease, or is named by another
uncollected Task's `af/TaskInputBindings@1` (any revision's) is never collected, and the preview
says which and why: `kept_newest`, `kept_recent`, `running`, `unfinished`, `writer_lease`,
`bound_by`. Without `--apply` the Store is opened read-only and nothing is written.

`--apply` first takes the **Store lease**: the Store's exclusive SQLite writer lock (`BEGIN
IMMEDIATE`), which every append takes too, so no event commits while it is held. It is refused
while any Task — collected or not, candidate or not — holds a live writer lease, and the refusal
names the Task. Under the lease it plans again and appends, for each collected Task, one
`TaskTransition@5` whose change is `task_collected`, carrying `af/TaskCollected@1` inline: the
Task's ID, kind, current revision ID, outcome, chargeable tokens, last event time, collection
time and `collected_bytes`, the bytes the plan computed. Its writer is `af-task-gc` at the
Task's epoch plus one, which fences the Task's last writer; its `artifact_refs` are empty. It
commits, and only then sweeps.

### The sweep is a function of the log

Under the Store lease again (and again refused while a writer lease is live), the sweep walks
every record the Store holds that is not collected — every event of every uncollected Task and of
every Campaign or other run, and every Attempt wall row — and removes each CAS object the walk did
not reach. Roots are each event's `artifact_refs` and every digest its payload spells; the walk
goes through every digest each reached object spells (an envelope's `content_id` and
`input_artifacts`, a Manifest's file digests, any identity a payload names). The rule is
conservative on purpose: a spelling that is not a reference only keeps an object longer, and no
reference, however a record nests it, is ever missed. An object filed at or after the Store lease
was taken is kept: it can only belong to a writer that has not appended yet.

Because the tombstone references nothing, a collected Task's objects become unreachable unless
another record reaches them, and those stay. Because the sweep reads only the log, a process that
stops between the tombstone and the end of the sweep leaves a Store that is consistent — a
tombstoned Task whose objects still exist — and the next `--apply` finishes the sweep from the
same rule without a second tombstone.

An append now checks, under the writer lock it already takes, that every object it references is
still filed. A command that verified an object before a concurrent sweep removed it is refused as
dangling and never commits the reference; `af task gc --apply` is the only command that removes a
CAS object (`Cas::remove_unreachable`).

### A collected Task's projection stops at the tombstone

`task_projection` of a collected Task returns `StoreError::Collected` before it reads any
artifact, so replay never reports a missing object of a collected Task as corruption, and every
command that acts on a Task — `run`, `explain`, `output`, `deliver`, `refresh` — refuses with
`Task <id> was collected <time>`. Task enumeration skips a collected log; `collected_tasks` lists
them from their tombstones and the result ID their `finished` transition named. `af task list`
lists each as `collected <time>` with the retained summary (its `af/task-list-entry@2` carries
`collected`), and `af task show` prints the summary instead of the artifact-backed sections
(`af/task-collected-inspection@1` for `--json`). The browser lists the Tasks it can open.

### Sizes

`af task list --sizes` runs the same walk and prints, per uncollected Task, the bytes and objects
only that Task reaches and those it shares with another Task or Campaign record, plus the Store's
total bytes, object count and unreachable bytes; `--json` adds `sizes` to each entry and `store`
to the document. The preview's `reclaimable_bytes` is what a sweep after the plan removes —
everything no retained record reaches, including objects nothing reached already — so the Store's
total drops by exactly that amount when nothing else writes in between.

### The Warm Check Cache's two bounds

R2's implementation Attempt was ended 61 seconds into its gate because the warm directory,
already at 13.5 GB from earlier Tasks, crossed its single 16 GiB bound while the candidate
compiled: a check failed for growth it did not cause. ADR-0123 is amended:

- `[warm] max_bytes` stays the **eviction bound**. A key above it is removed before a check (the
  check runs cold, as before) and after one, under the key lock — and that check's own result
  stands. Only the next check finds the directory gone.
- A new `[warm] hard_max_bytes` is the **only bound that ends a running check**, with the same
  `warm_cache_bound_exceeded` reason, during the check or when it ends. It defaults to twice
  `max_bytes`, at most 32 GiB — the hard maximum ADR-0123 declared, which this does not widen —
  and a declared value must lie between `max_bytes` and 32 GiB. With `max_bytes` at 32 GiB the
  two bounds coincide.
- `TaskCacheObservationV1` gains an optional `bound`: `max_bytes` for a removal before a check or
  an eviction after one whose result stood, `hard_max_bytes` for a check the bound ended; never
  beside a `suspect` eviction. `af task show` prints it after the eviction's cause, as in
  `removed 6144 (bound_exceeded max_bytes)`.

Suspicion and uninspectable directories keep ADR-0123's rules: they fail the check.

## After the first verification

The package's review (Task `research-r5`) changed four rules, and the text above reads as amended:

- **A reader that loses an artifact to a concurrent sweep reports the Task as collected.** A
  projection that fails on a missing artifact checks for a tombstone committed meanwhile and
  answers `collected`, never corruption; the Store stays consistent either way.
- **A listing projects the uncollected Tasks first and reads the tombstones after**, so a Task
  collected between the two is skipped by the projection and listed from its tombstone, and no
  Task the log retains is omitted.
- **Opening a Task re-checks every Task its bindings name, inside the opening transaction.** A
  referenced Task collected after the binding was resolved refuses the opening, so `gc --apply`
  cannot collect a Task another Task is about to bind.
- **A collected Task's row under `--sizes` carries a zero footprint** in both formats, rather
  than no numbers.

## After the verification

The package was verified (Task `research-r5-verify-1`); the three interleavings its reviewers
still reported were reconciled after the verdict and are not re-verified. A reader never holds a
lease against the sweep; it tolerates the sweep instead:

- A listing whose projection lost a Task's revision to the sweep skips that Task and lists it
  from its tombstone; a Task tombstoned after its projection is listed once, from the tombstone.
- `list --sizes` reads its rows and its footprints twice when a row has no footprint, so a Task
  collected between the two reads is listed from its tombstone with a zero footprint.
- Scheduled interleaving fixtures for these paths are a follow-up; the reconciliations are
  pinned by the existing collection tests and reviewed by hand.

## Consequences

An operator can see where a Store's bytes are and reclaim them without losing a Task's record:
its outcome and spend stay listed forever, and its ID stays taken. What collection cannot do is
bring an output back; a collected Task cannot be delivered, bound or inspected in depth, which is
why a bound Task is refused and why only finished Tasks beyond `--keep` are candidates.

The walk reads every reachable object once, so `--sizes` and `gc` cost a pass over the CAS.
Nothing is cached between runs.

`af task gc --apply` waits for the Store's writer lock and is refused while any writer lease is
live, so it runs between Tasks, not beside one. A command that starts while a sweep holds the lock
waits or fails and must be re-run; it never records a reference to a removed object.

Old records stay readable: every new field is optional, a Store without a tombstone replays as
before, and a code policy without `hard_max_bytes` gets twice its `max_bytes`. This repository's
policy sets `max_bytes` to 32 GiB, so its two bounds coincide until a human lowers `max_bytes`.

`crates/review-store/src/store/task/tests/collection.rs` and `crates/af/tests/task_gc.rs` pin
the rules; `crates/af/tests/task_warm_checks.rs` pins the two bounds.
