# Review Tasks

The versioned Task-file entry point runs standalone Review through the common Task runtime:

```text
af review plan --file review.json --state /tmp/af-review-state --json
af task explain review-cli --state /tmp/af-review-state --json
af task run review-cli --execute --state /tmp/af-review-state --json
```

`af review run --file review.json` captures, plans and executes in one command. The file must
declare `kind = "review"`. Its Pipeline and Worker packages come from committed
`.af/task-catalog.toml`. The catalog's `review` table declares required reviewer names, their
Demand requirements, the severity gate and bounded convergence policy. Its optional
`generation` key may only be `2`, the one Review generation; omitting it means the same
`af.review-task-policy/2`. The existing captured
code-check policy supplies the required checks. Command Workers and native Model Workers use
the same runtime; [Model bindings](model-bindings.md) require captured identity mode and charged
capability admission inside the Task. Pi Kimi has explicit unavailable account identity and
requires trusted policy without principal independence.

Default `af onboard` emits a standalone native Review Pipeline and its exact example Task file
at `.af/task-packages/onboard/review-code/task.json`. Copy it to `../review.json`, choose a fresh
Task ID and commit the generated authority. Plan a change with
`af review plan --file ../review.json --authority HEAD --base HEAD --uncommitted`, inspect its
recorded plan, and approve that exact plan with `af review resume ID --confirm-plan PLAN_ID`.
`run --file` creates a new Task; it does not silently replace an existing capture.

The executable fixture is `fixtures/task-runtime/review`. Copy it into an empty directory,
initialize and commit a Git repository there, and run the commands above. It has two command
reviewers and one deterministic check. One reviewer reports an open major Finding, so the
final `run` exits 3. No credentials or model calls are needed. `fixtures/task-runtime/review-v2`
runs two Rounds, so its second Round disposes of the first Round's Finding.

```text
                    one Task / Store / budget
                    =========================
 captured source ------+--------------------------+
                       |                          |
 empty/prior history -> review-bind                check
                       |                          |
                       +---- typed Subject -------+
                       |                          |
                  correctness                    bugs
                       |                          |
                       +----------+---------------+
                                  |
                           review-reduce
                         /                \
                complete gather       missing required input
                      |                        |
              FindingSet + DemandSet    selected results remain
              closed Round + history   visible; no partial Ledger
                      |                        |
             Pass / ChangesRequested          Incomplete
```

`review-bind` derives a whole-tree or diff Subject from declared immutable source ports. Diff
binding compares the captured manifests and preserves the canonical patch, changed paths and
rename policy. The `af/TaskReviewSubject@3` Subject carries that scope and declares the patch
as a read-only file with its exact content ID and size. Its payload declares
`af.task-review-subject/3`; retired Subjects are refused. Shared borrowed identity and scope
validation covers the wire form and its resolved host view, retaining complete changed paths
and the existing context bounds. Binding an empty Diff is refused. The
prior history input is explicit; only a root port declaring the empty Review history default
can synthesize the initial value. `review-bind` also emits one `af/TaskReviewAssignment@1` per
configured reviewer, listing only that reviewer's own current prior Findings. The reviewer's
`review.kernel/ReviewerResult@2` must dispose of each assigned Finding exactly once.

`review-reduce` is one atomic gather and canonical reduction barrier. Every configured reviewer
must have a binding in the definition. Reviewer bindings use the reserved verification role,
the same source, Subject, checks and history as the reducer, and their own assignment from
`review-bind`. Optional reducer ports allow
failed or suppressed execution to produce an incomplete receipt; they do not permit omitting
a required reviewer from the definition. No authoritative partial Finding or Demand Set is
published. A failed reducer itself leaves the public obligation missing.

The domain reuses the canonical reducer and Finding projection. Immutable prior Rounds rebuild
that same Ledger without a second database or Attempt allowance. Review history preserves the
original closed Round and exact canonical views; a failed gather leaves the previous history
intact. Final admission recomputes the receipt against its recorded Task, plan, policy and
selected inputs.

| Review output | Generic Review Task acceptance | CLI exit |
|---|---|---|
| Complete, clean | Satisfied | 0 |
| Complete, open blocking Finding or required Demand | Satisfied | 3 |
| Missing reviewer or unavailable required check | Inconclusive | 4 |

Completing a Review Task means producing its required review evidence. The separate domain
conclusion still governs whether the reviewed change passes. `af task run` retains these
Review exit codes on replay, and inspection includes the typed Round receipts.

## Ledger inspection and contained checks

`af review ledger ID --json` prints `af/review-ledger@3`: the complete selected canonical Finding
Set and original Report envelopes, plus current statuses, severity counts and reviewer coverage.
A later Round containing only `not_reproduced` or `dispute` dispositions retains the unresolved
original Finding. A dispute changes its current status to contested; not-reproduced does not
verify a fix. Text and SARIF retain the same complete Finding selection and original Report
identities. `--round N` selects a recorded Round. `af task findings` is the separate current-Round
report view; absence from that view never closes a canonical Finding.

The captured code policy's `require_container = true` selects the shared container provider for
local checks. Unavailable containment fails ordinary isolation admission before a check runs.
Successful native check execution records `af/TaskCheckContainment@1` beside the exact
`af/TaskCheckReceipt@2`, retaining admitted Container isolation, the pinned default image and
observed execution. The receipt's check results still decide acceptance, including command
failure and source-integrity refusal. Private container runtime paths replace host HOME/scratch;
`[warm]` and native host-toolchain mappings remain incompatible with required containment.
Worker execution keeps its own environment authority. See the
[live proof and its execution boundary](../security/containment-probes.md).

Before checks or Worker dispatch, compilation proves schema-required verifier inputs from the
bound Task inputs and flattened wiring, including owned Scatter reviewer templates and their
kernel-generated Slice inputs. Empty root artifact lists are absent. The bounded proof handles
root property counts and name restrictions through schema composition, local references and
conditionals; unsupported presence constraints refuse admission. Schema-valued property
restrictions retain their Boolean composition and local references. A `oneOf` with multiple
branches requires proven exclusive acceptance; individually safe branches alone do not establish
it. A single-branch `oneOf` uses that branch's presence proof and retains runtime value validation.
A goal-only Review whose reviewers require Requirements must provide an explicit `requirements`
table or issue capture; otherwise planning refuses it. Reviewers with permissive schemas may omit
Requirements, including Pipelines without a public requirements port. Value-dependent presence
conditions that cannot be proved safe are refused; payload validation still runs at invocation.

## Human Review decisions

Read the selected complete Review with `af review decision-context TASK --json`. Its
`af/review-decision-context@1` document exposes the exact Review artifact, policy, Subject,
candidate Snapshot, Base, canonical Finding and Demand Sets, current Finding view IDs,
resolutions and persisted policy time. Incomplete Rounds have no human decision authority.

Write a closed `af.review-decisions/1` JSON batch. Copy `review_id` and the Review's `policy_id`,
`subject_id` and `snapshot_id` from that context. Include `base_snapshot_id` for a Diff, copied
from `subject.base_snapshot_id`; omit it for a whole-tree Subject. Set the asserted `actor`,
the batch's `reason`, and one to 32 actions. The whole file is bounded to 256 KiB.
The current input schema is `schemas/review-decisions-v1.json`:

| Action | Required data and authority |
| --- | --- |
| `attach` | `demand_id`, `content` (one to 65,536 UTF-8 bytes). The kernel stores observational bytes in CAS and records exact Demand, current Subject and actor. |
| `satisfy` | `demand_id`, `evidence`, `reason`. Evidence is `{ "kind": "attached", "action": 0 }` for an earlier zero-based attachment in this batch, or `{ "kind": "recorded", "artifact_id": "sha256:…" }` for already attached Evidence of this Demand. It must be current on this exact Subject. |
| `reuse` | `demand_id`, `satisfaction_id`, `reason`. Explicitly authorize an existing satisfaction after a Subject change. Original Evidence and satisfaction provenance stay unchanged; the containing Task binds the new Subject. |
| `reuse_evidence` | `demand_id`, `evidence_id`, `reason`. Explicit cross-Demand reuse requires identical claim, measurement method, rationale and requirement. The new Evidence retains the original Evidence reference and exact content. Implicit cross-Demand satisfaction is refused. |
| `waive` | `waiver`, the canonical DemandWaiver payload with exact Demand, Subject, policy revision, actor and reason. |
| `resolve` | `resolution`, the canonical FindingResolution payload. `rejected` requires evidence. `wontfix_tracked` also requires a sufficient severity ceiling, tracking reference and future policy-time expiry. Both require the current Finding view ID. `fixed` is refused here. |
| `challenge` | `challenge`, the canonical ResolutionChallenge payload naming the current Resolution. New evidence, higher severity, changed scope or expiry must justify reopening; repeating the Resolution's existing evidence is insufficient. |
| `policy_time` | `time`, the canonical PolicyTime payload with actor, reason and a strictly increasing positive safe-integer `tick`. Expired tracked resolutions become contested through recorded challenges. |
| `group` | `grouping`, the canonical FindingGrouping payload (`group` or `ungroup`, directed `from`/`into`, original Round), plus `from_view_id` and `into_view_id`. Both exact current views are required. Grouping never changes original Report or Finding identity. |

The canonical `waiver`, `resolution`, `challenge` and `time` payloads use the same actor as the
batch; policy-bearing payloads use its exact original `policy_id` as `policy_revision`.
Evidence references must exist and verify in CAS. This records the operator's explicit decision;
the actor string itself is not an authentication credential. The reviewed exact plan authorizes
only its captured batch, under the original policy and Subject.

```text
af review decide decisions-1 --from review --file decisions.json --json
af review inspect decisions-1 --json
af review resume decisions-1 --confirm-plan PLAN_ID --json
af review decision-context decisions-1 --json
```

`decide --execute` explicitly authorizes the captured batch in one command. Use a fresh Task ID;
resume retains the existing exact batch and never rereads an edited decision file. Onboarding
emits `onboard/review-decisions`; a committed catalog can name the equivalent
`kernel/review-decisions` with `--pipeline`. These are native Task packages, with no Campaign
selector or execution fallback.

The deterministic Task has no Workers, token reservation or Attempt, and cannot integrate,
deliver or publish source. It binds the original complete Task result and exact accounting conclusion as typed root inputs,
replays the selected complete original Round, applies canonical
transitions, then publishes new exact sets and history through the common fenced Task lease.
A refused batch publishes no partial Round. Original claims, Base, history and charges remain
immutable. Each Task reports its own cumulative accounting; a decision Task spends zero tokens
and does not advance a discovery Round, reset convergence or erase earlier charges. Existing
clean-window and Round bounds still govern the verdict. Evidence remains observational or
explicitly authorized under this policy; it grants no source or publication authority.

Change attestation and positive/negative fix verification remain the native
[bounded repair](bounded-repair.md) path: current checks, exact S1/S2 lineage, reserved independent
verification and canonical fix receipts. A decision batch cannot manufacture a fixed claim.
Telemetry observes committed native outputs and their original producers. A capture made before
recording began remains an explicitly missing prefix; telemetry never supplies Ledger authority.

See [the Review facade](campaign-review.md) for commands, output contracts and retired selectors and
[bounded repair](bounded-repair.md) for current-Snapshot repair operators.
