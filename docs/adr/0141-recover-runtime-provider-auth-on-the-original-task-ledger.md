# ADR-0141: Recover runtime Provider authentication on the original Task ledger

**Status:** accepted (2026-10-06). Completes the runtime stage of issue #122 that
[ADR-0137](0137-permit-provider-logins-through-private-host-capabilities.md) and
[ADR-0139](0139-deliver-native-login-challenges-to-a-verified-private-requester.md) left
separate. Their login, private-delivery, credential and identity fences are unchanged.

## Context

A native Provider can report "authenticated" through its token-free status while real inference
fails with a revoked token, an expired session or a failed refresh. Before this decision such a
failure was an ordinary Attempt failure: the node retried the same broken context until its
allowance was spent, siblings kept dispatching, and the Task finished as failed. The private
login of ADR-0137/0139 could repair the credentials, but nothing joined that login to the Task:
a terminal Task cannot resume, a captured one-Attempt admission has no retry left, and a login
must never mint new budget, plan, deadline or publication authority.

Eight findings from an earlier candidate are binding regressions here: probes must consume the
same Task allowance as live reservations (1); every paid verification rechecks revision, plan,
approval, deadline, principal, executable binding, model and effort (2); an overrunning
acknowledgement verifies nothing (3); a newer failure invalidates an older claimed resume at the
dispatch boundary (4); every still-required context is verified before dispatch (5);
continuation is automatic and crash-safe (6); each participation notifies its own coordinator
(7); and an already-terminal Task continues only through an explicitly linked successor (8).

## Considered options

- **Treat a successful login, or an "authenticated" status, as recovery.** Rejected: the
  defect being fixed is precisely a status that lies. Only a paid inference proves usability.
- **Give verification its own budget.** Rejected (finding 1): a separate allowance double-counts
  headroom. A Task with 100 tokens left could admit two 100-token probes.
- **Keep recovery in a machine-local file beside the Stage 1 session.** Rejected: claims and
  dispatch must be fenced in the same transaction as the Task log, and Task state already lives in
  the Store. The Stage 1 directory stays credential-free and session-scoped.
- **Reopen or rewrite a finished Task.** Rejected (finding 8): terminal history, failed spend and
  acceptance are immutable. A successor is a new Task with an explicit link.
- **Let `af task run` probe a suspended Task implicitly.** Rejected: verification is a separate
  paid decision. `run` reports the suspension and spends nothing.
- **Detect auth failures from diagnostic prose.** Rejected: model output could spoof it. Only the
  adapter's closed native classification, established from protocol or process status, counts.

## Decision

**Classification and suspension.** Native adapters return their closed `NativeFailureKind`
beside the redacted capture. Its authentication classes become the closed
`TaskAuthFailureV1`; quota, model, network and unknown failures have no recovery class and
never suspend or ask for a login. The pre-send identity recheck reports a client that answers "no
signed-in account" as `auth_missing` without sending anything; a changed account stays an
identity failure, never a login request. In the runtime an auth failure settles its Attempt at its
exact charge, stops the node's retry loop and refuses every later dispatch of the run before any
reservation; in-flight siblings settle normally. A contended refresh is the one transient class:
it is retried once within the node's own allowance. Provider admission is one paid probe per run.
After the scheduler returns, and before any result is assembled, the Store records
`af/TaskAuthSuspension@1` and moves the Task to `waiting: needs_provider_auth`. The suspension
binds the Task, revision, plan and original deadline, keeps each failed Attempt's node,
reservation and exact charge as read from the ledger, names every required auth context with the
plan's principal, model, effort and invocation policy, and carries the participation's opaque
requester and coordinator references. A plain `resumed` transition cannot leave this pause.

**Recovery generations.** Each auth context (Provider label, kind, principal) has one append-only
log of `af/ProviderAuthRecoveryEvent@1` rows in the Task Store. A failure joins the open generation
or, after verification, opens the next; concurrent failures of one context share one generation,
one login and one verification. Stage 1's completed private login is joined only by its opaque
recovery ID, as `authenticated`, which verifies nothing. Every recovery row and the Task transition
it accompanies are written in one Store transaction; every Task write that depends on a log
carries that log's exact next sequence and fails if another write raced it.

**Paid verification on the original ledger.** A plan captures a recovery allowance only through
its catalog's `provider_recovery` table, compiled into the graph and so covered by the plan's
approval. Absent, a login cannot create one; an existing capture keeps its exact bytes. The
allowance is a dormant budget node, `recovery.providers.verify`, outside every Pipeline call
scope: each probe reserves, begins and settles on the Task's own `TaskBudget`, so cumulative
probe usage and live reservations consume the same tokens, Attempts and deadline as Workers and
never the protected verification reserve. Each captured probe also lets a verified resume spend
one more Provider admission Attempt. Before reservation and again before dispatch, the Store
rechecks the writer lease, the exact suspended revision and plan, the current developer approval
and authorization, the original deadline, an unbreached ledger and the binding the host resolves
now; any drift blocks with a closed reason and spends nothing. Only an acknowledgement whose exact
charge fits its own reservation records `verified`. An overrun keeps its charge, breaches the
ledger and fences all later dispatch. A probe that still fails authentication tells every
participant of the generation that a private login is required.

**Claims and continuation.** `af task recover` asks each still-unverified context for its
token-free status. "No signed-in account" returns the `login_required` handoff with no paid call;
"authenticated" earns one probe. When every required context is verified under its current
generation, the Store records `af/TaskAuthResumeClaim@1` and returns the Task to running, and the
command continues the same Task through the ordinary runtime. Every later dispatch rechecks the
claimed generations inside its write transaction, so a newer failure recorded by any Task
invalidates the claim before work starts, and the runtime suspends the Task again. A crash after
the claim is finished by continuing it, never by a second claim; a crash after dispatch is
recovered by the ordinary abandoned-Attempt settlement, and selected outputs replay. A lost
writer's probe is settled at its full reservation.

**Notifications.** Every claim, required login, failed verification and block records one outcome
for the suspended participation, routed to that participation's own coordinator reference, never
to whoever ran the login. A coordinator acknowledges a delivery with its own reference;
repetition is idempotent, another coordinator is refused, and an outcome without a private route
is reported as undeliverable, never redirected.

**Terminal predecessors.** `af task continue` opens a successor whose revision carries an
`af/TaskContinuation@1` link with the predecessor's result, plan, original limits, exact charge
and begun Attempts. The Store validates the link when the successor opens: the predecessor is
finished and unsatisfied, the request, acceptance and authority are unchanged, the successor's
limits fit in what the original limits left with no later deadline, and each finished Task is
continued at most once. The successor stops at plan preview and runs only after the usual plan
confirmation. The predecessor's log is never written.

## Consequences

Recovery never logs in, never trusts a login and never reports success without a verified probe.
Documents are `af/task-auth-recovery@1`, and none carries a URL, code, token, native text or
account identity. Exit code 3 means a human or the private host must act. Plans whose catalog has
no `provider_recovery` stay exactly as before; their suspended Tasks report
`allowance_missing`, and a finished one can only be continued by a linked successor. Credential-free
tests cover the Store fences, the runtime and production probe, and the `af` binary with a fake
native client; live proof needs separate host authorization and publishes nothing.
