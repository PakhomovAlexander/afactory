# Implementation through human merge

A Pipeline describes one Task's execution and typed acceptance. The reusable `impl` Scenario
orchestrates native Task outcomes, bounded repairs, owned candidate preparation, scoped PR publication, completed CI,
actual human merge and owned cleanup. Models run only inside Tasks. An independently pinned verification Task must precede preparation. There is no automatic merge or scheduler installation.

`scripts/scenario-impl.py` is an offline authoring recipe. It creates packages in a consuming
repository, using the existing native package manifests and `af catalog digest`; it installs
nothing into Afactory's catalog. Supply the team's runners, Provider bindings, Attempt budgets
and native independence policy in a JSON file:

```json
{
  "workers": {
    "implementer": {
      "runner": {"kind": "model", "provider_kind": "codex", "model": "YOUR_MODEL", "effort": "medium"},
      "provider": "YOUR_CONFIGURED_PROVIDER",
      "attempt": {"tokens": 100000, "wall_ms": 900000}
    }
  },
  "independence": {
    "command_workers_by_package": true,
    "distinct_principals": false,
    "distinct_providers": false,
    "distinct_models": true
  }
}
```

The [complete inert shape](../fixtures/scenarios/impl/team.example.json) supplies all four roles
with placeholder models and no live bindings. The abbreviated example above must be completed with `correctness`, `bugs` and `evaluator` entries of
the same shape. Choose each runner and reservation explicitly; command runners use the native
`command` object and zero tokens. Personal accounts, models and effort choices belong in the
consumer's workstream configuration. Both reviewers are required and independent of the
implementer; the evaluator is also independent. Native admission enforces the supplied independence
policy. Distinct principals, providers or models can be required by the consuming policy.

From the consumer repository (output must be a new directory immediately below
`.af/task-packages/`):

```sh
python3 -I /path/to/afactory/scripts/scenario-impl.py packages \
  --output .af/task-packages/my-team --namespace my-team \
  --af /private/tools/af --team /private/workstream/team.json --check project-gate
```

Review the generated `catalog.toml`, merge its entries into the project's catalog, and commit
the packages and policy together. Keep the project's local checks under `.af/code-policy.toml`.
The generated catalog is a complete minimal example, not permission to overwrite existing
configuration. Recompute pins after any package edit. No personal pipeline is shipped in AF.

The generated implementation Pipeline routes optional typed continuation `findings` to the
implementer. `af task run NEW --from OLD` uses the consumer's `[task.defaults]` Pipeline and
carries native findings and candidate provenance. The focused regression actually plans and
executes this continuation with inert Workers; a bundle's structure alone proves no such support.
The correctness reviewer has `read-source` and `execute-checks`, returns required exact-Snapshot
`ImplementationTestEvidence@1`, and must build, privately install or stage the candidate in the
project's appropriate isolated test environment, and execute meaningful tests and acceptance cases.
The evaluator receives that evidence, local checks, requirements and the reduced Review Round
through explicit typed edges. Failed or unavailable execution cannot become passing acceptance.

Author a native implement Task using `my-team/implementation`, `verification: "review"`,
`facts: {"standard": true}` and adequate Task/verification limits for the team's declared
reservations. Review the exact native plan before execution. Pin the committed Task template
with `af scenario template-pin` as described in [Scenarios](scenarios.md), then build the bundle:

```sh
python3 -I /path/to/afactory/scripts/scenario-impl.py bundle \
  --pin template-pin.json --verification-pin verifier-template-pin.json \
  --destination github:OWNER/REPOSITORY \
  --title 'Implement the requested change' --body-file pr-body.md \
  --repairs 2 --owner owner --output impl-bundle.json
```

The recipe accepts any reviewed implement template; inspect its pinned package contracts before
granting machine authority. The Scenario name is `impl`; its Pipeline name is configurable.
Template pinning, policy installation and exact Run-pin confirmation remain operator-controlled.
The default recipe requires owner-admitted startup scope. `--signed-publication` explicitly
selects the generic recipe with a fresh signed Decision for each publication.
Use the separately installed [Git workspace capability](scenario-git-workspace.md) under adapter
name `workspace`, granting only its preparation, publication, observation and cleanup operations
at the exact destination. Its exclusive owned writer restriction and all publication,
lease, recovery and owned-resource checks remain binding. The
[executable-review guide](task-execution/executable-review.md) explains evidence limits.

`--repairs` permits zero, one or two fresh implementation repairs in total. Ordinary native
check failures and reviewer/evaluator changes requested can select a repair from exact terminal
`task_outcome` evidence. Native continuation binds the reviewed candidate and its findings; typed
requirements preserve the terminal result, check receipt and accounting. Missing continuation,
unknown usage, exhaustion, cancellation and Provider/preflight failures do not authorize repair.
Accepted `task` receipts remain distinct; unsuccessful outcomes cannot prepare or publish.

Supply the required `--verification-pin verifier-template-pin.json` for an independent whole-change
verification Task before every preparation. Pin its explicit original base (the implementation template authority commit), read-only native
Pipeline and independent Worker bindings. It consumes total Task resources but no repair slot.
A verifier rejection and a later failed repair gate each consume one edge of the same remaining
repair allowance. The recipe never resets it in a nested branch. Every branch's actual template
and effective Provider pool must pass startup admission before any paid work.

Completed failing CI still selects a fresh repair with the accepted predecessor, exact observation
and preparation receipt. Pending, missing or unknown CI never selects repair. Scoped publication
uses the admitted startup capability for first and same-PR publication without a later Decision. Successful checks precede a
separate actual `merged` Wait and cleanup; closed-unmerged PRs cannot complete. Finite total Task,
token, elapsed-time and dispatch bounds derive from the complete generated tree. Scoped Runs also
check the shared implementation reservation count on every new Task reservation, across restart.
Generic pending observations consume dispatch allowance. Scoped idle Wait polls use the retained
driver cadence without a new dispatch reservation.

Remote review comments and base/conflict bytes remain unavailable as automatic repair inputs.
Human review and
merge remain external actions; CI readiness proves neither. Handle conflicts through a separately
captured source and explicit operator action. Task Stores are retained; cleanup removes only the
workspace capability's explicitly owned resources after its independent actual-merge readback.

Run the focused native tests without live services or models:

```sh
cargo nextest run --workspace --locked --profile ci \
  -E 'binary_id(af::it) & test(/scenario::generated_impl/)'
```

The focused generated recipe tests use private homes, command Workers and an inert workspace
outcome service. They distinguish package pins/planning and another configured team without execution from executed
native continuation with actual findings delivery, two consecutive gate failures, two review rejections, review rejection followed by repair gate failure, independent verifier rejection followed by repair gate failure and an accepted repair, terminal failed gates, native check-preflight blocking, Attempt-wall exhaustion with its partial candidate, native command-output admission refusal, and zero/one/two CI repairs with fresh
signatures, lazy skipped receipts, same-PR identity, separate merge waiting and cleanup. This
outcome fixture models the lifecycle; it does not prove production adapter composition.

The installed production adapter regression
`production_workspace_scenario_repairs_same_pr_and_cleans_only_after_merge` independently
exercises that boundary with real Git commits, worktrees and bare remote refs, real accepted
command-backed Tasks and native signed Decisions. Only external HTTPS/GitHub transport is mocked.
It covers ready and pending CI, stale-head refusal, completed failing CI admitting a fresh repair
with predecessor/source provenance, a second signature and the same PR with exactly one creation
POST. An actual local merge precedes removal of owned worktrees, build roots and local/remote
refs; the merged base remains. It uses the reusable capabilities directly, separately from the
generated recipe fixture.

See [generic capability coverage](scenario-git-workspace.md#recovery-and-bounds) for production
transport/recovery, dirty/foreign cleanup refusals and the ordinary native repair regression,
including `python3 -I scripts/test-scenario-git-workspace.py`. None of these tests claims live
publication.

## Scoped startup admission (Stage A)

An owner-only machine policy may set `implementation_startup` to an
`af/implementation-startup-scope@2` object. Review its complete recipe digest (`hash(bundle)`),
exact template IDs and effective Worker model pool, native repository/source,
GitHub repository numeric ID and actor ID/login, branch prefix/base, exact grants, finite Run
limits, repair count and expiry before confirming the ordinary Scenario pin. Changing any of
these fields changes that pin. This capability is separate from performance standing authority.
The public schema is `schemas/implementation-startup-scope-v2.json`. Pass
`af scenario start --require-startup-scope` to refuse an accidentally absent scope before
Run creation or Task dispatch. A policy that carries scope always enforces it.

Startup and each new foreground driver process validate every declared template, including
conditional repair branches, against captured native authority before dispatching work. Preflight also creates or
reuses deterministic, never-executed
`startup-<template digest>` native plans against the captured source, proving native contracts,
resource feasibility and check configuration without inference. Future predecessor content is
still validated by native input resolution when the actual Task is planned. Command Workers
and remote gates are refused. Local replacement Workers and child slot mappings resolve to the
effective pool; unused
replacements are refused by native compilation. Provider probes are token-free;
normal Task admission and acceptance still apply. The installed workspace adapter must support
startup preflight and pass pinned configuration/tool, local/remote base and GitHub identity checks.
Unavailable credentials or prerequisites refuse admission without including credential bytes.
Scoped creation, elapsed budgets and expiry use live Host time; `--now` cannot extend authorization.
Each dispatch is rechecked after
native planning and its supervised timeout is bounded by the remaining scope lifetime. The
watcher requests cancellation on expiry. Cancel the Run to stop dispatch.

The scope is retained inside the Run's confirmed immutable pin. Replay validates its recipe and
scope identity without probing credentials, so expired evidence remains inspectable. Standalone resume
performs live preflight again; one foreground process reuses that startup admission. No historical Store or plan is rewritten on upgrade.

Stage A tests exercise conditional-template refusal, exact recipe/source/budget/model pools,
missing machine preflight support, live creation/elapsed budgets, expiry before and during Task planning, cancellation, retired
versions, native zero-dispatch refusal, and admitted personal native plans reused with zero
Attempts and no inference. Workspace preflight uses a fake read-only GitHub transport. They do not establish
live GitHub permissions, VM reboot survival or continuous service operation. Stage B adds the
terminal evidence and repair paths above. Stage C1 below adds scoped publication; continuous
driving is described below; full original-issue integration remains separate. Existing Task wake support
does not implement a Scenario driver.
See [the startup ADR](adr/2026-11-16-admit-implementation-startup-scope.md).

See the [Stage B decision](adr/2026-11-16-route-terminal-native-outcomes-to-bounded-repairs.md).
The command fixtures use zero-token Workers and prove exact zero native charges, retained Task
identities and no redispatch on terminal resume. They do not establish nonzero paid-provider
charges or production whole-change verification/publication composition. The retained driver
regressions below use separate admitted-provider fixtures for those paths.

A separate coordinator fixture settles four Tasks at 17 tokens each and restarts at the shared
repair boundary. It proves 68-token accounting and that verification/report reservations do not
spend repair slots. These are synthetic coordinator settlements, separate from the executed native
command Workers' zero charges. Unknown-usage and cancellation boundary fixtures make no paid-provider
or physical interruption claim.

## Scoped native publication (Stage C1)

`publish_implementation_pr_v1` is a separate capability. Install its workspace adapter with
`implementation_state` naming the private native Scenario Store, and admit its exact grant in
`implementation_startup`. Confirming the complete pin authorizes that bounded scope; it retains
no signing key. Generic `publish_pr`, `followup` and performance publication retain their existing
contracts. The recipe passes both the accepted implementation and independent verifier receipts,
and the exact preparation receipt. The verifier template must carry the same original goal and
requirements and an explicit original baseline commit. The live destination base is checked
separately; the candidate cannot serve as its own verification baseline.

The native `af scenario implementation-proof --file REQUEST --state STORE --json` reader binds
the exact intent and input digest to its retained Run, pin, admitted template pool and native
Task plans/results. It opens accepted native Tasks, candidate SourceTree artifacts, complete
independent Review Rounds, evaluator evidence and original baseline provenance. Missing, foreign,
unaccepted or narrowed evidence refuses. Its proof includes the native Task report for the PR.
`--live-only` rechecks current authorization without repeating immutable acceptance validation.
An immutable proof alone does not authorize a send.

After blocking observations, the production adapter rechecks the exact actor/repository, local
and remote source/base, then native active Run, cancellation, current intent, budgets and trusted
expiry immediately before each push, POST or PATCH. HTTP connects before this guard, disables
implicit reconnection and recomputes remaining timeout afterward. Git checks before spawning.
Same-PR repairs retain the original marker and update the exact PR body with the new native
Task report. A retained metadata dispatch is never repeated merely because readback still shows
old content. Conflicting same-head PRs, including another base, refuse. Unknown effects remain
uncertain until the exact retained journal reconciles them.

The native scoped fixture uses admitted Codex bindings with deterministic Provider IO, production
proof/workspace/publication code, real disposable Git refs and controlled GitHub IO. It exercises
first publication, a CI repair to the same PR, lost publication responses and retained recovery.
It supplies no live publication or VM-reboot evidence. The retained driver tests below add default
cadence and CLI process restart evidence; mid-Worker termination and original-issue integration
remain separate. See the [C1 decision](adr/2026-11-16-publish-implementation-under-native-startup-scope.md).

## Retained foreground driver

Every newly admitted Run retains a driver schedule. Start with the normal confirmed pin;
`--poll-ms` selects the immutable idle cadence (default `30000`). The complete effective tree
must allow two polls strictly inside each scoped publication deadline, including conditional
repair branches. An incompatible cadence is refused before `start --execute` can dispatch a
Task. The generated scoped recipe gives publication an immutable `120000` ms deadline.

```sh
af scenario start implementation --file impl.json --authority policy.json \
  --inputs inputs.json --confirm-pin SHA256 --require-startup-scope
af scenario drive implementation --state /absolute/private/scenario-store --json
af scenario driver-status implementation --state /absolute/private/scenario-store --json
```

The second command is the foreground service. A supervisor restarts the same command, Store and
Run ID after disconnect or process loss; no service is installed automatically. Cancellation is
`af scenario cancel implementation --reason 'owner stop' --state STORE`. The service observes
that retained cancellation by its next due wake. Human merge remains external.

The two typed publication checkpoints are native proof checked and owned preparation checked.
Each binds the exact retained intent and inputs. The pinned adapter journals it before returning;
the runtime retains the frontier, and a single Store transaction acknowledges it while consuming
the next resume reservation. Genuine progress can therefore run immediately. Replayed progress,
unchanged pending CI, idle waits, missing responses and uncertainty cannot earn credits. Scoped
standalone `resume` and `drive --once` obey the same schedule; `--now` cannot backdate it.

There are at most 512 consumed resume reservations, including immediate progress, idle polls and
calls lost to a crash. Killing the CLI does not refund a reservation or reset any Run/step/scope
clock. A Run left running still waits for its actual due reservation when no new checkpoint is
available. The driver retains terminal blockers for exhausted allowance and elapsed/expiry or
other runtime boundaries. Corrupt journal bytes fail closed and remain available for diagnosis;
no replacement evidence is manufactured. Ordinary native code failures keep the existing fresh,
bounded repair routing. Repeated scoped Wait polls no longer consume dispatch allowance.

The complete startup pool is probed once per foreground service process. Native per-Task admission
and final push/POST/PATCH fresh guards remain live after blocking observations. No signing key,
new publication authority or merge capability is introduced. See the
[driver decision](adr/2026-11-16-drive-scoped-implementation-with-retained-progress.md).

The focused native driver tests use the generated scoped recipe, admitted `codex-personal`
bindings with controlled Provider IO, real native Task/Attempt receipts, independent whole-goal
verification, disposable local Git and controlled GitHub API IO. Separate Runs preserve the
original repair bounds and publication deadlines when exercising code failures or interruption.
They compare Scenario charges to distinct native settled Attempt receipts, including provider
probe charges and zero-token checks. They do not establish paid live-provider behavior, external
GitHub publication, physical VM reboot or mid-Worker termination. The original issue's final
integration and independent base verification remain separate operator steps.
