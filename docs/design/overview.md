# Afactory — architecture (redesign, v2.1)

**Status:** v2.1 — decisions D1–D17 and D19 resolved; D18 superseded ·
**Values:** [`values.md`](values.md) ·
**Detail:** [`entities.md`](entities.md) · [`state-machines.md`](state-machines.md) ·
[`config.md`](config.md) · [`store.md`](store.md) · [`research.md`](research.md)

**Next increment:** [Task execution](task-execution.md) develops this direction
into shared/generated Pipeline definitions, compiled Execution Plans, local Worker bindings,
and an implementation cutover from the two existing drivers. Read it for the current proposal.
This v2.1 document is architecture direction, not an inventory of shipped 0.8.0 capabilities.

Afactory is a software factory run by coding agents. `af` is the kernel that makes their work
**verifiable**: it hands a Worker the smallest sufficient exact Input and an inspectable Snapshot
in an isolated environment with brokered Tools and a budget, records what happens in a durable
Store, and lets an
independent evaluator — never the worker itself — decide whether the goal was reached. There
is one execution kernel; a review is a Task whose goal is "review this Subject" and whose extra
input is a diff. Every entity is explicit, configurable in TOML, and replaceable; every
lifecycle is a state machine driven from the log.

## 1. What `af` is

Two ways it plugs into a developer's day, one kernel:

- **Guest** — `af` inside Claude Code / Codex. `af connect claude` registers `af` as an MCP
  server and a stop hook; the agent gets tools (`af.review`, `af.findings`, `af.resolve`) and one
  rule in its instructions: *nothing is done until `af` says so.* The review runs as a separate
  principal — a different session and, by default, a different provider — so a PR arrives with
  an independent ledger of findings, evidence, and spend. v1 first provides the same review
  boundary locally; connection plumbing is deliberately later.
- **Host** — `af` drives the agents. `af task start` takes a **Task** — a goal, an acceptance
  contract, authority, constraints, a budget — runs workers in
  environments through providers, verifies the result with gates and an evaluator, and returns
  a verified derived snapshot or an evidence-backed terminal outcome.

`af review …` is sugar over a review-kind Task; the Review Kernel's vocabulary
(Subject, Report, Finding, Demand, Evidence, Convergence …) is the review Task kind's
artifact vocabulary, unchanged in meaning.

## 2. Rules the values impose

1. **Spend tokens for information that changes the outcome.** Every model call has a bounded
   reservation and a named transition or typed artifact it can advance; fan-out and retries need
   distinct hypotheses or new information. Report total tokens per verified outcome.
2. **Give each Worker the smallest sufficient context.** Inputs are exact, typed, role-scoped,
   and measured. The Snapshot is inspectable on demand; parent transcripts, full Ledgers,
   unrelated documents, and other Workers' private reasoning are not injected by default.
3. **One execution kernel; Task kinds on top.** `review` and `implement` are Task kinds: each
   brings a pipeline, artifact types, and reducers — never a second orchestration model.
4. **Six authored, six recorded.** Humans author **Provider, Env, Tool, Worker, Pipeline, Task**
   in TOML; the kernel records **Snapshot, Input, Attempt, Output, Event, Ledger**. Nothing else
   is a top-level concept. Where each lives is a separate axis (§6): git, the machine, or the
   Store.
5. **Git is configuration and versioning.** Declarations colleagues share live in `.af/` and
   are versioned there; git also supplies Snapshots (trees by digest) and delivery targets
   (branch, PR — human-confirmed). Git never coordinates: no state in the tree, no state
   merged through git.
6. **State lives in the Store.** Tasks, snapshots, inputs, attempts, outputs, events, ledgers,
   objects — all of it — in a kernel-level Store with pluggable backends; local scratch under
   XDG directories; nothing machine-written in the repository.
7. **Everything that moves is a state machine.** Task ⊃ Pipeline ⊃ Worker, each with named
   states, typed events, guards over exact views, and effects issued as commands. The next
   state is a pure function of (state, event, views); every transition is an event in the log.
8. **A generator never grades itself.** Verification is executable checks the implementer
   cannot edit, then an independent evaluator, then required Evidence. Silence is never success.
9. **Reserve before spend; fence what you abandon.** Unchanged from the kernel today.
10. **Substitute by name; second implementation for every kind.** Named TOML tables with a
    `kind`; executables on `PATH` for new implementations.
11. **Everything non-deterministic is journaled.** Model calls, tool calls, clocks, network,
   randomness — recorded with their result, replayed, never re-asked. Results are admitted in
   canonical order. Replay reproduces the ledger byte-for-byte.
12. **Unix-native surfaces.** stdout result, stderr progress, exit codes, `--json`, XDG
    directories, signals, plugins on `PATH`; the TUI is a plugin.
13. **No credential ever enters a project, package, artifact, or sandbox.**

## 3. The picture

```
   surfaces                 │                    kernel                          │  outside
  ──────────────────────────┼────────────────────────────────────────────────────┼───────────────
   af  (CLI, --json)        │   ┌──────────── coordinator ─────────────┐         │
   af mcp serve ◄── agents  │   │  Task FSM ⊃ Pipeline FSM ⊃ Worker FSM │         │  Claude Code
   af hook      ◄── hooks   │   │  next = f(state, event, views)        │         │  Codex CLI
   af-tui       (plugin)    │   │  effects out as commands, results in  │         │  model APIs
   CI job                   │   │  as events · canonical order · fenced │         │  (providers)
                            │   └──────┬───────────────────┬───────────┘         │
                            │  authored│                   │recorded             │
                            │  ┌───────▼──────┐   ┌────────▼──────────────────┐  │
                            │  │ Provider     │   │ Snapshot · Input · Attempt│  │
                            │  │ Env · Tool   │   │ Output · Event · Ledger   │  │
                            │  │ Worker       │   ├───────────────────────────┤  │
                            │  │ Pipeline     │   │        S T O R E          │  │
                            │  │ Task         │   │ log · objects · views     │  │
                            │  └──────────────┘   │ sqlite now · shared later │  │
                            │  .af/ (git) · ~/.config/af │ $XDG_STATE_HOME · a service │
```

One attempt, end to end: the Task FSM enters `running` and starts the Pipeline FSM; a
`workers` state opens one Worker FSM per worker; the Worker names a **Provider**, an **Env**,
and **Tools**; the kernel captures a **Snapshot**, writes the exact **Input**, reserves budget,
opens an **Attempt** under a fresh epoch, materializes the Env, starts the Provider, brokers
Tool calls (each an event), seals the Env into an **Output**, and appends **Events** to the
Store; the reducer rebuilds the **Ledger**; guards over the exact final views drive the
Pipeline FSM to a terminal state; the Task FSM records the outcome.

## 4. Entities at a glance

| Entity | One line | Lives in | Substituted by |
|---|---|---|---|
| **Provider** | An authentication context to one backend (`claude-code`, `codex`, `anthropic`, `openai`, `openai-compatible`, `gemini`, `acp`, `command`) behind one trait that emits normalized events | the machine: `~/.config/af/config.toml` (a project may only *require* a name and kind) | `kind`; `af-provider-<name>` |
| **Env** | Where an attempt runs: tier `host` / `process` / `container` / `vm`, base by digest, caches, network, filesystem policy, limits; declared *provides* vs required, checked by `admit` | git: `.af/af.toml` `[env.<name>]` | `isolation`; `af-env-<name>` |
| **Tool** | A brokered, audited capability: `shell` (prefix rules), `command` (typed arg slots), `mcp`, `af` (kernel-native) | git: `.af/af.toml` `[tool.<name>]` | `kind`; any MCP server |
| **Worker** | A role package (`reviewer`, `implementer`, `evaluator`, `planner`): provider, model, instructions, tools, env requirement, budget defaults; runs as a Worker FSM | git: `.af/workers/<name>/`, pinned in `.af/af.lock` | one key; another package |
| **Pipeline** | A declared state machine: states with kinds (`capture`, `gate`, `workers`, `reduce`, `verify`, `decide`), typed wiring, guarded transitions, terminal outcomes | git: `.af/pipelines/<name>.toml` | another file |
| **Task** | A durable contract — goal, acceptance, authority, constraints, budget, strategy — with a `kind` (`review`, `implement`); immutable revisions; runs as the Task FSM | the Store (authored via `af task start` or a TOML file passed to it; never committed) | strategy; evaluator; budget predicates |
| **Snapshot** | Immutable source state with one Tree Digest | the Store (objects) | source adapter |
| **Input** | The exact, content-addressed inputs of one attempt | the Store | — |
| **Attempt** | One execution of one worker under one epoch and one reservation | the Store (events) | — |
| **Output** | Sealed diff, typed results, usage receipt | the Store (objects + events) | — |
| **Event** | One append-only, versioned record; derived id from run + sequence | the Store (log) | — |
| **Ledger** | The projection of a run: Finding/Demand Sets, task state, verdicts, spend | the Store (views) — rebuilt from the log | — |

The **Store** itself is the kernel-level entity that owns the log, the objects, and the views —
"the heart of the whole system" — with one backend for now, `sqlite` (embedded, local), and a
shared, consensus-based distributed coordinator later, its database deliberately undecided. Its internals are a separate design
step ([`store.md`](store.md)); this document only places it. The **worker protocol** (how an
attempt receives its Input and returns its Output) is the other separate step
([`entities.md` → Protocol](entities.md#the-worker-protocol-separate-design-step)).

## 5. Lifecycles — three nested state machines

Specified in [`state-machines.md`](state-machines.md). In one screen:

```
Task      submitted → planning → running ⇄ waiting → verifying → VERIFIED
          │ rejected            │ blocked · needs-human · exhausted · cancelled      │ unverified
          └────────────────────────────────── (verifying fails, budget left → planning)

Pipeline  capture → gate → workers ═══► reduce → verify → decide ─┬─ verified
(review)  │ incomplete   │ blocked  (all regions terminal)         ├─ clean round → capture
                                                                   ├─ blocking → capture
                                                                   └─ max rounds → unverified

Worker    pending → reserved → dispatched(e) → running ⇄ tool_call → sealing → admitted
          │ released         │ fenced (timeout / supersede) → late output quarantined
                             │ failed · malformed (charged)
```

The rule for all three: `next = f(state, event, views)`; effects leave as commands (dispatch,
run check, reduce, fire timer); results come back as events; replay of the events reproduces
the states. A `workers` state is a set of orthogonal regions with a lossless barrier: every
region must reach a terminal Worker state before the transition, and a missing region's
output makes the round `incomplete`, never passed.

## 6. Where things live

```
<repo>/.af/                     # git: the declarations colleagues share — and nothing else
  af.toml                       # project declaration (envs, tools, defaults)
  af.lock                       # af version + worker/pipeline/extension digests (machine-written TOML)
  workers/<name>/worker.toml + worker.md
  pipelines/<name>.toml         # state machines
~/.config/af/config.toml        # the machine: providers by reference, defaults, trust, store connection
~/.local/state/af/              # the Store's embedded backend (sqlite) when no shared Store is configured
~/.cache/af/                    # materialized envs, cache snapshots, large blobs
$XDG_RUNTIME_DIR/af/            # locks, sockets
<store>                         # the Store: tasks, log, objects, views — a sqlite file now; a distributed coordinator later
```

`.af/af.local.toml` is the one personal file (gitignored by `af init`). Tasks are not stored in
the repo: `af task start --file ticket.json` captures the Task file's contract in the Store, and
`af task show <id>` inspects it. Nothing under `.af/` is machine-written except the lock.

## 7. Kernel layout — crate map

`af` is the only name. The `review-*` crates, `.review/`, `review.kernel/*` artifact types,
`review.lock`, and *Campaign* are retired in one breaking migration (D13); the review Task kind
keeps the Review Kernel's artifact vocabulary under the `af/*` namespace.

| Current `review-*` workspace | Proposed | Owns |
|---|---|---|
| review-core | `af-core` | ids, canonical JSON, envelopes, entity types, event vocabulary |
| review-store | `af-store` | the Store interface + `sqlite` backend (a distributed backend later, database undecided); log, objects, views, leases |
| review-source-git | `af-source` | Snapshot capture, Tree Digest, materialize, tree diff, seal |
| review-config | `af-config` | layered TOML, package/pipeline/lock loaders, `toml_edit` rewrites |
| review-sandbox | `af-env` | tiers `host` / `process` / `container` / `vm`, `admit`, Cache Snapshots, bounded probes |
| review-runner, -claude, -codex | `af-provider` | Provider trait, adapters, preflight Operations, usage |
| review-check | `af-tool` | `shell` / `command` / `mcp` / `af` tools, typed arg slots, prefix rules, broker |
| (packages) | `af-worker` | worker packages, lock digests, instruction rendering, the Worker FSM, the worker protocol |
| review-graph + review-attempt + `Kernel` | `af-engine` | the state-machine engine: Task/Pipeline FSM driver, scheduler, budgets, fencing, replay |
| review-pipeline + `authority.rs` | `af-task` | Task kinds (`review`, `implement`): pipelines, reducers, Findings/Demands, convergence, verdicts |
| af | `af` | clap CLI (`--json` everywhere), `af mcp serve`, `af hook`, `af connect`; the TUI moves to `af-tui`, a plugin |

Eleven crates plus the plugin. The kernel's ADRs are re-homed under the new names where
they still hold (most do: base-pinned authority, path-independent identity, silence is not a
drop, fixed requires verification, handles not secrets, sandbox-local caches).

## 8. Surfaces

**CLI contract**: stdout is the result, stderr is progress, `--json` on every read, exit codes
`0` pass · `1` error · `2` usage · `3` fail · `4` incomplete, `--help` under 50 ms, `-c key=value`
overrides, XDG directories, signals handled. Namespaces: `af init` · `af doctor` · `af provider
add|status|login` · `af env check` · `af connect claude|codex` · `af review [--uncommitted]
[--diff BASE..HEAD]` (sugar) · `af task start|status|history|resume|cancel|report|show` ·
`af attempt show` · `af mcp serve` · `af hook <event>` · `af store status|migrate` ·
`af config show --origin` · `af lock`.

**MCP server** (`af mcp serve`, stdio, `rmcp`, stateless, deterministic `tools/list`): the
kernel's tools for external agents — `af.review`, `af.findings`, `af.resolve`, later
`af.task.*`. Whether workers *inside* an attempt also talk to the kernel over MCP is the
protocol design step's call (D6); until then the current prompt-in / JSON-out contract stays.

**Hooks**: `af hook stop` runs the review pipeline on the uncommitted change and returns
blocking findings (exit 2 → the agent keeps working). `af connect` writes the MCP entry, the
hook, and an `AGENTS.md` rule — nothing else.

### First five minutes

```sh
brew install af            # or the release install script
cd myrepo && af init       # .af/af.toml, two review workers, the review pipeline; embedded Store
af provider setup claude-main --kind claude  # login + explicit machine-local binding
af connect claude          # MCP server + stop hook + AGENTS.md rule
af doctor                  # every entity green, or the exact next action
```

## 9. Decisions

D1–D11 were resolved first, D12–D16 in the v2 draft, and D17–D18 as implementation
priorities. D19 then superseded D18 and narrowed the release cut without changing the entity
model.

| # | Decision | Resolution |
|---|---|---|
| D1 | Kernel vs orchestration | **One execution kernel.** Review is a Task kind: goal "review this Subject", extra input a diff or PR. No separate capability layer. |
| D2 | Name of the contract | **Task.** |
| D3 | Entity model | **Six authored, six recorded**, plus the Store as the kernel-level home of state. |
| D4 | Project config home | **`.af/af.toml`**; `.review/` removed. |
| D5 | The log / ledger | **Separate design step** — the Store: interface placed here, internals designed next ([`store.md`](store.md)). |
| D6 | Worker result channel | **Separate design step** — the worker protocol; the existing prompt-in / JSON-out contract stays until then. |
| D7 | Providers | One trait over harness CLIs and APIs, normalized events, cost tagged by source — explained in [`entities.md` → Provider](entities.md#provider). |
| D8 | Environments | Tiers with `admit` wired — what "wired" means is in [`entities.md` → Env](entities.md#env). v1 ships the `host` tier with the check in place. |
| D9 | Verification | Read-only oracles; gates before an independent evaluator; attestations per gate; verification reserve. |
| D10 | Extension model | Executables on `PATH` + MCP; no WASM in v1. |
| D11 | Phasing | **`af` as soon as possible** — §10 shrinks v1 to final local review on one machine. |
| D12 | Store backend | **`sqlite` only, for now**: embedded under XDG state, zero install. The interface stays backend-agnostic; which database backs the shared, consensus-based coordinator later is deliberately undecided. |
| D13 | Breaking migration | Revised by D19. `af` and `.af/` are the only user-facing names in v1; consumers migrate when they bump the pinned release. Frozen `.review/`, `review.kernel/*`, and `review-*` contracts may remain internal through v2. Their physical migration and harness retirement move to v3 under a separate migration decision. |
| D14 | Lifecycles | Task, Pipeline, and Worker are state machines; pipelines are authored as state machines ([`state-machines.md`](state-machines.md)). |
| D15 | Git's roles | Configuration and versioning of `.af/`; source of Snapshots; delivery target. Never coordination. |
| D16 | State location | Never in the repository — the Store, XDG state/cache/runtime dirs. `af init` writes no `.gitignore` beyond `.af/af.local.toml`. |
| D17 | Value priority | **Wise token consumption first; minimum Worker context second; deterministic pre-last.** Model work is budgeted by expected information gain, Worker Inputs are exact and role-scoped, and existing deterministic contracts remain binding until explicitly superseded ([ADR-0028](../adr/0028-prioritize-wise-token-use-and-minimum-worker-context.md)). |
| D19 | Minimal release cut | **v1 local review → v2 sequential implement → candidate dogfood.** v2 ends at a verified internal Snapshot; delivery, scale, optional integrations, and physical internal renaming are v3. Recorded in ADR-0030, which GA deletes as spent. |

Open questions answered: `.af/af.toml`; no state tracked in git; the TUI is a plugin; crate
re-layout as needed.

## 10. Implementation order — v1, v2, then dogfood

"`af` as soon as possible" means the shortest complete local loop, not the largest plausible
platform. The internal bootstrap does not relax the pinned-release rule for consuming projects.

### v1 — final local review

v1 contains only what a local non-interactive review needs:

- `af review` over an exact committed or uncommitted Subject, with a structured typed outcome;
- `.af/af.toml` declarations and `.af/af.lock`, with state outside the repository;
- the local Store boundary with embedded SQLite and sequential Task/Pipeline/Worker execution;
- one admitted `host` Env, command Tools, and the existing prompt-in/JSON-out Worker protocol
  behind the final Worker boundary;
- exact least-sufficient Input manifests, bounded reservations, and Provider token receipts;
- the existing Review Kernel behavior and frozen persisted contracts reused internally.

v1 does not require `init`, `doctor`, `connect`, hooks, MCP, TUI, direct API Providers, richer
Envs, parallel execution, delivery, or physical crate/contract renaming.

### v2 — sequential implementation with internal verification

v2 adds one `implement` Task kind. One implementer Worker receives the goal, acceptance contract,
and exact Snapshot; its candidate becomes an internal derived Snapshot. Read-only acceptance Gates
run first. A separate evaluator Worker then sees the goal, acceptance evidence, candidate diff,
and required authority — never the implementer's private reasoning — and returns a typed verdict.

The terminal result is `verified` only when every required Gate attests success, the evaluator
accepts, the derived Snapshot is sealed, and token accounting is complete. Otherwise it is typed
`unverified` with evidence. Execution is sequential. v2 does not write the Snapshot into a working
tree or branch and does not create a PR.

### First candidate dogfood

After v2, the kernel repository uses `af` to implement one real Afactory change. Exit requires the
candidate identity, exact Input/context, token spend, acceptance attestations, independent
evaluation, sealed Snapshot, and final typed outcome, beside a green `make check`. Pinned
`v0.2.0` remains available as the independent last-green reviewer during bootstrap.

### v3 — only after dogfood evidence

v3 owns working-tree/branch/PR delivery, dynamic fan-out and parallel work graphs, a
shared/distributed Store, TUI, MCP/hooks/`af connect`, direct API Providers, process/container/VM
Envs, hosted integrations, and physical migration of frozen internal names and contracts. The
remaining milestones resume in dependency order after the first dogfood and may pull an item
forward only when v1 or v2 cannot meet its stated exit criteria without it.

## 11. What does not change

The pinned-release rule for consumers ([ADR-0044](../adr/0044-af-manages-itself-and-dispatches-to-the-pinned-release.md),
[ADR-0045](../adr/0045-one-release-train-and-a-pin-that-binds-bytes.md)),
kernel docs living in the kernel repo, the meaning of the Review Kernel's terms
([`CONTEXT.md`](../../CONTEXT.md)), and the standing rules: never weaken a contract,
fixture, gate, budget, or sandbox boundary to pass; no credential in any artifact; publishing
is a human action.
