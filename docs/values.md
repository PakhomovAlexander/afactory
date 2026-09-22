# Afactory — engineering values

**Status:** binding · **Applies to:** every piece of Afactory — the kernel, `af`, and every
design or review of them, including the projects that pin a release of it.

Eight engineering values, in priority order. When two collide, the earlier one wins — and the collision is
worth a line in the ADR or PR that resolved it. Each value comes with the test that decides
whether a change honours it; a change that cannot answer its tests is not done.

The order and the first two values are recorded in
[ADR-0028](adr/0028-prioritize-wise-token-use-and-minimum-worker-context.md).
Sharability, batteries included, and pluggability were added as product values with the
[Task execution increment](task-execution.md) under one explicit rule: **keep the existing
priority order and add these values**. All eight engineering values retain their order, led by
token economy and minimum Worker context. The three product values supplement that order; their
tests apply alongside the engineering values without overriding the existing priorities.

## 1. Wise token consumption

Spend model tokens only when the expected information gain can change a recorded decision or
produce a required artifact. Optimize total tokens per verified outcome, not the apparent cost
of one call: prefer local deterministic computation and exact retrieval, do not ask two Workers
the same question without a distinct hypothesis, and retry only when the next Attempt receives
new information. Reserve a bounded token budget before dispatch and preserve enough verification
reserve to judge the work. Cheap but inconclusive work is waste, not efficiency.

- Every Attempt records input, output, cache, and reasoning tokens where the Provider exposes
  them; every Task report shows total spend and the budget that bounded it.
- Every model call names the state transition or typed artifact it can advance; fan-out and retry
  counts are bounded before dispatch.
- A change to prompts, Worker fan-out, retry policy, or context reports a representative measured
  baseline and the new token total, or says explicitly why no comparison can be run yet.
- Content already present by digest is referenced or retrieved, never copied into another prompt
  merely for convenience.

## 2. As little information in Worker context as possible

A Worker receives the smallest sufficient, role-specific, exact Input for the Output it must
produce. The model context contains typed artifacts and instructions wired to that Attempt — not
the parent session, the whole Ledger, orchestration history, unrelated documents, or a repository
dump. The immutable Snapshot may be inspected on demand through allowed Tools; inspectability is
not a reason to inject its contents into the prompt. Any context expansion is explicit, bounded,
and journaled.

- Every Attempt has an Input manifest that names each injected artifact, its type, and the port or
  rule that requires it; no ambient context is available.
- Prompt fixtures record byte and estimated-token sizes and prove that unrelated files, parent
  transcripts, and other Workers' private reasoning are absent.
- Implementers see the work contract and exact evidence they need; evaluators see acceptance,
  the result diff, and gate attestations — not the implementer's chain of thought or full
  transcript.
- When a Worker needs more, it retrieves one named object or bounded slice through a Tool; the
  retrieval and result become events rather than a permanent default-prompt expansion.

## 3. Performance

The kernel's overhead must vanish next to the model calls it schedules. Rust, no daemon in the
kernel itself, no JIT, no ambient polling. Snapshots are captured with git plumbing and
copy-on-write clones, not file copies; projections are incremental and cached, never recomputed
on every read; attempts run concurrently and are admitted deterministically.

- `af --version` and `af <anything> --help` answer in under 50 ms.
- Reading state (`status`, `ledger`, `report`) stays under 200 ms on a 10k-event run.
- Every hot path has a number attached to it in the PR that touched it.

## 4. Fast

Time-to-value for a developer: install `af`, connect a provider, connect it to Claude or
Codex, and see a task or PR get better — in minutes, with defaults that are right for most
repos. A single static binary, a pinned release, `af catalog init` and `af provider doctor`
that say exactly what is missing. Afactory itself is usable for its own development.

- First useful result within three commands of installation.
- The kernel repo implements its own real changes with `af`.
- Every failure names its next action; nothing fails with a bare exit code.
- Nothing requires a service, an account, or a config file that the default path does not
  create.

## 5. Simple and clear

Few entities, each with one job and one home; one way to do each thing; explicit over
implicit. Every input is named, every output is typed and versioned, every decision is
recorded as an event with its reason, every refusal names the policy or budget that refused.
No ambient state: a state machine consumes exactly the events and views wired to it. Humans
write TOML; machines write records; the two never blur. Removing an entity is preferred to
adding a knob.

- The whole entity model fits on one page; each entity fits on one screen.
- A repo with an empty `.af/af.toml` still works with defaults.
- Every persisted record carries a `kind@version` and is reproducible from its inputs.
- An error message states the entity, the knob, and the fix.
- A newcomer can trace one task end-to-end from the docs without reading the code; a new
  concept must retire, merge with, or clearly separate from an existing one before it is added.

## 6. Extendable

Every entity is an interface with at least two implementations — the built-in one and a
substitute — and can be swapped by name in TOML. Adding a provider, environment, tool, worker,
or store backend needs no kernel change: executables on `PATH` speaking a versioned JSON
contract (`af-<kind>-<name>`, the `git-*`/`cargo-*` convention) and MCP for tools.

- The second-implementation rule: no entity ships with a single implementation baked in.
- A substitute is chosen by editing one TOML key, never by editing code.
- Extension contracts are versioned like every other artifact.

## 7. Deterministic

Same inputs, same outputs. The coordinator and every state machine in it
are pure functions of (state, event, exact views); everything non-deterministic — model calls,
tool calls, clocks, network, randomness — is journaled with its result and replayed from the
log, never re-asked. Results are admitted in canonical order, never arrival order; identities
are derived from content or from run + sequence, never from wall-clock time or a counter that
two writers could share. The earlier values decide genuine design trade-offs; an already accepted
deterministic contract remains binding until an explicit superseding ADR and fixture change it.

- Replay of the log plus its referenced artifacts reproduces the ledger byte-for-byte, whatever
  order the reviewers finished in.
- No `Debug` impl, string comparison, or map iteration order is load-bearing.
- Time and randomness are injected; a fixture can run the same run twice and diff nothing.

## 8. Unix-native

`af` is a Unix tool and composes like one. One static binary; stdout is the result, stderr is
progress, exit codes mean something; `--json` and JSONL streams so every command sits in a
pipe; stdin accepted where a prompt or a patch makes sense. Files and sockets over daemons;
XDG directories for config, state, cache, and runtime; `$EDITOR`, `$PAGER`, `NO_COLOR`,
`SIGINT`/`SIGTERM` handled, children in their own process groups. Plugins are executables on
`PATH`; the TUI is one of them, not the kernel.

- Every command works non-interactively and in a pipe (`af … --json | jq`).
- No command needs a terminal, a browser, or a GUI; interactive prompts have a headless
  equivalent that yields a recorded `needs-human` outcome.
- Config in `$XDG_CONFIG_HOME/af`, state in `$XDG_STATE_HOME/af`, caches in `$XDG_CACHE_HOME/af`,
  locks and sockets in `$XDG_RUNTIME_DIR/af`; nothing machine-written lands in the repository.
- `man af` exists and matches `--help`.

## Product value: sharability

Useful ways of working are portable team assets. Pipelines, Worker definitions, their
contracts, fixtures, and optional profiles can be reviewed and shared through Git. A
developer can reuse a colleague's Pipeline with a different admitted local Worker binding;
the exact effective binding remains visible in the execution evidence.

- A second developer can run a shared Pipeline from locked files without copying credentials,
  machine paths, private transcripts, or live Task state.
- A generated Pipeline can be exported, tested, committed, and reused without another planning
  model call when the next Task fits.
- A shared Worker is referenced by several Pipelines; a deliberate lock update controls adoption.

## Product value: batteries included

Afactory ships useful, tested starting points: review, small implementation, heavy
implementation, documentation, planning, verification recipes, and failure explanations.
Newcomers learn by executing a complete working example and adapting its files.

- A command-based tutorial runs without model credentials or paid inference.
- Onboarding supplies a coherent starter pack and names missing toolchain/Provider bindings.
- Every starter Pipeline includes acceptance, budgets, and positive/negative fixtures;
  an attractive example that cannot execute is not a shipped battery.

## Product value: pluggability

Developers can replace how a capability is performed through a declared contract. Shared
Worker packages and local bindings enable tuning; installed source, Provider, Tool, and
environment adapters extend the product without a new scheduler.

- A replacement Worker satisfies its slot's input/output and independence contracts.
- A second Provider and source adapter run through the same Task runtime.
- Effective overrides are captured; replacing an implementation cannot silently remove
  mandatory verification, enlarge authority, or change a resumed Plan.

These are acceptance requirements for the
[Task execution runtime](task-execution.md), not claims that the full
catalog, plugin contracts, or starter pack already ship.

## Two consequences of the values

- **TOML-first.** All human-authored configuration is TOML: `.af/af.toml`, user config, worker
  packages, pipelines, task contracts, lockfiles. Layered, explicit precedence, no other config
  format for the same purpose.
- **Git is configuration and versioning — not coordination.** Git versions the declarations
  colleagues share (`.af/`) and supplies Snapshots and delivery targets. History, logs,
  tasks, artifacts — the state — live in the kernel's Store (SQLite for now), never in the
  repository. No lock files in the tree, no state merged through git, no state branches.

These values are applied in [`architecture.md`](architecture.md); the
kernel invariants in [`AGENTS.md`](../AGENTS.md) (never weaken a contract, fixture, gate,
budget, or sandbox boundary; no credential in any artifact; publishing is a human action)
remain in force alongside them.
