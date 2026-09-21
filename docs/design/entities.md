# Afactory — the entities

**Status:** v2 · part of [`overview.md`](overview.md). Kernel terms (Snapshot,
Subject, Attempt, Finding, …) keep their canonical meaning from the kernel's
[`CONTEXT.md`](../../CONTEXT.md);
this document places them in the factory and adds the ones the factory needs. Lifecycles are in
[`state-machines.md`](state-machines.md); the Store in [`store.md`](store.md).

The [Task execution increment](task-execution.md) proposes the next concrete contracts:
Pipeline definitions versus recorded Execution Plans, reusable Worker slots/local bindings,
and applicability-driven selection or generation. Its examples are proposed syntax; the
descriptions below retain the earlier v2 design context.

Six entities a human **authors** (TOML): [Provider](#provider) · [Env](#env) · [Tool](#tool) ·
[Worker](#worker) · [Pipeline](#pipeline) · [Task](#task).
Six the kernel **records**: [Snapshot](#snapshot) · [Input](#input) · [Attempt](#attempt) ·
[Output](#output) · [Event](#event) · [Ledger](#ledger).
Where each lives is a separate axis: **git** (`.af/` — Env, Tool, Worker, Pipeline), **the
machine** (`~/.config/af` — Provider), **the Store** (Task and everything recorded).
Cross-cutting mechanisms that are properties, not entities: [Budget, Broker Handle,
Operation](#cross-cutting-budget-broker-handle-operation); and two things designed in their
own steps: [the Store](store.md) and [the worker protocol](#the-worker-protocol-separate-design-step).

Each entity is specified the same way: what it is, how it is authored or recorded, its
interface, how it is substituted, what it relates to, and what to avoid. Interface sketches
are Rust because the kernel is Rust; they are shapes, not signatures.

---

## Provider

**What.** An operator-named, machine-local authentication context for one backend kind, plus
the adapter that speaks to it. The ID is a stable label for the context, never a verified
account identity; credentials are used *through* a Provider and are never part of it.

**Kinds.** `claude-code` (drives `claude -p`), `codex` (drives `codex exec` / app-server),
`anthropic` (Messages API), `openai` (Responses API), `openai-compatible` (`base_url`),
`gemini`, `acp` (any Zed-ACP agent binary), `command` (a deterministic program — the test and
fixture adapter). v1 ships `claude-code`, `codex`, `command`.

**Authored** — in the *user's* config; a project may declare that a name exists and what kind
it is, never how it authenticates:

```toml
# ~/.config/af/config.toml
[provider.claude]
kind = "claude-code"
auth = "login"                     # login | env:ANTHROPIC_API_KEY | helper:"<command printing a token>" | keychain | none
home = "~/.claude"                 # the harness's own home; read in place, never copied
model = "opus"                     # default for workers that do not say

[provider.codex]
kind = "codex"
auth = "login"                     # ~/.codex/auth.json or the keyring, read in place

# .af/af.toml (project) — a requirement, so `af doctor` can name what is missing
[provider.claude]
kind = "claude-code"
```

**Interface.**

```rust
pub trait Provider {
    fn kind(&self) -> ProviderKind;
    fn caps(&self) -> ProviderCaps;          // structured_output, tool_calls, resume, cost_reported, native_sandbox, mcp
    fn preflight(&self, op: &mut Operation) -> Preflight;      // bounded auth probe + one smoke inference
    fn start(&self, spec: AttemptSpec) -> Result<Session>;     // root, instructions, tool policy, MCP servers, model, effort, budget
    fn events(&self, s: &Session) -> impl Stream<Item = ProviderEvent>;   // normalized
    fn cancel(&self, s: &Session, how: Cancel);                // Graceful (SIGINT) | Kill
}
pub struct Usage { input: u64, output: u64, cache_read: u64, cache_write: u64, reasoning: u64,
                   cost: Option<Money>, cost_source: CostSource /* Reported | Estimated{table} */ }
```

**Why one trait — D7, explained.** Today the CLI picks an adapter by the runner's file name
(`claude` → `ClaudeAdapter`, `codex` → `CodexAdapter`, anything else → a raw `Command`), and
each adapter parses its own dialect: Claude Code's `{is_error, result, usage}` from
`claude -p --output-format json`, Codex's JSONL `turn.completed` / `item.completed` from
`codex exec --json`. The engine, the ledger, and the TUI therefore see harness-specific shapes,
and a third backend touches all of them. *One trait* means every backend implements the same
five functions and the engine only ever sees `ProviderEvent` — the same dozen event shapes
whoever produced them: text, thought, tool call, tool result, file change, command execution,
usage, result. *Zed-ACP vocabulary* means those events borrow their names from the Agent
Client Protocol — the JSON-RPC protocol Zed's editor uses to talk to Claude Code, Codex,
Gemini CLI, Goose and others: `tool_call.kind ∈ read | edit | delete | move | search | execute |
think | fetch | other`, `status ∈ pending | in_progress | completed | failed`, `stop_reason ∈
end_turn | max_tokens | max_turn_requests | refusal | cancelled`, `usage_update`. It is the only
published, vendor-neutral schema for exactly this stream, so `af`'s events mean the same thing
for every backend — and an `acp` kind can drive any ACP-speaking agent with no bespoke adapter.
*Cost tagged by source* means: Claude Code reports `total_cost_usd`, but it is a client-side
estimate; Codex reports tokens only; only gateways such as OpenRouter return real money. So
`Usage.cost` says whether it was reported or estimated, and by which price table — a budget in
dollars is honest about what it enforces. Concretely: the same attempt run through
`claude-code` and through `codex` yields two logs with identical event kinds and identical
Output types; only the `provider` field and the numbers differ.

**Harness hygiene.** Attempts run harness CLIs with their *user* settings only and an explicit
MCP configuration (`claude -p --bare --setting-sources user --strict-mcp-config …`;
`codex exec --ignore-user-config --ephemeral -a never …`), so a repository's own `.claude/` or
`.codex/` cannot inject hooks, environment, or servers into an attempt, and so an attempt's
sandbox directory never lands in the user's `~/.codex/config.toml` trust table (it does today).

**Recorded.** Provider `Operation` events (`running` · `waiting_for_human` · `resumed` · `done`
· `failed`) with redacted failure fingerprints and a circuit breaker after two identical
failures; per-attempt `Usage` in the Output receipt. Never a
credential, token, code, or raw secret-bearing output.

**Substitution.** Change `kind`; or an executable `af-provider-<name>` on `PATH` implementing
the same contract as JSON over stdio (`af.provider@1`).

**Relationships.** Worker → Provider by name · Attempt → Session (transient, disposable) ·
Broker Handles cover external capabilities, not the Provider's own authentication.

**Avoid.** "provider" for a model, a binary, or a package · passing provider credentials into
an Env · treating `auth status` as readiness (the smoke inference is the readiness).

---

## Env

**What.** A reproducible place to run one attempt, declared by what it *provides*: an isolation
tier, a base (toolchain or image), Cache Snapshots, a network policy, a filesystem policy, and
resource limits. A Worker or pipeline declares what it *requires*; `admit` refuses a pairing
where provided < required.

**Tiers.** `host` — a copy-on-write materialization on the host, no isolation (today's
`trusted_local`; may never claim more) · `process` — an OS sandbox around the process (macOS
Seatbelt, Linux bubblewrap + Landlock/seccomp; what Codex and Claude Code use) · `container` —
OCI container by pinned image digest (docker / podman / nerdctl / Apple `container`) · `vm` —
microVM or remote environment. Every tier materializes the same Snapshot and seals the same
way. v1 ships `host`.

**Authored.**

```toml
# .af/af.toml
[env.default]
isolation = "host"                   # host | process | container | vm
fallback = "fail"                    # or ["host"]: a downgrade is recorded in the run report, never silent
network = "none"                     # none | allowlist | proxy | full
writable = ["."]                     # paths inside the materialized tree
read_only = [".git"]
protected = [".git/hooks", ".git/config", ".af", ".claude", ".codex"]   # never writable: self-escalation paths
caches = ["cargo"]                   # named Cache Snapshots from [cache.*]; sandbox-local copies
limits = { cpu = 4, memory = "8g", wall = "30m", disk = "10g" }

[env.ci]
isolation = "container"
image = "ghcr.io/org/dev@sha256:…"   # by digest, never a floating tag
setup = "scripts/setup.sh"           # runs with network before the attempt; its result is snapshotted
```

**Interface.**

```rust
pub trait EnvProvider {
    fn provides(&self) -> Isolation;                                   // the tier it can truthfully claim
    fn probe(&self, timeout: Duration) -> Probe;                       // always bounded
    fn materialize(&self, snap: &Snapshot, spec: &EnvSpec, mode: Mode) -> Result<Sandbox>;  // CoW clone
    fn exec(&self, sb: &Sandbox, cmd: &Command, grants: &Grants) -> Result<Exit>;
    fn seal(&self, sb: Sandbox) -> Result<Sealed>;                     // mutations derived by rescanning, never reported
}
pub fn admit(required: Isolation, provided: Isolation) -> Result<(), Refused>;
```

**What "admit wired" means — D8, explained.** The kernel already contains the pieces: an
`Isolation` enum (`None`, `Process`, `Container`), a `Policy`, an `admit(required, provided)`
function that refuses a pairing where the sandbox provides less than the pipeline requires,
and a `ContainerProvider`. Nothing calls them: `Kernel::sandbox` always builds the host copy
and reports `Isolation::None`, a pipeline cannot state a requirement, and the container probe
(`docker info`) had no timeout until it was bounded later — a fix the trait makes structural. *Wired* means four things: (1) every
Worker and pipeline states `requires`; (2) every Env states what it `provides`; (3) the Worker
FSM calls `admit` at `reserved → dispatched` and records `AttemptReleased{reason: isolation}`
instead of dispatching when provided < required; (4) every runtime probe is bounded
(`HEAD /_ping`, 1–3 s) and classified `not-installed` / `stopped` / `waking` / `hung`. In v1
only `host` exists, so every `requires` is `host` — but the check runs on every dispatch, so
adding `process` later is a config change, and no pipeline can ever silently run below what
it asked for.

For `container` and `vm` the Snapshot is exported *into* the guest disk, not bind-mounted:
bind mounts cost 2–3× on macOS and a writable host mount defeats the boundary. MCP servers and
hooks a worker uses run inside the same boundary as the worker, or through the broker.

**Recorded.** `ExecutionBinding@1` — the digest of the resolved spec — referenced by every
Input; probe results as events.

**Substitution.** `isolation` selects the tier; `kind` or `af-env-<name>` selects the
implementation (`af-env-e2b`, `af-env-apple-container`).

**Avoid.** A worktree mistaken for a security sandbox (the `host` tier cannot claim isolation)
· host cache passthrough · untimed probes · secrets inside the Env — the proxy injects them or
a Broker Handle stands in for them.

---

## Tool

**What.** A capability a worker may invoke during an attempt, mediated and audited by the
kernel's broker. Four kinds: `shell` (commands under ordered prefix rules), `command` (one
executable exposed as a named tool with typed argument slots — the gate checks), `mcp` (a
server the kernel launches or proxies), `af` (kernel-native tools served over MCP to
*external* agents in v1; to workers inside attempts once the protocol step decides — D6).

**Authored.**

```toml
[tool.shell]
kind = "shell"
rules = [                                   # first match wins; deny beats everything at any layer
  { prefix = "git diff",   decision = "allow" },
  { prefix = "cargo test", decision = "allow" },
  { prefix = "git push",   decision = "deny" },
  { prefix = "*",          decision = "ask" },      # headless: `ask` becomes a needs-human outcome
]

[tool.lint]
kind = "command"
program = "npx"
args = [{ value = "--yes" }, { value = "markdownlint-cli2@0.22.1" }, { value = "**/*.md" }]   # literal slots; `untrusted` never in options

[tool.github]
kind = "mcp"
command = "gh-mcp"
handle = "github:read"                      # a Broker Handle: named operations, revocable on fencing
```

**Interface.**

```rust
pub trait Tool {
    fn describe(&self) -> ToolSpec;                                  // name, input schema, hints: read_only / destructive
    fn call(&self, ctx: &AttemptCtx, input: Value) -> Result<ToolResult>;   // ctx carries the epoch; a stale epoch is refused
}
```

**Recorded.** `ToolCalled@1` / `ToolReturned@1` per attempt — quarantined if the epoch was
revoked; `CheckCompleted@1` for gate commands; Broker Handle issue and revoke events.

**Substitution.** Any MCP server; `af-tool-<name>` executables; prefix rules are data.

**Relationships.** Worker.tools lists names · a pipeline `gate` runs `command` tools · the
broker maps external capabilities to handles.

**Avoid.** Reusable credentials in tool config · tools that can edit acceptance checks (the
evaluator never gets them) · interactive prompts in headless runs.

---

## Worker

**What.** A role: the package a Provider executes as a Worker state machine. Instructions
(`worker.md`), model and effort, tools, Env requirement, budget defaults, kind, and output
contract. Kinds: `reviewer` (judges, produces Reports), `implementer` (changes code, produces a
Proposal), `evaluator` (judges a result Snapshot against an acceptance contract, produces a
Verdict), `planner` (produces a bounded plan). v1 ships `reviewer`.

**Authored** — `.af/workers/<name>/worker.toml` + `worker.md`, content-pinned in `.af/af.lock`:

```toml
name = "architecture"
version = "2.0.0"
kind = "reviewer"                    # reviewer | implementer | evaluator | planner
provider = "claude"                  # a Provider name; the user's config supplies auth
model = "opus"
effort = "xhigh"
tools = ["shell", "lint"]
env = "default"
requires = "host"                    # minimum isolation; admit() enforces it
subjects = ["diff", "whole-tree"]    # reviewer kind only
outputs = ["af/ReviewerResult@1"]

[budget]
tokens = 300_000
wall = "20m"
```

**Interface.** Data plus one function: `render(&Input) -> Instructions`. Rendering includes only
the Worker's declared, typed prompt ports; it never expands the whole Snapshot, parent transcript,
full Ledger, or another Worker's private reasoning into the model context. The Snapshot remains
inspectable through allowed Tools. The attempt runner records the rendered byte and estimated-token
counts, builds an `AttemptSpec` from Worker + Env + Tools + Input, and drives the Worker FSM
([`state-machines.md` §4](state-machines.md#4-worker)) through `Provider::start`.

**Recorded.** The package digest inside every Input. A Worker has no state of its own.

**Substitution.** One key (`provider`, `model`); another package by name, pinned by digest.

**Relationships.** Pipeline `workers` state lists names · Task strategy binds roles to workers ·
a Task's evaluator must be a different Worker than its implementer, and by default a
different Provider (enforced at admission).

**Avoid.** "reviewer" for any other kind · instructions that read ambient state — every input
is wired · a Worker that names a credential.

---

## Pipeline

**What.** A declared state machine over one round: states with kinds (`capture`, `gate`,
`workers`, `reduce`, `verify`, `decide`, terminals), typed wiring between them, guarded
transitions, and the budget and convergence parameters the guards read. Specified with a
complete `review.toml` in [`state-machines.md` §3](state-machines.md#3-pipeline).

**Authored.** `.af/pipelines/<name>.toml`, pinned in `.af/af.lock`.

**Recorded.** Pipeline digest in the Task's plan; `StateEntered@1` per transition;
`RoundStarted@1` / `RoundClosed@1`.

**Substitution.** Another file; a Task's strategy names one; a Task kind registers state kinds
and guards.

**Avoid.** Node semantics in a `match` inside the engine — kinds and guards are registered by
Task kinds · running a subset of states ad hoc (a pipeline defines what a round is) · ambient
inputs.

---

## Task

**What.** A durable contract over one engineering outcome — a *Mission* — with a
`kind`. Goal, acceptance, authority, constraints, budget, strategy. Any change outside
`strategy` creates a new immutable **revision**. The outcome is decided by evidence the worker
did not produce. Kinds: **`review`** — goal "review this Subject", extra input a diff (or
whole tree), pipeline `review`, acceptance = the convergence policy, output the Finding and
Demand Sets; **`implement`** (v2) — goal and acceptance contract, pipeline `implement-verify`,
output a verified derived Snapshot.

**Authored, stored in the Store.** A Task is *not* a file in the repository: `af task start
--kind review --diff main..HEAD`, `af review --uncommitted` (sugar), or `af task start
contract.toml` records the contract in the Store; `af task show <id> --toml` prints it back.
Colleagues share pipelines and workers through git, and tasks through the Store.

```toml
# contract.toml — passed to `af task start`, recorded in the Store
[task]
kind = "implement"
goal = "Add a token-bucket rate limiter to the HTTP API: 100 req/s per key"
context = ["docs/api.md"]

[acceptance]                                # pinned before dispatch; a change is a new revision
evaluator = "evaluator-strict"              # a Worker of kind evaluator, ≠ the implementer, other provider by default
evidence  = ["bench:rate-limit"]            # required Evidence (Demands), waived only explicitly
criteria  = ["existing routes keep behavior", "limit is configurable per key"]

[[acceptance.check]]
id = "AC-1"
text = "WHEN a key exceeds 100 req/s THE SYSTEM SHALL answer 429 with Retry-After"
kind = "fail_to_pass"                       # must fail on Base, pass on the result
tool = "cargo-test"
args = ["rate_limit_429"]

[[acceptance.check]]
id = "AC-2"
text = "existing behaviour is preserved"
kind = "pass_to_pass"                       # runs first; exit(0)/skip tricks fail here
tool = "cargo-test"

[[acceptance.check]]
id = "AC-3"
text = "the new tests are not vacuous"
kind = "mutation_score"
scope = "src/net/limit.rs"
min = 0.8

[authority]
base = "main"                               # resolved once, pinned by Snapshot ID
writable  = ["src/net/**"]
read_only = ["tests/**", "Cargo.lock", ".github/**"]   # the implementer cannot edit its own oracle
deliver = "none"                            # none | branch | pr — publishing is a separate, human-confirmed step

[constraints]
providers = ["claude", "codex"]
deadline = "4h"

[budget]                                    # predicates; the first to fire yields `exhausted`
tokens = 3_000_000
usd = 25
attempts = 12
wall = "2h"
fan_out = 3
verification_reserve = 0.25                 # held back so gates and the evaluator always run

[strategy]                                  # replaceable inside a revision
pipeline = "implement-verify"
workers = { implement = "implementer", evaluate = "evaluator-strict" }
```

**Interface.** A Task kind registers: its pipeline(s), state kinds and guards, reducers, the
Output types it accepts, and the outcome mapping. The Task FSM
([`state-machines.md` §2](state-machines.md#2-task)) is the same for every kind.

**Verification order.** Gates run in a fresh materialization of the result Snapshot with the
acceptance checks restored from the Authority Snapshot: `pass_to_pass` first, then
`fail_to_pass`, then test quality (`mutation_score`), then static and policy gates, and the
evaluator last — as a reader of evidence that sees only the diff, the criteria, and the gate
results, and can never override a failed gate. Every gate result is an attestation: subject
(result Snapshot digest), predicate, result — the same shape as Evidence.

**Recorded.** `TaskSubmitted@1` (revision digest; the contract as an object),
`TaskRevised@1`, `PlanRecorded@1`, `VerdictRecorded@1`, `TaskWaiting@1` / `TaskAnswered@1`,
`TaskClosed@1`. Outcomes: `verified` · `unverified` · `blocked` · `exhausted` · `needs-human` ·
`cancelled` · `rejected`.

**Substitution.** The strategy; the evaluator; the budget predicates; the kind.

**Relationships.** Task → Rounds → Attempts · pins an Authority Snapshot · an integrated
Proposal advances an internal derived Snapshot that a later round verifies.

**Avoid.** Worker-declared completion · silent acceptance drift · a `verified` outcome without
artifact-linked evidence · committing task files to the repository.

---

## Snapshot

**What.** An immutable, admissible source state carrying repository and capture provenance
plus one Tree Digest. The factory's unit of "which code" for Inputs, Outputs, Evidence,
Proposals, and Outcomes alike. Git is the source of Snapshots (trees by digest); the Store
holds them.

**Interface.** The source adapter — `capture(selector) -> Snapshot`, `materialize(snapshot,
dest)`, `tree_diff(base, head) -> ChangeSet` — is git today (allowlisted plumbing, bare-repo
diff). A `dir` source for fixtures is the second implementation.

**Avoid.** Commit, branch, or ref as identity — those are selector inputs.

---

## Input

**What.** The exact, content-addressed inputs of one attempt: snapshot and subject ids, wired
artifact ids, worker package digest, instructions digest, Execution Binding digest, tool policy
digest, budget reservation, epoch, and a context manifest naming every artifact rendered into the
model context with its byte and estimated-token size. The Snapshot is an inspectable source object,
not an instruction to paste the repository into the prompt. Bounded on-demand retrieval is a Tool
call whose request and result are events. `af attempt show <id> --json`.

**Avoid.** Ambient inputs of any kind — if it is not in the Input or a recorded Tool result, the
Worker did not have it and replay would not have it either · default parent transcripts, full
Ledgers, repository dumps, unrelated documents, or other Workers' private reasoning.

---

## Attempt

**What.** One execution of one worker under one epoch and one budget reservation, run as the
Worker FSM. Reserve before dispatch, fence on supersession or timeout, quarantine late
arrivals, charge what was spent.

**Recorded.** `AttemptReserved@1` · `AttemptDispatched@1` · `AttemptSealed@1` ·
`AttemptAdmitted@1` · `AttemptReleased@1` · `AttemptFailed@1` · `AttemptFenced@1` ·
`OutputQuarantined@1`; the reservation across scopes `attempt` · `node` · `fan_out` · `run`.

---

## Output

**What.** What an attempt produced, sealed by the kernel: the derived diff (rescanned, never
reported), typed result artifacts, and the usage receipt. Per Worker kind: reviewer →
`ReviewerResult@1`; implementer → `PatchProposal@1` (admitted only if equal to the sealed
diff); evaluator → `Verdict@1`; planner → `SliceSet@1`. Raw output is stored before it is
parsed, so "malformed" stays a falsifiable claim.

---

## Event

**What.** One append-only, versioned record in a run's log: run id, dense sequence, derived
id, `type@version`, machine and state, node and attempt ids, causation, artifact references,
payload. Vocabulary in [`state-machines.md` §5](state-machines.md#5-events-the-vocabulary-the-machines-consume);
storage in [`store.md`](store.md).

---

## Ledger

**What.** The projection of a run, rebuilt from the log and the objects it references, served
by the Store as a versioned view; never written directly. The review kind projects Finding
Sets and Demand Sets; the implement kind projects plan, verdicts, outcome, and spend. `rebuild`
is the only constructor.

**Avoid.** Treating it as storage · ambient "latest" queries — every consumer names one exact
view.

---

## Cross-cutting: Budget, Broker Handle, Operation

- **Budget** is authored on a Task, a Pipeline, or a Worker and enforced by Attempt
  reservations; predicates compose (`tokens` AND `wall` OR `attempts`), the tightest scope
  refuses and names itself.
- **Broker Handle**: a non-secret, attempt-and-epoch-bound capability for named external
  operations, revoked on fencing. How a Tool reaches GitHub without a token.
- **Operation**: a durable, resumable, fenced provider interaction
  (`running` → `waiting_for_human` → `resumed` → `done` | `failed`) — the shape every
  human-in-the-loop pause in the factory reuses.

---

## The worker protocol (separate design step)

**What it is.** How an attempt receives its Input and returns its Output — the wire between
the Worker FSM's `running`, `tool_call`, and `sealing` states and the process the Provider
started. Today: instructions and inputs on stdin, one JSON document on stdout, parsed after
being stored raw. This is designed as its own activity (D6).

**What is already fixed** (by the state machines and the Store): the events the protocol must
produce (`ToolCalled`, `ToolReturned`, `AttemptSealed`, result / error / malformed), that every
message carries the attempt id and epoch so stale traffic is quarantined, that raw bytes are
stored before parsing, that results are typed artifacts, and that silence is never success.

**Candidates the step weighs.** (a) the current prompt-in / JSON-out contract with
`--output-schema`; (b) kernel-native tools over MCP inside the attempt (`af.report` …), which
Claude Code and Codex already speak; (c) JSON-RPC over stdio or a Unix socket in the Zed-ACP
shape, which would make `af` an ACP client and any ACP agent a worker; (d) a hybrid — MCP for
tools, a final structured result over stdout.

**Questions it answers.** Transport and framing; the message vocabulary and its versioning;
streaming and backpressure; authentication of the worker to the kernel (a per-attempt token);
how `command` providers and fixture workers speak it; audit and redaction; how a worker asks a
question (`needs-human`) without ending the attempt.
