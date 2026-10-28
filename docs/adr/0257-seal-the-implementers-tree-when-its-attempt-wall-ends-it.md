# ADR-0257: Seal the implementer's tree when its Attempt wall ends it

**Status:** accepted (2026-10-26). Extends
[ADR-0212](0212-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md)
(a continuation also binds the partial tree an Attempt wall left) and
[ADR-0208](0208-recover-an-attempt-from-a-transient-provider-failure-and-a-reply-that-does-not-parse.md)
(an identity re-probe that proved nothing has its own Task cause). Keeps
[ADR-0049](0049-run-task-workers-through-shared-durable-attempts.md),
[ADR-0068](0068-retain-inflight-task-usage-in-the-common-budget.md),
[ADR-0129](0129-stop-task-workers-when-af-is-interrupted.md),
[ADR-0143](0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md),
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md) and
[ADR-0209](0209-recover-a-lost-or-interrupted-attempt-without-charging-its-reservation.md).

## Context

Issue #331, items 5 and 9, a 0.14.0 release condition. A source-writing Worker that reaches its
Attempt wall is killed by its runner. The kernel then dropped its sandbox: everything the
implementer wrote in half an hour was gone, and the next Task started again from the base. The
end read as a malformed reply: the runner's `implementer did not answer within …ms and was
killed` named no cause af's state model knew, the Attempt's retry feedback said its output was
refused, and `af task show` printed `provider_failed/malformed_reply` with `af task show` as the
next command. `af task plan` did not say how long one Attempt may take, so a wall too short for
the goal was found only after it ran out.

An identity recheck whose three probes proved nothing (`ProviderEventV1::IdentityReprobed`) fails
its call with the closed class `environment` (ADR-0208). The state model read the word
`identity` in that diagnostic and printed `identity_unverified`, an account problem with `af
provider setup ID --login` as the fix; and a real account change, `the Claude account differs from
the one captured at plan`, read the same.

## Decision

1. **The wall end is detected where the Attempt is.** A Worker Attempt failed at its wall when its
   runner says it killed the Worker at the deadline it was given (`… did not answer within …ms
   and was killed`), which is the remaining Attempt wall, or the Attempt's reservation deadline
   has passed, and its host did not cancel it. Only a node that runs a Worker slot is judged so;
   a check keeps its own wall and receipt.
2. **The tree is sealed as a candidate is.** When the Worker writes source (its contract has the
   kernel-captured `candidate` port), the host seals its sandbox exactly as `finish` seals an
   ordinary candidate, through the same capture: shell scratch and the project's ignore rules are
   left out, and what was left out is listed in `af.seal-ignored/1`. The tree becomes a derived
   Snapshot of the Worker's source with its own `af/SourceTree@1`, recorded as
   `af/TaskPartialCandidate@1` (`af.task-partial-candidate/1`,
   `schemas/task-partial-candidate-v1.json`): the parent Snapshot, the Snapshot and SourceTree of
   the tree and how many paths it changed. A Worker that changed nothing, or writes no source,
   keeps none; a seal that fails is named in the Attempt's error.
3. **The end is a typed record.** The runtime records `af/TaskAttemptWallEnd@1`
   (`af.task-attempt-wall-end/1`, `schemas/task-attempt-wall-end-v1.json`): the Task, the node,
   the Attempt, the node's `wall_ms_per_attempt` and the time the Attempt took, and the partial
   candidate when one was kept. Both records are raw evidence of the Attempt's settlement, never an
   output: nothing seals, checks, reviews, accepts or delivers a partial candidate, and the
   Attempt's other outputs are not admitted. The Attempt's error leads with what happened, `implementer
   reached its Attempt wall of 30 min after 30 min; its edits (3 paths) are kept as partial
   candidate sha256:…`, and its diagnostic's closed class is `attempt_wall`.
4. **Accounting and recovery are unchanged.** The Attempt settles `failed`, charged what its
   Provider reported, or zero with its usage unknown (ADR-0143), and counts against its node's
   Attempt limit; its node retries it within that limit as before, from the invocation's source.
   It is not abandoned, so it spends no recovery allowance (ADR-0209). No wall, deadline, budget
   or retry bound is lengthened. The order keeps that true when the writer fails meanwhile: the
   Attempt's reported usage is retained in the Store's usage sidecar first, so a CAS failure while
   the wall end is written loses no counter (ADR-0068); then the wall end is written, flushed and
   its ID kept in the sidecar's `attempt_wall_end` table, before the rest of the Attempt is
   published. A writer lost after that leaves its successor the record: recovery settles that
   Attempt `failed` at its wall, as its writer would have, with the wall end and the partial
   candidate among its raw evidence and the diagnostic class `attempt_wall`, charged what was
   reported (the reservation an incomplete report keeps), else zero with its usage unknown, and
   spends no recovery allowance; a `--from` continuation finds the partial tree. A kept wall end
   that is not that Attempt's own record (another producer, Task, node or Attempt) is refused.
   A kept wall end is a collection root like the Task's events (ADR-0135): `af task gc --apply`
   run after the lost writer's lease expired, before any successor, keeps it and the partial
   tree it names. Only a writer lost between writing the record and keeping its ID leaves an unsettled Attempt
   that recovery abandons, as before this decision.
5. **Two causes, and a new state version.** The Task state's causes add `attempt_wall`, of the
   outcome `exhausted` (exit 4), and `provider_environment`, of the outcome `provider_failed`
   (exit 6). The state reads the typed records first: a wall end on the node's last failed Attempt
   is `attempt_wall`; a diagnostic whose class is `environment` is `provider_environment`. The
   runtime writes that class when the Worker's transport reported the environment failure
   (`CallNotes::environment`, from `NativeFailureKind::Environment`): an identity re-probe that
   proved nothing, or a CLI that cannot start or serve. A diagnostic's own words still classify
   where no record says more, and now name `(environment)` as `provider_environment` and `differs
   from the one captured` as `identity_changed`, before `identity_unverified`. The next command
   after `attempt_wall` is `af task run NEW_TASK_ID --from TASK` when edits were kept, else `af task
   show TASK`; after `provider_environment` it is `af provider status`. The node's retry policy
   is unchanged: an environment failure is retried within the node's Attempt limit, and its next
   Attempt probes again.
6. **The documents that print the state take new versions.** The state schema is
   `schemas/task-state-v2.json`, and every document that carries it is a new version whose
   previous one is neither printed nor read (ADR-0145): `af/task-inspection@14` (also
   `attempt_wall_ends`, `slots` and a partial `continues`), `af/task-list@4` and
   `af/task-list-entry@4`, `af/task-checks@3`, `af/task-collected-inspection@3`,
   `af/task-detached@3`, `af/task-findings@2`, `af/task-report@3`, `af/task-status@2` and
   `af/task-cancelled@2`. `af/task-gc@1` and `af/task-plan-inspection@1` keep their shapes and
   now name the new versions of the definitions they borrow. The Rust state types keep their
   names: a wire identity is the document's kind and schema, not a type name.
7. **Every rendering says it in the same words.** `af task show` prints `NODE: attempt_wall:
   implementer reached its Attempt wall of 30 min after 30 min; its edits … are kept as partial
   candidate …` and the next command; `--json` lists each wall end with its partial candidate
   under `attempt_wall_ends`; `af task run` prints `implement failed: its Attempt wall of 30 min
   ran out after 30 min; its edits are kept as a partial candidate` and ends with
   `finished/exhausted/attempt_wall`; `af task report` names the wall, af's own words and never a
   Worker's, where it names no diagnostic, followed by `; next: af task run NEW_TASK_ID --from
   TASK` when edits were kept. Its failure breakdown counts a wall-ended Attempt under the class
   `attempt_wall` of `af/task-report@3`, read from the wall end, never `provider_failure`, which
   the re-ask feedback of a timed-out model Worker would otherwise say.
8. **A continuation starts from the kept edits.** `af task run NEW --from TASK` (or a Task file's
   `continue` table without `round`) of a finished Task that recorded no review round binds the
   latest partial candidate as NEW's source, exactly as a bound derived tree is bound (ADR-0117):
   re-rooted over the identical Manifest. The wall end goes into NEW's revision provenance, which
   the Store keeps across selection and refresh; no findings travel with it, so no `findings`
   port is required. `af task show NEW` prints `continues TASK from the partial tree its node N
   left at its Attempt wall`, `--json` carries the partial form of `continues`, and `af task
   report` names it. NEW runs its whole pipeline; only its own verified result can be delivered.
   A Task that recorded a review round continues from its reviewed candidate as before.
9. **The plan shows each slot's wall.** `af task plan`, `start` and `explain` print each Worker
   slot's Attempt wall beside its binding (`implementer: claude-opus-5/high via claude, Attempt
   wall 30 min`), and the `--json` document lists `slots`: the slot, its Worker, its Provider,
   model and effort or a command Worker, and the longest `wall_ms_per_attempt` of the nodes it
   runs as. The baseline and candidate arms of an optimization experiment run as its children,
   not as nodes of the graph: their wall is the Attempt wall their captured Worker package
   declares, the one every arm Attempt runs under. Every slot a node or arm runs is listed with
   its wall however many there are; only bound slots nothing runs fold after eight lines.
   `--tree` prints the same walls beside each Worker node, and an experiment node names each
   arm with its wall.

## Consequences

- An implementer that runs out of time loses nothing it wrote: the next Task starts from those
  edits with one command, and the stop reads as the wall it was, with the wall and the time.
- A partial tree is one more Snapshot per wall-ended writer Attempt, retained with the Task's
  settlement records and collected with them (ADR-0135).
- A Task that reviewed a round and then had a later writer stopped by its wall continues from the
  reviewed candidate; the later partial tree stays recorded and visible, but `--from` does not
  choose it. A flag to choose it is a later decision.
- Scripts that read `af task … --json` must accept the new document versions. A Worker named in
  the runner's own timeout words keeps its qualified slot (`root.slots.implementer did not answer
  …`); the new leading sentence names its slot's own name.

The proof is `crates/af/tests/it/task_attempt_wall.rs` (a command implementer that writes two
files and sleeps past a 5 s wall, then a `--from` continuation that starts from them),
`crates/review-pipeline/tests/it/task_runtime/resilience.rs` (a model Worker killed at its wall
is charged zero with its usage unknown and spends no recovery allowance; an exhausted identity
re-probe is recorded as `environment`), the state unit tests and the schema parity tests of
`af.task-attempt-wall-end/1`, `af.task-partial-candidate/1`, `task-state-v2` and
`af/task-inspection@14`.
