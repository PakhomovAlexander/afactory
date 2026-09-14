# ADR-0056: Share planning accounting and authenticate generated plan decisions

Status: accepted, 2026-09-11.

## Context

A generated Pipeline is executable authority. A model's proposal must not install operators,
change acceptance or approve itself. Planning also consumes the Task's finite resources before
its business verification exists. Restart must retain both that spend and the exact proposal.

## Decision

Only a persisted semantic no-fit selection with permitted generation enters the fixed engine
bootstrap. Its captured plan-role Worker receives a bounded Task contract and installed public
interfaces through `PlanningRequest@1`, and returns `PipelineProposal@1`. Source payloads,
Worker instructions, Provider credentials and developer keys do not enter that request.

Every preparation plan has typed public ports. It has no business acceptance coverage and cannot
satisfy the Task. The bootstrap and all Planner retries use the common scheduler, Store,
Attempt ledger and budget. Protect the complete future verifier reserve while planning. Permit
at most two Planner Attempts, including one repair from persisted typed compiler feedback.

The proposal contains at most 16 generated Pipeline definitions, under `generated/`, referring
to captured Workers, operators and child Pipelines. The normal compiler enforces contracts,
effects, independence, coverage, 64 expanded nodes and depth four. Generated text cannot shadow
captured packages or invoke the internal bootstrap. Only a selected, published Planner output
can establish the Store proof used to install generated dependencies.

One `PlanningCompleted` transition binds the bootstrap, proposal, normalized next Task revision
and generated execution plan. Preserve the original Task identity, goal, requirements, inputs,
authority and limits; only admitted root constructors may add inputs. Retain all prior
reservations, charges, Attempt identities and the original deadline. Validate execution against
remaining capacity. Scope output reuse and retry feedback to the plan as well as its node.

Persist the exact generated dependency closure and enter `needs-plan-review`. Developer
authorization is a detached minisign signature over canonical, domain-separated bytes binding
the Task revision, plan, policy, developer, decision, reason and expiry. Project authority
captures public verification keys; private signing keys remain outside af and Worker contexts.
No actor label, TTY, environment variable or model reply authenticates a decision.

Approval records a decision and does not execute. Every admission and dispatch rechecks the
signature and Store revocation state. Signed rejection of an approved plan records a retained
revocation proof. New revision, graph, child, Worker, policy, input or budget identities cannot
reuse the approval. A generated child makes the containing execution plan require approval too.

## Consequences

`task plan` remains token-free and may persist a fixed preparation plan. `task start` or `task run`
may execute that admitted bootstrap, then pause at the generated plan. Resume uses captured
packages and selected output; it does not ask the Planner again because the process restarted.
Exhausted planning or a proposal that cannot be admitted produces an inconclusive Task with
retained accounting. Export and reviewed catalog installation are separate operations.

Historical artifact identities remain unchanged where the new optional fields are absent.
The proof and budget handoff are trusted Rust capabilities, never model-deserialized authority.
