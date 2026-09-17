# Worker warm layers

**Status:** design proposal, 2026-09-16, revised 2026-09-17 after its first `af review` Round
(Campaign `worker-warm-layers`, eight Findings, all accepted). Against `main` 166eca5
(v0.9.0-rc.2). No ADR yet. Companion vocabulary lives in [`entities.md`](entities.md) and
[`state-machines.md`](state-machines.md); the binding record is [`../adr/`](../adr/); the
delivery sequence is [`worker-warm-layers-plan.md`](worker-warm-layers-plan.md).

A Round 3 reviewer today pays the same cold start as Round 1: a fresh harness, a fresh tree, no
build cache, and a model that re-reads everything it already read twice. This design splits a
Worker Attempt into layers with distinct identities and lifetimes, so each layer can be carried
from Round N to Round N+1 as a declared, content-addressed input instead of rediscovered.

## What cold start actually costs

The cost is not one thing. It is five layers, paid at different scopes, and only one of them is
amortized today.

| Layer | Cold cost | Paid per | Evidence |
|---|---|---|---|
| Admission | One capability probe per distinct binding, thousands of tokens plus wall time | Campaign or Task | Already amortized. [ADR-0091](../adr/0091-capture-explicit-task-provider-admission-costs.md) records one probe at 5,712 chargeable tokens. |
| Harness boot | Seconds | Attempt | One `claude -p` or `codex exec` process per Attempt, spawned after the Gate passes. Not carried; see L2. |
| Tree | Seconds to tens of seconds for a large tree | Round for the template, plus one clone per Attempt | The template is built from scratch per Round in a fresh temporary directory. |
| Build caches | Minutes for anything that compiles or tests | Attempt | No cache reaches a Worker sandbox. The Gate builds the same head minutes earlier and its output is discarded. |
| Rediscovery | The dominant cost: repeated file reads and re-derived understanding | Attempt | [ADR-0028](../adr/0028-prioritize-wise-token-use-and-minimum-worker-context.md): later Rounds approached or exceeded one million tokens while reviewers rediscovered overlapping parts of the same change. |
| Prefix re-send | Full input tokens for the rendered prompt | Attempt | Each Attempt is a new conversation, so provider prompt caching cannot serve it even when the previous Round ended minutes ago. |

## The one rule

**Warmth is an artifact, never ambient state.** Every warm layer a Worker receives is
content-addressed, appears in the Attempt's context manifest with its bytes and estimated tokens,
and can be re-materialized from CAS. Delete the projection, replay the log, same inputs. This is
the only way warm Attempts stay inside the kernel's existing guarantees: minimum Worker context
(ADR-0028), declared inputs only
([ADR-0033](../adr/0033-configured-workers-authorize-declared-input-delivery.md)), one exact
manifest per Attempt, and a Ledger that is a pure function of the log.

The corollary is that no process, sandbox, or session outlives an Attempt with hidden state. A
layer is carried by capturing it at seal time and declaring it at the next reservation, not by
keeping something running in between. Nothing starts before the Gate has passed and the Attempt
is reserved, context-bound and identity-rechecked.

## The layer stack

```text
                identity key                        lifetime             cold cost it removes
------------------------------------------------------------------------------------------------
L6 Turn         attempt id + epoch                  one Attempt          none: this is the work
L5 Session      provider kind + principal + model   at most the cache    re-sent prefix and
                + effort + SessionSnapshot id       TTL; forked, never   re-reasoning, when the
                                                    mutated              next Round is soon
L4 Notes        WorkerNotes id from the previous    Campaign or Task,    rediscovery of the tree
                admitted Attempt of the SAME node   same node only
L3 Workspace    Tree Digest + BuildCache digest     Campaign or Task,    tree materialization,
                + mode                              re-based per head    dependency and build caches
L2 Runtime      provider kind + executable path     machine              nothing: identity and
                + auth directory + recheck receipt                       recheck receipt only
L1 Binding      package digest + principal          Campaign or Task     admission probe (today)
                + admission receipt
L0 Definition   package content digest              catalog pin          none
------------------------------------------------------------------------------------------------
A Cold Attempt uses L0 to L2 only. A Warm Attempt adds any of L3 to L5, each recorded.
```

### L4 Notes: the layer that pays for the rest

At the end of an Attempt the Worker may emit one bounded, typed artifact beside its result: which
paths it inspected, its model of the change, open questions, and per-path hints. It is an
inspection map, not a verdict. Verdicts already live in the Ledger as Findings. On the next Round
the kernel delivers the previous admitted Attempt's Notes to the same node, rendered under a
"data, not instructions" heading exactly as prior Findings and refused Attempts are rendered
today.

Paired with Notes, the kernel computes a **Head Delta**: its own artifact, `HeadDelta@1`, holding
the from and to Snapshot IDs, the diff policy identity, the complete add and delete path set with
rename truncation metadata, and one mark per path. The Change Set type is not reused: a Change Set
carries Subject identity and Report Scope, a Head Delta carries neither, and a whole-tree Subject
has no Change Set at all but still has two heads. Marks are computed over the union of the paths
the Notes reference, the previous Subject view and the current Subject view, so a path the Notes
mention that was reverted to Base or removed still receives a mark: `changed`, `unchanged`, `new`,
`reverted`, `removed`, or a rename state. **Delta Marking** is the rendering of those marks beside
the Change Set section. It costs a few bytes per path and tells the Worker where its Notes may be
stale.

### L3 Workspace: a stable root and a carried build

Each node in a Campaign gets a stable workspace root instead of a fresh temporary directory per
Round. When the head advances, the template is **re-based**: a copy-on-write clone of the previous
template receives the tree diff, and the result's manifest digest must equal the new head's Tree
Digest or the kernel falls back to a full materialization. Per-Attempt sandboxes remain fresh
clones of that template, so sibling isolation is unchanged.

The Gate has already built the head before any reviewer is dispatched. Its declared cache
directories become a **Build Cache**, `BuildCache@1`: a candidate-built artifact that is explicitly
unsafe and is never typed or named as a Cache Snapshot, because it was produced by candidate code
under a policy that can read anything the operator can. Its capture is closed: regular files only,
descriptor-relative no-follow traversal, entry, depth, path and byte limits, fixed modes, extended
attributes and ACLs stripped, and producer plus head provenance recorded. Worker sandboxes that
declare the `cargo_target` kind receive a copy-on-write clone with `CARGO_TARGET_DIR` pointed at
it, and the bytes are removed before seal. The handoff is admitted only under the trusted-local
policy; a safe pipeline refuses it before any Worker dispatch and keeps the existing
administrator-approved registry snapshot.

### L5 Session: opt-in, forked, measured, Claude only

Claude 2.1.273 accepts `--session-id`, `--resume`, and `--fork-session` in print mode, so the
kernel can assign each Attempt a session ID derived from its Attempt ID, capture the transcript at
seal into a **Session Snapshot**, and resume it next Round with a fork so the captured transcript is
never mutated, sending only a shorter delta prompt.

Capture is a two-phase protocol under the Attempt epoch: capture and verify the bounded session
bytes; append a prepared event referencing the CAS ID and the exact source identity; perform an
idempotent no-follow deletion from the harness directory; append a cleanup-completed event.
Recovery finishes cleanup without a provider call. Warm Set selection requires both the Attempt's
admission and its completed cleanup, so a crash between the phases leaves neither an orphaned CAS
object nor an ambient transcript.

Codex is excluded until it has a protocol of its own. The pinned 0.154.0 CLI has no `--session-id`
and emits its own thread ID; its `exec resume` and `exec fork` subcommands reject the adapter's
`-C` and `-s` flags; and its authentication directory is the same directory a kernel-owned session
home would replace. Codex Workers keep `--ephemeral`.

The layer is gated three ways: the provider must support resume, the previous Attempt must be
younger than a policy age so a prompt cache can plausibly still serve it, and the transcript's
estimated tokens must fit the reservation alongside the delta prompt. When any gate fails the
Attempt runs on Notes alone. The saving here is only real when cache-read tokens show it. The
design ships this layer behind a policy default of off until a dogfood baseline shows cache reads
on resumed Attempts.

### L2 Runtime: identity only

A harness process cannot host two independent conversations, so there is no pool to share, and a
process started before the Gate passes would read mutable authentication and configuration and
perform startup work outside any recorded reservation or fence. The runtime layer therefore
carries nothing. It keeps its identity key and the recheck receipt of
[ADR-0090](../adr/0090-recheck-native-task-provider-identity-before-private-invocation.md), and
the harness starts, as today, only after the Attempt is reserved, context-bound and rechecked.

## Round N to Round N+1

```text
Round N                                           Round N+1
--------                                          ----------
reviewer Attempt (admitted)                       reviewer Attempt (warm)
  |-- result   -> Ledger Findings         ---->     prior_findings          (exists today)
  |-- notes    -> WorkerNotes@1           ---->     notes                   (P1)
  |-- session  -> SessionSnapshot@1       ---->     resume + fork           (P4, if gates pass)
  |-- sandbox sealed and discarded
gate Attempt on head_N                            gate Attempt on head_N+1
  |-- cache dirs -> BuildCache@1          ---->     CoW clone into sandbox  (P2, trusted-local)
template(head_N) at stable root           ---->     rebase to head_N+1, verify digest (P3)
change_set(Base -> head_N)                        change_set(Base -> head_N+1)
                                                  HeadDelta(head_N -> head_N+1) -> marks (P1)

WarmSet@1 { node, round: N+1, source_attempt: A_N,
            notes: id | none, session: id | none, workspace: rebased | full,
            build_cache: id | none }               recorded before reservation, in the manifest
```

Selection is strict. Only an **admitted** Attempt of the same node in the previous **closed**
Round can be the source, and for the session layer only one whose cleanup completed. Fenced and
quarantined Attempts carry nothing. An incomplete Round resumes its already pinned inputs,
including its Warm Set. A retry within a Round is a new Attempt with a new epoch and inherits the
Round's Warm Set, not the failed sibling's state.

### Cold Closeout

A Round is only known to be the closing Round after its results are reduced, so a Round-level rule
cannot be scheduled. Cold Closeout is instead compiled into the graph: for each required reviewer
with warm layers, a conditional cold Attempt of the same node is dispatched inside the Round only
when the warm result would make the Round clean. Its reservation is protected in the same way the
Task runtime protects verifiers, both results fold into the Ledger before the convergence
decision, and feasibility accounting refuses a policy whose budget cannot admit the extra Attempt.
Warm Rounds find News cheaply; the cold Attempt confirms the clean one.

## Decisions

- **D1, warmth is declared.** Every warm layer is a CAS artifact recorded in the context manifest
  with bytes and estimated tokens. No layer is ambient. Replay reconstructs the same inputs.
- **D2, node-private layers stay private; caches are the one transferable layer.** Notes, Session
  Snapshots and workspace state are keyed by Campaign or Task plus node and never cross nodes.
  Build caches are typed Round artifacts carried Gate to Worker through declared ports with
  provenance checks. Slots declared independent are checked by the compiler. Other Workers'
  private reasoning stays absent by construction.
- **D3, Notes are advisory.** Notes never replace the explicit Report, Dispute, or Drop owed for
  every prior Finding. Silence is still not a Drop. Notes are rendered as data, bounded by policy,
  and dropped with a recorded reason when over the bound; the Attempt is still admitted.
- **D4, admitted sources only.** A Warm Set is built only from the previous closed Round's
  admitted Attempt of the same node. Fenced, quarantined, malformed, and released Attempts
  contribute nothing.
- **D5, sessions are forked, capped, and crash-consistent.** Session Snapshots are captured to CAS
  through the two-phase protocol, resumed with a fork, aged out by policy, and refused when their
  estimated tokens do not fit the reservation. Claude only; Codex keeps `--ephemeral`.
- **D6, Cold Closeout is compiled.** A conditional cold Attempt of the same node inside the Round,
  with a protected reservation, dispatched only when the warm result would make the Round clean.
  The default is one required reviewer; policy may set all or none, and an infeasible policy is
  refused at compile time.
- **D7, the build cache is explicitly unsafe and trusted-local only.** `BuildCache@1` never
  carries the Cache Snapshot guarantee. Safe pipelines refuse it.
- **D8, every warm Attempt reports.** The run report shows, per Attempt, which layers were used,
  rendered bytes, input tokens, cache-read tokens, and wall time. Any change to warm policy ships
  with a representative dogfood baseline or states why none exists, as ADR-0028 already requires.

## Contracts

### Vocabulary, in CONTEXT.md form

```text
**Warm Set**:
The exact, per-node set of carried layers one Attempt starts from: Notes, Session Snapshot,
workspace basis, and Build Cache, recorded before reservation and listed in the manifest.
_Avoid_: "warm cache" or "the previous session" as ambient state; a Warm Set is an artifact.

**Worker Notes**:
A bounded, typed inspection map one admitted Attempt leaves for the next Attempt of the same
node: paths inspected, model of the change, open questions, per-path hints. Data, not a verdict.
_Avoid_: treating Notes as a disposition; every prior Finding still needs its explicit
Report, Dispute, or Drop.

**Head Delta**:
The kernel-derived relation between two consecutive heads of one node: exact from and to
Snapshot IDs, diff policy identity, complete path set with rename truncation, and one mark per
path over the union of Notes paths and both Subject views.
_Avoid_: **Change Set**; a Head Delta carries no Subject identity and no Report Scope, and it
exists for whole-tree Subjects too.

**Delta Marking**:
The rendering of Head Delta marks beside the Change Set section: changed, unchanged, new,
reverted, removed, or renamed since the previous Round's head.
_Avoid_: a second full patch in the prompt; the marks exist to keep context minimal.

**Build Cache**:
A bounded, explicitly unsafe capture of a Gate's candidate-built output, admitted only under the
trusted-local policy and cloned into Worker sandboxes that declare its kind.
_Avoid_: **Cache Snapshot**; a Build Cache was produced by candidate code and carries no
administrator approval and no credential-free guarantee.

**Session Snapshot**:
The content-addressed transcript of one Attempt's harness session, captured at seal through a
two-phase protocol and re-materialized for a forked resume. Never mutated in place, never shared
across nodes.
_Avoid_: "resume the session" as a verb on a live process; the process ended with its Attempt.

**Cold Closeout**:
A compiled, conditionally dispatched cold Attempt of a required reviewer inside the Round that
would otherwise close on a warm clean result, holding a protected reservation.
_Avoid_: reading a warm clean Round as convergence on its own, or a Round-level rule the
scheduler cannot honor.
```

### Artifacts and events

```text
review.kernel/WorkerNotes@1        af/WorkerNotes@1        one per admitted Attempt, optional
review.kernel/HeadDelta@1          af/HeadDelta@1          one per node per Round after the first
review.kernel/BuildCache@1                                 one per Gate per Round, trusted-local
review.kernel/SessionSnapshot@1    af/SessionSnapshot@1    one per admitted Attempt, optional
review.kernel/WarmSet@1            af/WarmSet@1            one per node per Round, before reserve

WarmSetSelected@1          node, round, source attempt, layer ids     before AttemptReserved
WorkspaceRebased@1         from snapshot, to snapshot, verified digest, entries touched
BuildCacheCaptured@1       gate attempt, head snapshot, entries, bytes, limits applied
SessionSnapshotPrepared@1  attempt, session id, CAS id, source identity, bytes, tokens
SessionSnapshotCleaned@1   attempt, session id, deletion outcome
ColdCloseoutDispatched@1   node, round, warm attempt, cold attempt, protected reservation

WorkerNotes@1 payload
{ schema, node, attempt_id, head_snapshot_id,
  inspected: [{ path, tree_entry_digest }],
  model_of_change: string,            bounded
  open_questions: [string],           bounded
  hints: [{ path, note }] }           bounded; policy notes.max_bytes, default 16 KiB

HeadDelta@1 payload
{ schema, node, from_snapshot_id, to_snapshot_id, diff_policy_version,
  rename_detection_truncated,
  marks: [{ path, mark: changed|unchanged|new|reverted|removed|renamed, renamed_from? }] }
```

### Pipeline and package syntax

```toml
# .af/pipelines/review.toml  (reviewer node)
[[nodes]]
id = "correctness"
kind = "reviewer"
package = "correctness"
warm = { notes = true, workspace = "rebase", build_cache = ["cargo_target"], session = "off" }
# session: "off" | "if_recent" (policy warm.session.max_age) | "always"; Claude adapters only

[convergence]
clean_rounds = 1
max_rounds = 2
gate = "major"
cold_closeout = "one_required"      # "one_required" | "all" | "none"; compiled as a
                                    # conditional cold Attempt with a protected reservation
```

```toml
# worker.toml  (af.worker/1)
[signature.contract.inputs.notes]
artifact_type = "af/WorkerNotes@1"
cardinality = "one"
optional = true
[signature.contract.inputs.notes.affinity]
kind = "unbound"

[signature.contract.outputs.notes]
artifact_type = "af/WorkerNotes@1"
cardinality = "one"
optional = true
[signature.contract.outputs.notes.affinity]
kind = "same_as"
input = "source"
# The compiler wires outputs.notes of the previous admitted Attempt of the same slot to
# inputs.notes on retry and repair. Slots in independent_from never exchange notes.
```

### What is refused

- Sharing a node-private layer across nodes, slots, or Workers: Notes, Session Snapshots, and
  workspace state, including implementer to evaluator. Build caches travel only through declared
  Gate-to-Worker ports with provenance checks.
- A process, sandbox, or harness home that survives an Attempt with undeclared state, and any
  provider process started before the Gate passes and the Attempt is reserved.
- A raw transcript as the default carry. Notes are the default; a transcript is an opt-in
  transport optimization that must prove itself in cache-read tokens.
- Warm layers from Attempts that were fenced, quarantined, malformed, or released, or whose
  session cleanup did not complete.
- Closing convergence on warm results alone under the default policy.
- A candidate-built cache under a safe pipeline, or under any name that implies administrator
  approval.
- Session resume for Codex until a provider-specific protocol proves adapter-owned working
  directory, sandbox, configuration, output, and model enforcement on fork.

## Sequence

| Package | Delivers | Touches | Removes |
|---|---|---|---|
| P1 Notes + Head Delta | WorkerNotes carried per node; HeadDelta with reverted and removed marks; Warm Set selection; report columns; Task-path ports and compiler wiring | `review-core`, `review-config`, `review-pipeline` (carry beside `prior_findings`), `review-runner` (render and parse beside the flat result, the ADR-0038 pattern), `reviewctl` report | Rediscovery tokens. Pure kernel work, no provider dependency. |
| P2 Build cache carry from the Gate | `BuildCache@1` with its closed capture layout; `cargo_target` kind; typed Gate-to-Worker handoff; trusted-local gate | `review-sandbox` cache, `review-pipeline` Gate to reviewer handoff, policy validation | Build minutes for TDD reviewers and implementers. |
| P3 Warm Workspace | Stable root per node; rebase by tree diff with digest verification; `WorkspaceRebased@1` | `review-sandbox`, `review-source-git` (apply diff), events | Template materialization per Round. Also the precondition for P4, since harness session stores key by working directory. |
| P4 Session Snapshot and Cold Closeout | Kernel-assigned session IDs; two-phase capture; forked resume; delta prompt render mode; age and size gates; compiled Cold Closeout; Claude only | `review-runner-claude`, `review-runner` (resume transport), `review-pipeline`, `review-store`, policy | Prefix re-send and re-reasoning when Rounds are close in time. Default off until measured. |

Each package is one ADR. Every package ships with the cold-versus-warm dogfood comparison on the
standard one-reviewer policy of
[ADR-0027](../adr/0027-use-one-correctness-reviewer-per-milestone.md). The runtime overlap that
an earlier draft listed as a fifth package is withdrawn; see L2.

## Risks and how the design answers them

| Risk | Answer |
|---|---|
| Anchoring | A warm reviewer may rubber-stamp its earlier view. Notes are inspection maps, not verdicts; the per-Finding disposition obligation is unchanged; a compiled Cold Closeout confirms a would-be-clean Round with a cold Attempt. |
| Injection through Notes | Notes are model-authored text fed to a later model, the same class as prior Findings and refused Attempts. Same treatment: labelled data, JSON-encoded, bounded. |
| Stale Notes | Head Delta marks cover every path the Notes reference, including paths reverted to Base or removed, and exist for whole-tree Subjects. |
| Context growth | Notes have a byte bound. Transcripts must fit the reservation with the delta prompt or the layer is dropped. The Change Set is marked, not duplicated. |
| Secrets in transcripts | Grants are already redacted from stored output. Session files are captured into the local CAS and deleted from the operator's harness directory through a two-phase, epoch-checked protocol, which is stricter than today, where Claude session state is left in the user's home. |
| Host data in a build cache | A Build Cache is explicitly unsafe, captured through a closed layout with no-follow traversal and stripped metadata, admitted only under trusted-local policy, and never called a Cache Snapshot. |
| Provider drift | Resume flags are pinned by version fixtures exactly as the existing security flags are. Codex is out until its own flags are proven. |
| False savings | Every warm Attempt reports cache-read tokens and wall time. A layer that does not show savings in dogfood is turned off by policy, not defended. |
| Replay | Warm inputs are artifacts. Replay reproduces the inputs; model output was never deterministic and the design does not change that. |
