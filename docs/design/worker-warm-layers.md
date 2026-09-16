# Worker warm layers

**Status:** design proposal, 2026-09-16, against `main` 166eca5 (v0.9.0-rc.2). No ADR yet.
Companion vocabulary lives in [`entities.md`](entities.md) and
[`state-machines.md`](state-machines.md); the binding record is [`../adr/`](../adr/).

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
| Harness boot | Seconds | Attempt | One `claude -p` or `codex exec` process per Attempt, spawned after the Gate passes. |
| Tree | Seconds to tens of seconds for a large tree | Round for the template, plus one clone per Attempt | The template is built from scratch per Round in a fresh temporary directory. |
| Build caches | Minutes for anything that compiles or tests | Attempt | No Cache Snapshot reaches a Worker sandbox. The Gate builds the same head minutes earlier and its output is discarded. |
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
keeping something running in between.

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
L3 Workspace    Tree Digest + CacheManifest digest  Campaign or Task,    tree materialization,
                + mode                              re-based per head    dependency and build caches
L2 Runtime      provider kind + executable path     machine              harness boot, by overlap
                + auth directory + recheck receipt
L1 Binding      package digest + principal          Campaign or Task     admission probe (today)
                + admission receipt
L0 Definition   package content digest              catalog pin          none
------------------------------------------------------------------------------------------------
A Cold Attempt uses L0 and L1 only. A Warm Attempt adds any of L2 to L5, each recorded.
```

### L4 Notes: the layer that pays for the rest

At the end of an Attempt the Worker may emit one bounded, typed artifact beside its result: which
paths it inspected, its model of the change, open questions, and per-path hints. It is an
inspection map, not a verdict. Verdicts already live in the Ledger as Findings. On the next Round
the kernel delivers the previous admitted Attempt's Notes to the same node, rendered under a
"data, not instructions" heading exactly as prior Findings and refused Attempts are rendered
today.

Paired with Notes, the Change Set rendering gains **Delta Marking**: each path is marked changed,
unchanged, or new since the previous Round's head. The delta is the same typed tree diff the
kernel already computes, applied between the two heads. It costs a few bytes per path and tells
the Worker where its Notes may be stale.

### L3 Workspace: a stable root and a carried build

Each node in a Campaign gets a stable workspace root instead of a fresh temporary directory per
Round. When the head advances, the template is **re-based**: a copy-on-write clone of the previous
template receives the tree diff, and the result's manifest digest must equal the new head's Tree
Digest or the kernel falls back to a full materialization. Per-Attempt sandboxes remain fresh
clones of that template, so sibling isolation is unchanged.

The Gate has already built the head before any reviewer is dispatched. Its cache directories
become a Round-scoped **Cache Snapshot**, bounded and credential-free like the existing Cargo
registry snapshot, and are cloned into Worker sandboxes that declare them. A closed
`cargo_target` cache kind points `CARGO_TARGET_DIR` at the clone. This is admitted only under the
trusted-local policy, because a build produced by candidate code is not an administrator-approved
cache.

### L5 Session: opt-in, forked, measured

Both pinned CLIs can resume. Claude 2.1.273 accepts `--session-id`, `--resume`, and
`--fork-session` in print mode. Codex 0.154.0 has `codex exec resume <id>`, blocked today only by
the kernel's own `--ephemeral` flag. The kernel assigns each Attempt a session ID derived from its
Attempt ID, captures the session transcript at seal time into CAS as a **Session Snapshot**, and
deletes it from the operator's harness directory. On the next Round it re-materializes the
transcript, resumes with a fork so the captured transcript is never mutated, and sends a shorter
delta prompt.

This layer is gated three ways: the provider must support resume, the previous Attempt must be
younger than a policy age so a prompt cache can plausibly still serve it, and the transcript's
estimated tokens must fit the reservation alongside the delta prompt. When any gate fails the
Attempt runs on Notes alone. The saving here is only real when cache-read tokens show it. The
design ships this layer behind a policy default of off until a dogfood baseline shows cache reads
on resumed Attempts.

### L2 Runtime: overlap, not reuse

A harness process cannot host two independent conversations, so there is no pool to share. What
can be done is to spawn the harness while the Gate is still running and hold its stdin until the
prompt is ready. Boot cost overlaps Gate wall time. The identity recheck of
[ADR-0090](../adr/0090-recheck-native-task-provider-identity-before-private-invocation.md) runs
before the prompt is sent, as today. Small, safe, and last in the sequence.

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
  |-- cache dirs -> CacheSnapshot         ---->     CoW clone into sandbox  (P2)
template(head_N) at stable root           ---->     rebase to head_N+1, verify digest (P3)
change_set(Base -> head_N)                        change_set(Base -> head_N+1)
                                                  + delta(head_N -> head_N+1) as marks (P1)

WarmSet@1 { node, round: N+1, source_attempt: A_N,
            notes: id | none, session: id | none, workspace: rebased | full,
            caches: [kinds] }                      recorded before reservation, in the manifest
```

Selection is strict. Only an **admitted** Attempt of the same node in the previous **closed**
Round can be the source. Fenced and quarantined Attempts carry nothing. An incomplete Round
resumes its already pinned inputs, including its Warm Set. A retry within a Round is a new
Attempt with a new epoch and inherits the Round's Warm Set, not the failed sibling's state.

## Decisions

- **D1, warmth is declared.** Every warm layer is a CAS artifact recorded in the context manifest
  with bytes and estimated tokens. No layer is ambient. Replay reconstructs the same inputs.
- **D2, same node only.** Warm layers are keyed by Campaign or Task plus node. A node never
  receives another node's Notes, session, or sandbox state. Slots declared independent are
  checked by the compiler. This keeps other Workers' private reasoning absent by construction.
- **D3, Notes are advisory.** Notes never replace the explicit Report, Dispute, or Drop owed for
  every prior Finding. Silence is still not a Drop. Notes are rendered as data, bounded by policy,
  and dropped with a recorded reason when over the bound; the Attempt is still admitted.
- **D4, admitted sources only.** A Warm Set is built only from the previous closed Round's
  admitted Attempt of the same node. Fenced, quarantined, malformed, and released Attempts
  contribute nothing.
- **D5, sessions are forked and capped.** Session Snapshots are captured to CAS, resumed with a
  fork, aged out by policy, and refused when their estimated tokens do not fit the reservation.
  Codex keeps `--ephemeral` unless this layer is enabled, in which case its session files live
  only in the kernel-granted home and are captured and deleted after the Attempt.
- **D6, cold closeout.** The Round that closes convergence runs at least one required reviewer
  cold. Warm Rounds find News cheaply; a cold Round confirms it. The default is one required
  reviewer; policy may set all or none.
- **D7, candidate-built caches are trusted-local only.** A cache produced by building candidate
  code is not an administrator-approved cache. Safe pipelines refuse it and keep the existing
  registry-only snapshot.
- **D8, every warm Attempt reports.** The run report shows, per Attempt, which layers were used,
  rendered bytes, input tokens, cache-read tokens, and wall time. Any change to warm policy ships
  with a representative dogfood baseline or states why none exists, as ADR-0028 already requires.

## Contracts

### Vocabulary, in CONTEXT.md form

```text
**Warm Set**:
The exact, per-node set of carried layers one Attempt starts from: Notes, Session Snapshot,
workspace basis, and cache kinds, recorded before reservation and listed in the manifest.
_Avoid_: "warm cache" or "the previous session" as ambient state; a Warm Set is an artifact.

**Worker Notes**:
A bounded, typed inspection map one admitted Attempt leaves for the next Attempt of the same
node: paths inspected, model of the change, open questions, per-path hints. Data, not a verdict.
_Avoid_: treating Notes as a disposition; every prior Finding still needs its explicit
Report, Dispute, or Drop.

**Session Snapshot**:
The content-addressed transcript of one Attempt's harness session, captured at seal and
re-materialized for a forked resume. Never mutated in place, never shared across nodes.
_Avoid_: "resume the session" as a verb on a live process; the process ended with its Attempt.

**Delta Marking**:
Per-path marks on a rendered Change Set stating whether the path changed since the previous
Round's head, derived from the typed tree diff between the two heads.
_Avoid_: a second full patch in the prompt; the marks exist to keep context minimal.

**Cold Closeout**:
The policy that the Round closing convergence runs at least one required reviewer without
warm layers.
_Avoid_: reading a warm clean Round as convergence on its own.
```

### Artifacts and events

```text
review.kernel/WorkerNotes@1        af/WorkerNotes@1        one per admitted Attempt, optional
review.kernel/SessionSnapshot@1    af/SessionSnapshot@1    one per admitted Attempt, optional
review.kernel/WarmSet@1            af/WarmSet@1            one per node per Round, before reserve
review.kernel/ChangeSet@1 (delta)  unchanged type          between previous head and current head

WarmSetSelected@1         node, round, source attempt, layer ids     before AttemptReserved
WorkspaceRebased@1        from snapshot, to snapshot, verified digest, entries touched
SessionSnapshotCaptured@1 attempt, session id, bytes, estimated tokens, deleted from host: yes

WorkerNotes@1 payload
{ schema, node, attempt_id, head_snapshot_id,
  inspected: [{ path, tree_entry_digest }],
  model_of_change: string,            bounded
  open_questions: [string],           bounded
  hints: [{ path, note }] }           bounded; policy notes.max_bytes, default 16 KiB
```

### Pipeline and package syntax

```toml
# .af/pipelines/review.toml  (reviewer node)
[[nodes]]
id = "correctness"
kind = "reviewer"
package = "correctness"
warm = { notes = true, workspace = "rebase", caches = ["cargo_target"], session = "off" }
# session: "off" | "if_recent" (policy warm.session.max_age) | "always"

[convergence]
clean_rounds = 1
max_rounds = 2
gate = "major"
cold_closeout = "one_required"      # "one_required" | "all" | "none"
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

- Sharing any warm layer across nodes, slots, or Workers, including implementer to evaluator.
- A process, sandbox, or harness home that survives an Attempt with undeclared state.
- A raw transcript as the default carry. Notes are the default; a transcript is an opt-in
  transport optimization that must prove itself in cache-read tokens.
- Warm layers from Attempts that were fenced, quarantined, malformed, or released.
- Closing convergence on warm Rounds alone under the default policy.
- Candidate-built caches under a safe pipeline.

## Sequence

| Package | Delivers | Touches | Removes |
|---|---|---|---|
| P1 Notes + Delta Marking | WorkerNotes carried per node; delta-marked Change Set; report columns; Task-path ports and compiler wiring | `review-core`, `review-config`, `review-pipeline` (carry beside `prior_findings`), `review-runner` (render and parse beside the flat result, the ADR-0038 pattern), `reviewctl` report | Rediscovery tokens. Pure kernel work, no provider dependency. |
| P2 Cache carry from the Gate | Round-scoped Cache Snapshot of Gate cache directories; `cargo_target` kind; trusted-local gate | `review-sandbox` cache, `review-pipeline` Gate to reviewer handoff, policy validation | Build minutes for TDD reviewers and implementers. |
| P3 Warm Workspace | Stable root per node; rebase by tree diff with digest verification; `WorkspaceRebased@1` | `review-sandbox`, `review-source-git` (apply diff), events | Template materialization per Round. Also the precondition for P4, since harness session stores key by working directory. |
| P4 Session Snapshot | Kernel-assigned session IDs; capture to CAS and delete from host; forked resume; delta prompt render mode; age and size gates; Claude first, Codex behind a kernel-owned home | `review-runner-claude`, `review-runner-codex`, `review-runner` (resume transport), policy | Prefix re-send and re-reasoning when Rounds are close in time. Default off until measured. |
| P5 Runtime overlap | Spawn the harness during the Gate with stdin held | `review-process`, `review-runner` | Harness boot seconds. Smallest win, lowest risk, last. |

Each package is one ADR: P1 as the carry contract and Notes, P2 as cache carry, P3 as workspace
rebase, P4 as session capture and resume. P5 needs no ADR. Every package ships with the
cold-versus-warm dogfood comparison on the standard one-reviewer policy of
[ADR-0027](../adr/0027-use-one-correctness-reviewer-per-milestone.md).

## Risks and how the design answers them

| Risk | Answer |
|---|---|
| Anchoring | A warm reviewer may rubber-stamp its earlier view. Notes are inspection maps, not verdicts; the per-Finding disposition obligation is unchanged; Cold Closeout confirms convergence with a cold Attempt. |
| Injection through Notes | Notes are model-authored text fed to a later model, the same class as prior Findings and refused Attempts. Same treatment: labelled data, JSON-encoded, bounded. |
| Context growth | Notes have a byte bound. Transcripts must fit the reservation with the delta prompt or the layer is dropped. The Change Set is marked, not duplicated. |
| Secrets in transcripts | Grants are already redacted from stored output. Session files are captured into the local CAS and deleted from the operator's harness directory, which is stricter than today, where Claude session state is left in the user's home. |
| Cache poisoning | A candidate-built cache is admitted only under trusted-local policy, matching the existing non-goal of reviewing untrusted code with this tool. |
| Provider drift | Resume flags are pinned by version fixtures exactly as the existing security flags are. |
| False savings | Every warm Attempt reports cache-read tokens and wall time. A layer that does not show savings in dogfood is turned off by policy, not defended. |
| Replay | Warm inputs are artifacts. Replay reproduces the inputs; model output was never deterministic and the design does not change that. |
