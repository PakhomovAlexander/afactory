# ADR-2026-11-26-add-antigravity-model-provider: Add Antigravity (`agy`) as a first-class model Provider

Date: 2026-11-26. Status: proposed; accepted on merge.

## Context

Afactory supports three model Provider kinds: `claude` (`claude -p`), `codex` (`codex exec` +
`codex app-server`), and `pi` (AF-owned Pi 1.4.0 Node SDK host). The operator wants to drive
Google Antigravity (`agy` CLI) as a fourth model Provider kind across `af provider`, `af doctor`,
`af task`, and `af review` while keeping every AF invariant intact:

- Token-free capability checks before any login or paid dispatch ([ADR-0146](0146-refuse-a-provider-cli-that-does-not-accept-afs-invocation.md)).
- One model and effort validation authority across plan time, CLI discovery, TUI and adapter
  packages ([ADR-0148](0148-discover-provider-models-and-validate-every-binding-before-a-charge.md)).
- Terminal-only interactive login without capturing or relaying OAuth challenges
  ([ADR-0112](0112-refuse-agent-mediated-provider-logins.md)).
- Adapter-owned confinement derived strictly from kernel `WorkerAccess` so candidate hooks,
  skills, slash commands, and MCP servers cannot widen Worker authority
  ([ADR-0042](0042-require-provider-bindings-and-isolate-claude-reviewers.md),
  [ADR-0118](0118-let-review-workers-execute-checks-in-an-ephemeral-clone.md)).
- Honest account-principal reporting that never fabricates a principal from a path, machine
  installation ID, or credential digest
  ([ADR-2026-11-07](2026-11-07-generalize-pi-provider-authority-with-explicit-profile-capabilities.md)).
- Honest token accounting that records missing usage as unknown rather than estimating it
  ([ADR-0143](0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md)).
- Versioned closed schemas with no cross-release compatibility readers
  ([ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).

Empirical inspection of `agy` 1.5.18 established four key transport facts:

1. `agy acp --stdio --gemini-dir <auth_dir> --app-data-dir . --disable-telemetry` isolates all
   ACP state to `<auth_dir>`, answers JSON-RPC `initialize` token-free with agent metadata and
   `authMethods`, and answers `session/new` token-free with either `-32000 Authentication required`
   (without hanging or emitting OAuth URLs) or the authenticated model catalog
   (`models.availableModels`), effort options (`configOptions`), and subscription tier
   (`_meta.antigravity.tier.name`). It does not expose a stable account principal.
2. `agy --input-format stream-json --output-format stream-json --disable-slash-commands --model M
   --effort E [--json-schema FILE] --sandbox --settings FILE` accepts a single NDJSON user turn
   on stdin (`{"event":"user","message":{"content":"..."}}`), removing argv size limits for large
   Worker requests, and streams `init` (carrying the effective `model`), `step_update`, and
   terminal `result` envelopes (carrying `status`, `response`, `structured_output`, `error`, and
   `usage`).
3. `agy`'s `usage.input_tokens` already excludes `cache_read_tokens` (`step_update` and `result`
   confirm `input_tokens + output_tokens == total_tokens` while `cache_read_tokens` is reported
   separately).
4. `agy --mode plan` has no effect when `--disable-slash-commands` is enabled; tool confinement
   is enforced via `--sandbox` together with a session-only `--settings` JSON file whose
   `permissions` rules follow Antigravity's `deny > ask > allow` precedence.

## Considered options

- **Run `agy --print "..."` via argv**: rejected because Worker requests can be up to 1 MiB
  (`MAX_WORKER_BYTES`), hitting OS `ARG_MAX` limits, and plain print mode emits no `init` event
  naming the effective model or incremental progress events.
- **Run `agy models` text parsing for catalog discovery**: rejected because `agy models` writes
  terminal spinner frames and unstructured columns, whereas `agy acp --stdio` returns exact
  structured JSON (`models.availableModels`, `configOptions`, tier) and enforces explicit
  `--gemini-dir` isolation without creating files in `HOME`.
- **Manufacture a `principal_id` from `installation_id` or `auth_dir`**: rejected because neither
  proves native account independence; `installation_id` is generated even when logged out.
- **Drive `agy` through the Antigravity Python SDK**: rejected because it introduces a Python
  package and runtime dependency whereas the official `agy` binary already provides headless
  `stream-json` execution and `acp` stdio JSON-RPC.
- **Use `agy acp --stdio` for token-free compatibility, status, identity, and catalog probes and
  `agy --input-format stream-json --output-format stream-json` for Task Worker execution**:
  selected.

## Decision

Add `antigravity` as a fourth Provider kind driving the `agy` binary:

1. **Crate boundary (`crates/af-runner-antigravity`)**:
   - Owns `ANTIGRAVITY_INVOCATION: CliInvocation` (`program: "agy"`, `help_args: &["--help"]`,
     exact required flags, `upgrade: "agy update"`), held equal to the command builder by unit
     test (ADR-0146).
   - Owns `AntigravityTaskAdapter` implementing `af_runner::task::WorkerModelAdapter` over
     `agy --input-format stream-json --output-format stream-json`.
   - Generates a per-Attempt session-only `--settings` file inside the Attempt's private staging
     directory from `WorkerAccess` alone (`deny` > `ask` > `allow`), always passing `--sandbox`
     and `--disable-slash-commands` after package model flags so packages and candidate
     repositories cannot widen tool, shell, URL, or MCP authority.
   - Projects strict reply schemas into `<staging>/reply-schema.json` via `--json-schema` when
     supported by `ReplyContract::strict`, retaining kernel-only validation when a contract has no
     strict projection (ADR-0208, ADR-2026-11-17).
   - Verifies that `init.model`, when reported, matches the bound `--model` and refuses silent
     model substitution.
   - Charges `input_tokens + output_tokens` plus separate `thinking_tokens` when `total_tokens`
     includes them (`cache_read_tokens` excluded as already separated by `agy`). Missing or
     unreported usage settles as unknown with its classified cause (ADR-0143).
   - Removes the Attempt's recorded conversation artifacts under `<auth_dir>` after process exit
     unless `[storage] keep_worker_transcripts` is enabled (ADR-0144).

2. **Shared model rules (`crates/af-runner`)**:
   - Add `models::Kind::Antigravity` with `ANTIGRAVITY_EFFORTS = ["low", "medium", "high",
     "xhigh", "max"]` and add Go's `flag` parser diagnostics (`flags provided but not defined`,
     `flag provided but not defined`, `flag needs an argument`) to `INVOCATION_ERROR_MARKERS`.

3. **Machine-local Provider subsystem (`crates/af/src/providers*`)**:
   - Support `af provider setup/add/remove/status/models/detect/doctor` for `--kind antigravity`,
     default auth directory `~/.gemini/antigravity-cli`, and `O_NOFOLLOW` marker inspection
     (`installation_id`, `jetski_state.pbtxt`, `conversation_summaries.db`).
   - Capability checks verify both `agy --help` against `ANTIGRAVITY_INVOCATION` and a token-free
     `agy acp --stdio --disable-telemetry` JSON-RPC `initialize` round trip.
   - Status, Task identity, and model catalog probes run `agy acp --stdio --gemini-dir <auth_dir>
     --app-data-dir . --disable-telemetry` (`initialize` + `session/new`).
   - Interactive login (`af provider setup --kind antigravity --login`) remains terminal-only with
     `LOGIN_SECURITY_WARNING` and runs `agy acp --login --gemini-dir <auth_dir> --app-data-dir .`
     (ADR-0112). Remote auth handoff (`af provider auth`) and `af task wake` remain unsupported
     for Antigravity in this increment and fail closed.

4. **Honest unavailable principal identity**:
   - Because `agy acp` does not expose a verified account principal, Antigravity captures
     `principal_id: null` in `WorkerExecutionV3::Model` (or `Some(digest)` if a future harness
     reports one explicitly), forbidding Pi authority.
   - Trusted `distinct_principals` independence policy continues to refuse any binding with
     `principal_id: null` uniformly across provider kinds, and installed native Review retains its
     known-principal requirement.

5. **Versioned contracts (ADR-0145)**:
   - Advance closed provider schemas and Task execution contracts in lockstep with their writers,
     readers, shipped schema table, `af help json`, contract inventory, and parity tests:
     - `provider-registry` v4 -> v5 (`version = 5`)
     - `provider-status` v6 -> v7 (`af/provider-status@7`)
     - `provider-models` v4 -> v5 (`af/provider-models@5`)
     - `provider-detect` v4 -> v5 (`af/provider-detect@5`)
     - `provider-setup` v2 -> v3 (`af/provider-setup@3`)
     - `task-contracts` v4 -> v5 (`af/ExecutionPlan@5`, updating dependent Task inspection and
       provider-admission schemas where they reference `task-contracts:5`)
   - Retired versions fail closed with no migration or compatibility readers.

## Consequences

- Operators can register, detect, inspect, list models for, and execute AF Tasks on Antigravity
  (`agy`) with the same safety, budget, redaction, and contract guarantees as Claude, Codex, and
  Pi.
- Because `provider-registry` advances to version 5 and `task-contracts` advances to version 5,
  existing machine registries and Stores written by earlier releases fail closed under ADR-0145
  and must be recreated or finished under their original pinned release.
- `af onboard` default template selection, `af provider auth` remote handoff, `af task wake`, and
  repository operator catalog pipelines (`.af/task-packages/`) remain unchanged in this increment.
