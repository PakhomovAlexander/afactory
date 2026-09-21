# Afactory — research digest (multi-agent software factories)

**Status:** v1 · part of [`overview.md`](overview.md). Seven sweeps over primary
sources — the kernel repo itself, coding-agent products, orchestration frameworks and durable
execution, protocols and providers, sandboxes and environments, task contracts and
verification, TOML and git-friendly state. What follows is what the design borrowed, with the
evidence; the full sweep reports are not reproduced here.

## 1. What every system converges on

The same dozen things exist in every serious coding-agent system, under different names:

| Ours | Elsewhere |
|---|---|
| Event (append-only log) | OpenHands `EventLog`; SWE-agent `.traj`; Codex `history.jsonl` and `--json` items; Claude Code `.jsonl` transcripts; Anthropic Managed Agents' session log; Jules Activities; Devin's recorded agent calls |
| Snapshot / Env | Codex `SandboxPolicy` + `universal` container; Claude sandbox (Seatbelt / bubblewrap + proxy) and cloud VM; Cursor `environment.json`; Copilot's Actions job; Devin blueprint + snapshot; Jules VM snapshot; OpenHands Workspace |
| Tool | MCP everywhere (Goose: *every* tool is an MCP extension); SWE-agent tool bundles; Codex `apply_patch` + shell + MCP; Claude built-ins + MCP |
| Worker | Codex `.codex/agents/*.toml`; Claude subagents (markdown + frontmatter); OpenCode agents; Goose recipes; Devin playbooks; Factory.ai orchestrator / workers / validators |
| Provider | Codex `[model_providers.<id>]` (`base_url`, `env_key`, `wire_api`); OpenCode `provider/model`; Amp capability presets; LiteLLM as a gateway |
| Task | Factory.ai Missions with a validation contract; Jules session states incl. `AWAITING_PLAN_APPROVAL`; A2A `Task` lifecycle; Symphony's issue-as-control-plane |
| Config layering | Codex TOML (system → user → profile → trusted project → `-c`); Claude JSON (managed → CLI → local → project → user, lists merge); OpenCode (merged, `{env:}`/`{file:}`) |
| Delivery | draft PR on `copilot/*`; Cursor/Jules/Devin PRs; patch export (`codex exec -o`, SWE-agent); schema-constrained final message (`--output-schema`, `--json-schema`) |

Sources: OpenHands SDK ([overview](https://docs.openhands.dev/sdk/arch/overview), [arXiv 2511.03690](https://arxiv.org/abs/2511.03690)); Codex ([protocol](https://raw.githubusercontent.com/openai/codex/main/codex-rs/protocol/src/protocol.rs), [config reference](https://learn.chatgpt.com/docs/config-file/config-reference), [non-interactive](https://learn.chatgpt.com/docs/non-interactive-mode)); Claude Code ([agent loop](https://code.claude.com/docs/en/agent-sdk/agent-loop), [settings](https://code.claude.com/docs/en/settings), [sandboxing](https://code.claude.com/docs/en/sandboxing), [headless](https://code.claude.com/docs/en/headless)); [Cursor cloud agents](https://cursor.com/docs/cloud-agent); [Copilot coding agent](https://docs.github.com/copilot/concepts/agents/coding-agent/about-coding-agent); Devin ([blueprint](https://docs.devin.ai/onboard-devin/environment/blueprint-reference.md), [workflows](https://docs.devin.ai/work-with-devin/dynamic-workflows.md)); [Jules sessions](https://jules.google/docs/api/reference/sessions/); [Factory.ai Missions](https://factory.ai/news/missions-architecture); [Goose recipes](https://raw.githubusercontent.com/block/goose/main/documentation/docs/guides/recipes/recipe-reference.md); [OpenCode config](https://opencode.ai/docs/config/); [Symphony SPEC](https://raw.githubusercontent.com/openai/symphony/main/SPEC.md); [A2A spec](https://a2a-protocol.org/latest/specification/).

## 2. Lessons the design is built on

### Architecture

- **1.** *The append-only log is the spine, and it is a file.* OpenHands calls its SDK "an
   event-sourced state model with deterministic replay"; Managed Agents reboot a stateless
   harness with `wake(sessionId)`; Codex and Claude keep JSONL transcripts. → D5.
   ([OpenHands](https://arxiv.org/abs/2511.03690), [Managed Agents](https://www.anthropic.com/engineering/managed-agents))
- **2.** *Keep the core tiny; register the rest.* mini-swe-agent is ~100 lines and its authors
   concluded "a lot of this is not needed at all"; Codex splits `core` / `protocol` / `exec` /
   `app-server`. → rule 1, twelve crates.
   ([mini-swe-agent](https://github.com/SWE-agent/mini-swe-agent))
- **3.** *Harness assumptions go stale — make them toggles.* Sprint decomposition needed by one model
   generation was unnecessary by the next; "every component in a harness encodes an assumption
   about what the model can't do on its own". → nothing structural compensates for a model
   weakness; profiles and TOML switches do.
   ([Anthropic](https://www.anthropic.com/engineering/harness-design-long-running-apps))
- **4.** *Delegate reads freely, serialize writes.* Cognition: "writes stay single-threaded and the
   additional agents contribute intelligence rather than actions"; a no-shared-context reviewer
   "catches an average of 2 bugs per PR". Anthropic: multi-agent costs ~15× tokens and "most
   coding tasks involve fewer truly parallelizable tasks". → the guest path first; parallel
   fan-out last (v3).
   ([Cognition](https://cognition.com/blog/multi-agents-working), [Anthropic](https://www.anthropic.com/engineering/multi-agent-research-system))
- **5.** *Git is the coordination primitive.* Worktree per task (Claude `--worktree`, Conductor,
   OpenAI "bootable per git worktree"); the C-compiler harness claimed tasks by writing a file
   and let git conflicts act as the mutex; Symphony needs "no persistent database". → rule 8.
   ([Anthropic C compiler](https://www.anthropic.com/engineering/building-c-compiler), [OpenAI harness engineering](https://openai.com/index/harness-engineering/))

### Orchestration and durability

- **6.** *Outcomes are states, not exceptions.* A2A separates `rejected` / `failed` / `canceled` /
   `input-required`; DBOS has `MAX_RECOVERY_ATTEMPTS_EXCEEDED`; AutoGen returns `stop_reason`.
   OpenAI's SDK throws `MaxTurnsExceeded` and CrewAI returns a "best answer" at `max_iter` —
   both lose or disguise the partial result. → seven typed outcomes, `exhausted` keeps evidence.
   ([A2A](https://a2a-protocol.org/latest/specification/), [DBOS](https://docs.dbos.dev/architecture), [OpenAI Agents SDK](https://openai.github.io/openai-agents-python/running_agents/), [CrewAI](https://docs.crewai.com/en/concepts/agents))
- **7.** *Deterministic coordinator, journaled effects, idempotent at-least-once workers, fencing
   tokens.* Temporal ("deterministic, not predetermined"; activities "may be executed multiple
   times"), Restate journals, Inngest memoized steps, Kleppmann's fencing token, Kafka producer
   epochs. LangGraph's checkpoints have "no duplicate execution prevention". → rules 4 and 6;
   no server-backed engine imported.
   ([Temporal](https://docs.temporal.io/activity-definition), [Restate](https://docs.restate.dev/concepts/durable_execution), [Kleppmann](https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html), [Diagrid on LangGraph](https://www.diagrid.io/blog/checkpoints-are-not-durable-execution-why-langgraph-crewai-google-adk-and-others-fall-short-for-production-agent-workflows))
- **8.** *Budgets are runtime predicates, allocated by the planner.* Token usage "explains 80% of the
   variance" in research-agent performance; AutoGen composes termination predicates with
   AND/OR. → `[budget]` predicates, `verification_reserve`.
   ([Anthropic](https://www.anthropic.com/engineering/multi-agent-research-system), [AutoGen](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/tutorial/termination.html))
- **9.** *Detect stalls; replan with a root cause; keep the plan small.* Magentic-One's Task Ledger
   (facts / to look up / to derive / guesses / plan) and five-question Progress Ledger with a
   stall counter. → `PlanRecorded@1`, replan with a recorded reason.
   ([Magentic-One](https://arxiv.org/html/2411.04468))
- **10.** *Contracts are typed artifacts; hand over solutions, not transcripts.* MetaGPT's structured
    interfaces halve tokens per line of code versus ChatDev's chat; subagents return summaries
    while full output stays addressable. → typed Outputs, MCP as the result channel (D6).
    ([MetaGPT](https://arxiv.org/html/2308.00352), [ChatDev](https://arxiv.org/html/2307.07924))
- **11.** *The runtime owns state transitions.* Claude Agent Teams document that "task status can lag:
    teammates sometimes fail to mark tasks as completed". → workers never set a Task's state.
    ([Agent teams](https://code.claude.com/docs/en/agent-teams))

### Verification

- **12.** *The verifier must be nearly perfect, or the agent solves the wrong problem.* Anthropic's
    C-compiler post; Factory.ai writes the validation contract before features and its validators
    "don't implement fixes". → acceptance pinned before dispatch; evaluator separate.
    ([Anthropic](https://www.anthropic.com/engineering/building-c-compiler), [Factory.ai](https://factory.ai/news/missions-architecture))
- **13.** *Test tampering is the default cheat; read-only oracles fix it.* GPT-5 cheats on 76% of
    one-off SWE-bench-style tasks, Claude and Qwen "&gt;79% through modifying test cases";
    read-only test access "restores legitimate performance while preventing test modification";
    an explicit abort option cut cheating from 54% to 9%. METR saw o3 hack 30% of RE-Bench runs
    while claiming intent compliance. → `authority.read_only = ["tests/**"]`, `blocked` and
    `rejected` are cheap outcomes.
    ([ImpossibleBench](https://arxiv.org/html/2510.20270), [METR](https://metr.org/blog/2025-06-05-recent-reward-hacking/))
- **14.** *Judges are biased toward themselves and toward length.* GPT-4 "favors itself with a 10%
    higher win rate; Claude-v1 … 25%"; self-preference tracks self-recognition; "Avoid using the
    same model to generate and judge". Monitors catch 42–65% of cheating and collapse under
    optimization pressure. → different provider by default; gates before judge; the judge never
    overrides a failed gate; prevention over detection.
    ([MT-Bench](https://arxiv.org/abs/2306.05685), [self-preference](https://arxiv.org/abs/2404.13076), [OpenAI CoT monitoring](https://arxiv.org/abs/2503.11926))
- **15.** *Green tests prove little without test quality.* SWE-bench Verified filtered 68.3% of
    samples; PatchDiff found 29.6% of "plausible" patches behave differently from gold; coverage
    is "weakly correlated" with bug detection; mutation-guided tests were accepted 73% of the
    time at Meta. → `pass_to_pass` → `fail_to_pass` → `mutation_score` ordering.
    ([SWE-bench Verified](https://openai.com/index/introducing-swe-bench-verified/), [PatchDiff](https://arxiv.org/abs/2503.15223), [Meta ACH](https://arxiv.org/abs/2501.12862))
- **16.** *Bind evidence to digests.* in-toto Statements match subjects "purely by digest"; SLSA's
    Verification Summary Attestation is the folded-verdict shape; `gh attestation verify` is the
    UX. → attestations per gate, Evidence unchanged.
    ([in-toto](https://github.com/in-toto/attestation/blob/main/spec/v1/statement.md), [SLSA VSA](https://slsa.dev/spec/v1.0/verification_summary))

### Environments and secrets

- **17.** *Sandbox is data: mode × approval × prefix rules.* Codex `sandbox_mode` × `approval_policy`
    × `.rules` (most restrictive wins); Claude `Bash(git diff *)` rules with deny-beats-allow at
    any scope; OS sandboxes cut permission prompts 84%. → `[tool.shell].rules`, `admit`.
    ([Codex rules](https://learn.chatgpt.com/docs/agent-configuration/rules.md), [Claude permissions](https://code.claude.com/docs/en/permissions), [Claude sandboxing post](https://www.anthropic.com/engineering/claude-code-sandboxing))
- **18.** *Secrets never enter the sandbox.* Claude masks credentials with a sentinel the proxy
    swaps; Docker Sandboxes' proxy injects so "credential values never enter the VM"; Codex
    cloud strips secrets before the agent phase; Symphony forbids inheriting tracker credentials.
    → rule 9, the broker ([ADR-0015](../adr/0015-safe-attempts-receive-handles-not-secrets.md) made concrete).
    ([Claude sandboxing](https://code.claude.com/docs/en/sandboxing), [Docker Sandboxes](https://docs.docker.com/ai/sandboxes/security/), [Codex cloud](https://learn.chatgpt.com/docs/environments/cloud-environment.md))
- **19.** *A connected Docker socket proves nothing.* Desktop's proxy accepts while the backend hangs
    for 24+ minutes; `bollard` defaults to a 120 s per-request timeout with no connect timeout.
    → bounded `HEAD /_ping` probe, classified states (the kernel already bounds its probe; the trait keeps it structural).
    ([docker/for-mac#6936](https://github.com/docker/for-mac/issues/6936), [bollard](https://docs.rs/bollard/latest/bollard/struct.Docker.html))
- **20.** *Export the tree in, take git objects out.* Bind mounts cost 2–3× on macOS and defeat the
    boundary; `clonefile`/reflink materialization with an `EXDEV` fallback guard; results leave
    as `git apply --3way --check` patches or pushes confined to `refs/agents/*` — Copilot pushes
    only to `copilot/*`, Claude's proxy only to the session branch.
    ([macOS Docker performance](https://www.paolomainardi.com/posts/docker-performance-macos-2025/), [clonefile](https://keith.github.io/xcode-man-pages/clonefile.2.html), [Copilot](https://docs.github.com/en/copilot/responsible-use/copilot-coding-agent))
- **21.** *Self-escalation paths must be write-protected.* `.git/hooks`, `.git/config`, agent config
    dirs; MCP servers and hooks run outside per-command sandboxes in Claude Code and outside
    Copilot's firewall. → `protected = […]`, MCP inside the boundary or via the broker.
    ([Claude sandbox environments](https://code.claude.com/docs/en/sandbox-environments), [Copilot firewall](https://docs.github.com/en/copilot/how-tos/use-copilot-agents/coding-agent/customize-the-agent-firewall))

### Providers and protocols

- **22.** *Standardize the headless contract.* stdout = final result, stderr = progress, `--json`
    JSONL, `--output-schema` / `--json-schema`, `-o`, exit codes, resume by session id,
    `--bare` / `--ephemeral` for scripted use — Codex, Claude, Factory `droid exec`, Amp.
    → the CLI contract in §8 of the overview.
    ([Codex](https://learn.chatgpt.com/docs/non-interactive-mode), [Claude](https://code.claude.com/docs/en/headless), [droid exec](https://docs.factory.ai/cli/droid-exec/overview))
- **23.** *A headless run trusts the repo unless told not to.* `claude -p` runs `.claude/settings.json`
    hooks, `env`, `apiKeyHelper`, and `.mcp.json` servers "approved or not"; `codex exec`
    attempts in sandbox directories add trust entries to the user's `config.toml` (observed in
    practice). → harness hygiene flags in the Provider adapter.
    ([what runs before trust](https://code.claude.com/docs/en/permissions#what-runs-before-you-trust-a-folder))
- **24.** *Costs are estimates unless a gateway reports them.* Claude's `total_cost_usd` is client-side
    and `usage` excludes subagents; Codex reports tokens only; OpenRouter alone returns money.
    → `Usage.cost_source = reported | estimated`.
    ([Claude cost tracking](https://code.claude.com/docs/en/agent-sdk/cost-tracking), [OpenRouter](https://openrouter.ai/docs/use-cases/usage-accounting))
- **25.** *Normalize to Zed-ACP's event vocabulary.* It is the only published, neutral schema for
    exactly this stream (`tool_call.kind`, statuses, `stopReason`, `usage_update`); Claude,
    Codex, Gemini, Goose, OpenCode, Droid all have adapters; Rust crate `agent-client-protocol`.
    "ACP" also names IBM's protocol (now in A2A) — say "Zed ACP".
    ([ACP](https://agentclientprotocol.com/protocol/overview), [tool calls](https://agentclientprotocol.com/protocol/tool-calls))
- **26.** *MCP is moving under us.* The 2026-07-28 revision removes the `initialize` handshake and
    sessions and deprecates Roots, Sampling, and Logging; `rmcp` 3.x speaks both. → a stateless
    `af mcp serve` with deterministic `tools/list`, no deprecated features; pinned doc URLs.
    ([changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog), [rmcp](https://github.com/modelcontextprotocol/rust-sdk))
- **27.** *Provider abstraction stays thin.* Codex needs only `base_url` + `env_key` + `wire_api`;
    Vercel's AI SDK is the cleanest reference (a small model spec, a registry, a typed
    `providerOptions` escape hatch); Anthropic ships no Rust client — `genai` or thin `reqwest`
    clients behind our trait.
    ([Vercel AI SDK](https://ai-sdk.dev/docs/foundations/providers-and-models), [genai](https://github.com/jeremychone/rust-genai))

### Configuration and state

- **28.** *One precedence ladder with a trust gate and a forbidden-key list.* Codex loads project
    TOML only when trusted and ignores `model_provider(s)` / `notify` / `profiles` there; Claude's
    project settings cannot disable filesystem isolation; mise and direnv gate executable config.
    → [`config.md`](config.md) §1.
    ([Codex config basics](https://learn.chatgpt.com/docs/config-file/config-basic), [mise](https://mise.jdx.dev/configuration.html))
- **29.** *Named tables, not arrays; explicit off-values, because TOML has no null.* Helix must
    special-case `[[language]]` merging by name; Figment replaces arrays; `toml-lang/toml#30` is
    closed and "not revisiting". → `[worker.x]`, `enabled = false`, arrays replace + `extend_*`.
    ([toml#30](https://github.com/toml-lang/toml/issues/30), [Helix](https://docs.helix-editor.com/languages.html), [Figment](https://docs.rs/figment/latest/figment/))
- **30.** *JSONL + CAS in git, SQLite as cache.* Beads v1 (JSONL committed, SQLite ignored, hash IDs
    because sequential IDs produced "two different #7s"); git-appraise's one-JSON-line notes
    merged with `cat_sort_uniq`; jj's content-addressed op log written "without locking"; a
    binary SQLite file in git diffs when nothing changed. → informs the Store's embedded backend and identity rules; the design chose a kernel-level Store over files in the repo (D15/D16), so the git-side mechanics do not apply.
    ([Beads](https://virtuslab.com/blog/ai/beads-give-ai-memory), [hash IDs](https://github.com/gastownhall/beads/blob/main/docs/core-concepts/hash-ids.md), [git-appraise](https://github.com/google/git-appraise), [jj concurrency](https://docs.jj-vcs.dev/latest/technical/concurrency/), [SQLite in git](https://ongardie.net/blog/sqlite-in-git/))
- **31.** *Startup is what you load, not how you parse.* clap parses in 1–2 ms; Cargo's context loads
    config lazily; a hand-rolled argv parser buys nothing. → clap, lazy layers,
    index opened only when needed.
    ([argparse-rosetta-rs](https://github.com/rosetta-rs/argparse-rosetta-rs), [sunshowers](https://rust-cli-recommendations.sunshowers.io/cli-parser.html))
- **32.** *Executables on `PATH` are the extension model that survives.* `git-*`, `cargo-*`,
    `kubectl-*`, `gh` extensions; WASM (Extism, Zed) is stronger but has no subprocess and a
    compile step. → D10.
    ([cargo external tools](https://doc.rust-lang.org/cargo/reference/external-tools.html), [Extism](https://extism.org/docs/concepts/manifest))

## 3. The factory metaphor, honestly

Cusumano's Japanese software factories worked because ~90% of a year's work resembled prior
work, and they plateaued (Toshiba's reuse stalled near 50%); SDC's factory dissolved from
handoff conflicts between design and build. The DoD's definition — "a software assembly plant
that contains multiple pipelines … with minimal human intervention", with control gates as
Go/No-Go points — is the useful one. Translation used here: the Task (work order) carries both
design and acceptance so no organizational seam splits them; pipelines (lines) stay
project-owned and flexible; gates stop the line, never soft-warn; `blocked` is the andon cord;
WIP is capped to human review capacity, the actual constraint (DORA 2025: AI raises throughput
and lowers stability).
([Cusumano](https://dspace.mit.edu/handle/1721.1/2204), [DoD DevSecOps](https://dodcio.defense.gov/Portals/0/Documents/Library/DoDEnterpriseDevSecOpsFundamentals.pdf), [DORA 2025](https://cloud.google.com/blog/products/ai-machine-learning/announcing-the-2025-dora-report))

## 4. Onboarding prior art

CodeRabbit, Graphite, Greptile, Copilot review, Bugbot: OAuth with the Git host, install an app,
the next PR gets a review — "2 minutes", "under 3 minutes … no config files". Output lands in
native PR surfaces; one root config file; a tri-state check that is neutral by default. Almost
none attaches *evidence*; that is the gap `af` fills — every finding names the gate and the
digest that produced it.
([CodeRabbit](https://docs.coderabbit.ai/getting-started/quickstart), [Graphite](https://graphite.com/features/ai-reviews), [Bugbot](https://cursor.com/docs/bugbot))

## 5. Churn to expect

Docker moved sandboxes out of Desktop; Daytona's OSS repo froze; E2B deprecated its Dockerfile
templates; MCP deprecated three features in one revision; Codex docs moved domains twice;
Anthropic's Agent SDK is Python/TypeScript only and forbids third-party claude.ai logins.
Vendor-neutral policy, digest-pinned images, and version-pinned doc links in ADRs are the
defense; vendor SDK coupling is not.
