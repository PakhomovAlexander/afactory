# Captured local Worker bindings

Task-file commands accept an explicit `--bindings FILE`. The file uses
`schema = "af.task-bindings/1"` and may supply `slots`, `providers` and exactly pinned
`packages`. Omitted tables are empty. For an alternative Worker already in the committed
catalog, a local file can be as small as:

```toml
schema = "af.task-bindings/1"

[slots]
"root.slots.implementer" = "team/fast-implementer"

[providers]
"team/fast-implementer" = "codex-main"
```

Use `af task plan --file ticket.json --bindings alice.toml --json` to inspect the effective
plan before execution. The same option works with `task start`, `review run --file` and
`review plan --file`. Model, effort and Attempt cost are properties of the replacement
Worker package; its Provider alias supplies the locally admitted account.

An alias does not bypass Provider admission. A Pi Worker uses `provider_kind = "pi"`, an
exact SDK catalog model ID and thinking level, and an explicitly registered `openai-codex`
or `kimi-coding` OAuth Provider. OpenAI requires a stable opaque account identity; Kimi
explicitly records unavailable account identity separately from its auth context and refuses
trusted `distinct_principals` policy. The common factory checks available account identity,
auth context, backend, SDK and model configuration before private input. Kimi account
substitution within the same context cannot be detected. Its embedded SDK host exposes confined file tools and
no shell. See the [Pi Provider design](../design/pi-provider.md) for setup, the exact support
matrix and token-free fixture evidence; paid live smoke was not run.

Default `af onboard` emits standalone Review and implementation authority together. The explicit
`af onboard --tasks` mode previews or adds that catalog only where Task settings and target
files are absent; it refuses an existing `[task]` table even in preview mode. Use plain
`af onboard` to validate existing Task authority.

The generated `onboard/implementer` and `onboard/evaluator` use `codex-main`: the implementer
uses `gpt-6.1-sol` at high effort and the independent evaluator `gpt-6.1-terra` at high effort.
`--runner` selects the `onboard/bugs` and `onboard/correctness` Review Workers and their Provider
aliases: `codex-main` with `gpt-6.1-terra` at high effort, or `claude-main` with
`claude-opus-5-5` at high effort. Implementation and evaluation always require a Codex Provider,
including on a Claude-only machine.
If the local registry uses another ID, map those package names to that ID in `[providers]` of a
machine-local bindings file and pass `--bindings FILE` to `af task plan`. The implementer and
independent judging Workers use distinct models; one registered login can serve them.

Additional packages must use `local/*` names, exact version/digest pins and paths beneath
the bindings file's directory. They use the same Worker manifest and input/output schemas
as shared packages. Capture rejects symlinks, namespace shadowing and excessive closure
size. A local file cannot overwrite the committed policy or change mandatory checks.

Qualified slot names come from `af task explain`. An embedded slot mapped to a parent uses
the parent's physical slot name. Unknown names, forbidden replacements, changed payload
schemas, weaker evidence/retention or wider effects fail before model admission. Attempt
limits and verification reserves remain owned by the Pipeline and Task. Verification
Workers must remain independent of source-writing Workers under the captured project policy,
even if the Pipeline omits an independence annotation.

Repository planning fixtures retain committed model Worker contracts and the copied independence
policy. Their verification reserves include each protected Worker's Attempt cost, required checks
and the captured Provider admission costs; total limits also cover mandatory author or implementer
work and its admission. Reviewers may share a model when independence is required only from the
source-writing Worker. Isolated fake Providers answer planning probes without dispatching Workers;
same-model and command-only negative cases must still fail under distinct-model policy.

The plan includes the effective package and every default package whose contract constrained
its replacement. Resume recompiles those captured bytes and settings; it does not read the
local file again. The integration fixture runs Alice and Bob against the same Pipeline,
changes their local files after planning, and verifies both captured executions.

Scenario `template-pin --bindings FILE` uses this same bounded capture from the original file
location. Its version 2 template seals the full native authority closure in the Task Store.
Concurrent identical pins create one durable record; repeating a pin validates that exact record
and refuses conflicting, corrupt or duplicate history, including noncanonical event metadata.
A process-owned shared CAS reservation protects capture and reused objects through publication;
Task collection refuses while it is held. Git and package capture leave SQLite available for
unrelated Task heartbeat renewals. Process death releases the reservation automatically.
Fresh Tasks and restarts restore that closure without reading local paths, even after the
bindings and package directories are deleted. Committed customer Pipelines and policy remain
anchored to the exact authority commit; local additions remain Workers only. A new binding
requires a new template pin and Scenario/policy confirmation. See
[Fresh Task templates](../scenarios.md#fresh-task-templates).

## Report evaluation dependencies

Report verifiers may declare any number of prior `af/ReportEvaluation@1` inputs with
project-chosen names. Each is required, cardinality `one`, and `same_as` the `source` input.
The captured graph binds each to its exact verifier node's `result` port. Admission requires
distinct passed evaluations of the same document, sources, checks, Snapshot and requirements;
missing, stale or substituted evaluations are refused. A verifier with no prior evaluations
remains valid. Each dependency must appear in the Worker's `signature.retains` entry for `result`;
output admission checks that the result envelope retains its exact input artifact ID, so the
accepted report evidence preserves every declared prior evaluation in its lineage.
The consuming package owns its dependency count and independence bindings.

## Afactory operator catalog migration

Afactory's staged [issue-oriented catalog](../../fixtures/issue-workflows/README.md) owns its
nine public selections, exact role definitions and pins. Its launcher admits only personal
account aliases in local bindings; it refuses slot/package replacements for these selections.
This consuming policy leaves the generic native binding capability above available to other
projects. Use the read-only migration inventory before changing machine-local workstream
choices; do not rewrite the captured bindings or package bytes of historical Tasks.
