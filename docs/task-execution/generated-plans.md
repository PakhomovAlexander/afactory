# Generate and approve a Task plan

Generation starts only after [selection](selection.md) proves that no captured Pipeline fits,
and the Task or project permits generation. Unknown facts, unavailable capabilities, ambiguity
and insufficient resources retain their separate refusal outcomes.

```text
"Implement this Jira ticket"
             |
     capture Task + catalog
             |
     find an existing fit -------- yes ------> execute captured Pipeline
             |
       semantic no-fit
             |
     fixed Planner bootstrap
       | bounded public interfaces
       | same Task budget; verifier reserve protected
       v
     proposed Pipeline TOML
             |
     normal compiler -------- rejected -----> one bounded compiler-feedback retry
             |
     selected proposal + generated closure + remaining-budget check
             |
     atomic PlanningCompleted
             |
       NEEDS PLAN REVIEW
             |
     exact signed developer approval
             |
     run: recheck approval and captured authority
             |
       implement -> embedded Review -> independent acceptance
```

An embedded generated child is part of the same approved closure. The bootstrap, compiler
repair, implementation and verification all consume the original Task's allowance. A transition
cannot reset spent tokens, Attempt counts or the deadline. Intermediate artifact production
does not change the approved plan.

## Configure the fixed Planner

In the captured Task catalog, select a captured Worker and developer public keys:

```toml
[planner]
worker = "team/planner"
max_attempts = 2

[developers]
schema = "af.task-developers/1"

[developers.keys]
owner = "<developer minisign public key>"
```

The Planner Worker implements the installed data-only contract: role `plan`, input protocol
`af/PlannerInput@1`, input port `request: af/PlanningRequest@1`, and output port
`proposal: af/PipelineProposal@1`. Both ports are required, single and unbound. The proposal
retains the request. No effects or business evidence are permitted. Its captured package also
declares the usual input/output schemas, explicit runner and bounded Attempt cost.

The request contains the Task contract, known facts, budget, allowed effects and captured
operator/Worker/Pipeline interfaces. Its serialized metadata is capped at 64 KiB. The generic
Worker context limit and model token reservation still apply. It does not include source file
contents, package instructions, Provider secrets or developer keys.

The proposal carries exact TOML by generated package name. Limits are 16 definitions, 256 KiB
per definition, 1 MiB total, 64 expanded nodes and depth four. It can reference installed
packages and propose nested Pipelines. It cannot install Workers or operators, overwrite a
captured package, or declare the internal Planner operation. Invalid TOML or contract/coverage
errors become typed feedback for at most one repair. Compiler feedback includes the exact
rejected proposal and bounded compiler diagnostics as durable Attempt inputs.

## Inspect, sign and execute

```sh
af task plan --file ticket.json --json
af task run pagination-cli --json
af task explain pagination-cli --json
```

The first command captures the fixed preparation plan without starting an Attempt. Running it
persists a generated plan and returns `phase: {kind: waiting, reason: needs_plan_review}`.
`task start --file ticket.json` performs both steps and pauses at the same boundary.
Inspection includes exact source inputs, expanded calls, effective bindings, allowed effects,
acceptance coverage, limits, proposal origin, charged tokens and Attempt history.

Create bytes for an exact decision, sign externally with the trusted developer key, then submit
the payload and detached signature. Afactory never imports the private key:

```sh
af task decision-payload pagination-cli --developer owner --decision approved \
  --reason "Reviewed generated stages, child contracts and verification" \
  --output approval.payload
# Sign approval.payload with your existing minisign key, producing approval.minisig.
af task approve pagination-cli --payload approval.payload --signature approval.minisig
af task run pagination-cli --json
```

The payload uses an absent destination and binds exact Task revision, plan, policy, developer,
decision, reason and validity bounded by the original Task deadline. Approval alone starts no
work. Identical approval submission records one decision. Running rechecks authentication,
revocation, original deadline and every captured dependency before dispatch.

For rejection, create a payload with `--decision rejected`, sign it and submit with `task reject`.
Rejecting an approved plan records authenticated revocation. Developer writes use the common
writer lease; a running CLI retains that lease until it releases it or recovery fences it.
A rejected or revoked plan cannot be reapproved through the existing decision record.

## Results and recovery

| State | Exit | Behavior |
|---|---:|---|
| Fixed preparation planned | 0 | Zero Planner or business Attempts |
| Generated plan waiting for review | 0 | Proposal and exact plan persisted; no generated nodes dispatched |
| Approval recorded | 0 | Execution remains a separate command |
| Missing, invalid, expired or rejected authorization | 1 | Admission refused; no generated Attempt |
| Planning attempts exhausted | 4 | `planning_incomplete`, inconclusive acceptance, all usage retained |
| Proposed execution cannot fit remaining resources/capabilities | 4 | `planning_admission_failed`, retained proposal and diagnostic |
| Business execution completes | Domain exit | Existing acceptance and Review result semantics apply |

Restart restores the captured bootstrap and proposal closure, including deleted or edited live
catalog files. Successful selected work is reused; compiler feedback from an old plan cannot
leak into a new node with the same name. A Store test crosses the planning barrier with actual
charges and proves late Planner usage still updates the original ledger. CLI fixtures cover
nested generation, one compiler repair, full binding-independence validation, self-approval
refusal, insufficient remaining Attempts, signed approval, rejection, revocation and process-boundary
replay. A generated implementation composes the shared Review and adds only its declared history
constructor while preserving original inputs and limits.

These are deterministic fixtures and make no live-model performance claim. See [export](export.md),
[starters](starters.md) and [issue sources](issues.md) for the surrounding flows.
