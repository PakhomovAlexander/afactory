# Afactory — the state machines

**Status:** v2 · part of [`overview.md`](overview.md). Everything that moves in
`af` is one of three nested state machines: a **Task** runs a **Pipeline**; a Pipeline's
`workers` state runs one **Worker** machine per worker. All three obey one rule.

## 1. The rule

```
next(state, event, views) -> (state', commands[])
```

- `state` is the machine's current named state plus its recorded context (ids, counters).
- `event` is one record from the Store's log — an external fact (attempt admitted, check
  completed, timer fired, human answered) or an internal transition.
- `views` are exact, content-addressed projections named by id (a Finding Set, a Demand Set,
  a gate decision), never "the latest".
- `commands` are effects the engine performs *after* the transition is recorded: dispatch an
  attempt, run a check, reduce outputs, arm a timer, ask a human. Every command's result comes
  back as an event. The machine never performs an effect itself.

Consequences: the function is pure and total (every (state, event) pair has a defined
result — unknown events are recorded and ignored, not crashed on); replaying the log
reproduces every state; a crashed engine resumes by replay and re-issues only commands whose
result event is missing (idempotent by attempt id and epoch); two engines cannot both drive
one run because the Store hands out a single lease per run and fences the loser.

Time enters only as events: a deadline is `TimerArmed{at}` → `TimerFired{id}` recorded by the
engine, so replay sees the same firing. Randomness enters only as recorded values.

## 2. Task

```
                    ┌──────────────── revise (new revision) ────────────────┐
                    ▼                                                       │
 [*] → submitted ─► planning ─► running ─► verifying ─► VERIFIED            │
         │            ▲           │  ▲         │                            │
         ▼            │           ▼  │         ├─ fail, budget left ─► planning
      REJECTED        └───────── waiting       └─ fail, no strategy ─► UNVERIFIED
                                  │
 running ─► BLOCKED · NEEDS-HUMAN · EXHAUSTED · CANCELLED     (typed terminal outcomes)
```

| State | Meaning | Leaves on |
|---|---|---|
| `submitted` | contract recorded and validated | `admitted` → `planning`; `refused` → **rejected** |
| `planning` | strategy chosen (pipeline + worker bindings), budget allocated, plan recorded | `planned` → `running` |
| `running` | the Pipeline FSM is executing a round | pipeline terminal event → `verifying`; `question` → `waiting`; `dependency_missing` → **blocked**; budget predicate → **exhausted**; `cancel` → **cancelled** |
| `waiting` | a human answer, approval, or credential is pending; wall clock still runs | `answered` → `running` (or `revised` → `planning`); `deadline` → **needs-human** |
| `verifying` | gates and the evaluator judge the exact result Snapshot | `pass` → **verified**; `fail` with budget and strategy left → `planning` (verdict recorded as feedback); otherwise → **unverified** |

Terminal outcomes are states with a full record: revision, Subject and result Snapshots,
checks, Evidence, verdicts, spend, attempts. `af task resume` starts a new round from a
terminal-but-resumable state (`blocked`, `needs-human`, `exhausted`) — after the human action,
a new revision, or more budget — never by editing history.

For the **review** kind the Task FSM is thin: `planning` picks the `review` pipeline and the
workers; `verifying` is the pipeline's own convergence verdict; outcomes are `verified`
(converged), `unverified` (max rounds without convergence), `exhausted`, `blocked` (gate cannot
run), `cancelled`.

## 3. Pipeline

A pipeline is a declared state machine over a round. State kinds are fixed by the kernel;
the graph is the project's.

| Kind | Does | Emits |
|---|---|---|
| `capture` | resolve authority, capture the Subject Snapshot, derive the Change Set for a diff | `done{subject, change_set}` · `failed` |
| `gate` | run named `command` checks in a read-only materialization | `passed{decision}` · `failed{decision}` · `error` |
| `workers` | open one Worker FSM per listed worker with typed inputs; **lossless barrier** | `all_done{outputs}` when every region is terminal · `exhausted` · `incomplete` |
| `reduce` | fold Outputs into the Ledger with the kind's reducer (Findings/Demands; verdicts) | `done{views}` |
| `verify` | run acceptance checks and the evaluator on the result Snapshot | `pass{attestations}` · `fail{attestations}` |
| `decide` | evaluate named guards over the exact final views | one guard name |
| terminal | `verified` · `unverified` · `blocked` · `exhausted` · `incomplete` | — |

```toml
# .af/pipelines/review.toml
name = "review"
version = 1
initial = "capture"
requires = "host"                          # minimum Env isolation for every workers state

[state.capture]
kind = "capture"
subject = "diff"                           # diff | whole-tree
on = { done = "gate", failed = "incomplete" }

[state.gate]
kind = "gate"
checks = ["lint", "verify"]                # names from [tool.*] of kind command
on = { passed = "review", failed = "blocked", error = "incomplete" }

[state.review]
kind = "workers"
workers = ["architecture", "performance"]  # orthogonal regions, one Worker FSM each
inputs = { prior_findings = "ledger.findings", change_set = "capture.change_set", gate = "gate.decision" }
on = { all_done = "reduce", exhausted = "exhausted", incomplete = "incomplete" }

[state.reduce]
kind = "reduce"
reducer = "findings"                       # the review kind's reducer
on = { done = "decide" }

[state.decide]
kind = "decide"
guards = ["converged", "max_rounds", "blocking", "clean"]   # evaluated in this order; first true wins
on = { converged = "verified", max_rounds = "unverified", blocking = "capture", clean = "capture" }

[budget]
tokens = 2_000_000
attempt = 300_000

[convergence]                              # parameters the `decide` guards read
clean_rounds = 2
max_rounds = 4
gate = "major"
```

Load-time validation (the old planner, kept): every `on` target exists; every `inputs` value
names an output of an earlier state or a ledger view of the right type; a `workers` state
names workers whose `kind` fits and whose `requires` is satisfiable by the configured Env;
every path reaches a terminal state; guards are known to the Task kind. A pipeline that fails
validation never starts.

Wiring types are the artifact types (`af/FindingSet@1`, `af/ChangeSet@1`, `af/GateDecision@1`,
…) — the typed ports survive, now as state inputs and outputs.

**Rounds.** A pipeline runs one round from `initial` to a terminal state. `decide → capture`
starts the next round in the same Task with the head re-captured; the Task FSM counts rounds
and applies the budget predicates between them. An `incomplete` terminal keeps the round's
exact inputs for resume (never consumes a clean/cap slot — unchanged).

**Determinism at the barrier.** Regions finish in any order; `all_done` is emitted only when
every region is terminal, and Outputs are admitted to `reduce` in canonical worker order —
the existing determinism test, now a property of the `workers` state.

## 4. Worker

One machine per attempt, generic across worker kinds; instructions differ, the machine does
not.

```
 [*] → pending ─► reserved ─► dispatched(e) ─► running ⇄ tool_call ─► sealing ─► ADMITTED
                    │             │               │                      │
                    ▼             │               ├─► FAILED · MALFORMED │ (charged)
                 RELEASED         │               │                      │
              (refused /          └── timeout / supersede ──► FENCED ◄───┘  late output → QUARANTINED
               unavailable)
```

| State | Meaning | Leaves on |
|---|---|---|
| `pending` | selected by the pipeline, Input written | `reserve` → `reserved` |
| `reserved` | budget reserved across nested scopes (attempt, node, fan-out, run); Env admitted (`admit(required, provided)`) | `dispatch` → `dispatched`; `refused` / `unavailable` → **released** (reservation returned) |
| `dispatched` | epoch `e` issued; Env materialized; Provider session started | `started` → `running` |
| `running` | the model session is working | `tool_call` → `tool_call`; `result` → `sealing`; `error` → **failed**; `unparseable` → **malformed**; `timeout` / `supersede` → **fenced** |
| `tool_call` | a brokered tool is executing under epoch `e`; the call and its result are events | `tool_result` → `running`; stale epoch → the call is **quarantined**, machine unchanged |
| `sealing` | the Env is rescanned; the diff is derived; Output typed and stored raw-first | `sealed` → **admitted** (at the barrier, canonical order); `stale_epoch` → **quarantined** |

Terminal: `admitted`, `released`, `failed`, `malformed`, `fenced` (with `quarantined` as the
disposition of anything that arrives late). Charging: `admitted`, `failed`, `malformed`, and
`fenced` charge actual or reserved spend; `released` returns the reservation. A retry is a
new attempt with a new epoch, never a re-entry.

The Worker FSM is where the **worker protocol** (D6, separate step) plugs in: `running`,
`tool_call`, and `sealing` are the states whose events the protocol produces. Whatever
transport wins — today's prompt-in / JSON-out, MCP tools, or JSON-RPC over stdio — the
machine and its events do not change.

## 5. Events (the vocabulary the machines consume)

Task: `TaskSubmitted@1` · `TaskAdmitted@1` · `TaskRejected@1` · `PlanRecorded@1` ·
`TaskWaiting@1` · `TaskAnswered@1` · `TaskRevised@1` · `VerdictRecorded@1` · `TaskClosed@1`.
Pipeline: `RoundStarted@1` · `StateEntered@1` · `SourceCaptured@1` · `CheckCompleted@1` ·
`GateDecided@1` · `GenerationAdvanced@1` · `RoundClosed@1`.
Worker: `AttemptReserved@1` · `AttemptDispatched@1` · `ToolCalled@1` · `ToolReturned@1` ·
`AttemptSealed@1` · `AttemptAdmitted@1` · `AttemptReleased@1` · `AttemptFailed@1` ·
`AttemptFenced@1` · `OutputQuarantined@1`.
Engine: `TimerArmed@1` · `TimerFired@1` · `LeaseAcquired@1` · `LeaseFenced@1` · `Compacted@1`.

Names are proposals for the Store design step; the shape rule holds regardless: a payload
change bumps the version, the vocabulary is a closed enum mirrored in `schemas/`, every event
carries run id, sequence, derived id, machine, state, and the ids of the artifacts it
references.

## 6. Why state machines

- **Clear:** a pipeline reads as states and transitions, the way the run report reads.
- **Deterministic:** pure transitions over recorded events are replayable by construction;
  the old code's `Debug`-string convergence check and arrival-order hazards cannot exist.
- **Extendable:** a new state kind or guard is a registration in a Task kind, not a branch in
  a `match` in the engine; a new worker protocol changes events, not machines.
- **Simple:** one rule for three machines; the engine is one loop — read event, transition,
  record, issue commands.
