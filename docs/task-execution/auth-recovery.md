# Provider auth recovery

A Task whose native Provider stops authenticating while it runs is suspended, verified on its
own ledger and continued, never failed for a login problem and never resumed on a login's word
([ADR-0141](../adr/0141-recover-runtime-provider-auth-on-the-original-task-ledger.md)). The
private login itself is the Stage 1 host flow in [provider-auth-host.md](../provider-auth-host.md).

## What suspends a Task

Only a closed native authentication class does: `auth_missing`, `auth_revoked`, `auth_expired`,
`auth_refresh_contended`, `auth_refresh_failed` or `auth_rejected`. The adapter derives it from
the native protocol or process status and replaces the failed capture with one non-secret
summary before CAS publication. A pre-send identity recheck that finds no signed-in account is
`auth_missing`, and nothing is sent. Quota, model, network and unclassified failures keep their
ordinary retry and failure handling and never ask for a login. A different signed-in account is an
identity failure, not a login request.

When one Attempt fails authentication:

1. The Attempt settles at its exact charge; nothing is refunded.
2. The node stops retrying. A contended refresh is retried once within the node's own allowance;
   Provider admission is one paid probe per run.
3. Every later dispatch of the run is refused before it reserves anything. Work already running
   settles normally.
4. Before any result is assembled, the Task records `af/TaskAuthSuspension@1` and waits as
   `needs_provider_auth` (`af task show` and `af task run` exit 3). A plain resume cannot leave
   this pause, and `af task run` spends nothing on a suspended Task.

## Capturing a recovery allowance

Verification is paid, so the approved plan must carry its allowance. Add a `provider_recovery`
table beside `provider_admission` in `.af/task-catalog.toml`:

```toml
[provider_admission]
tokens = 4096
wall_ms = 45000

[provider_recovery]
probes = 2
tokens_per_probe = 4096
wall_ms_per_probe = 45000
```

The compiler installs it in the plan's graph (`auth_recovery`), so plan confirmation and developer
approval cover it. Probes reserve on the Task's own budget node `recovery.providers.verify`: they
consume the same tokens, Attempts and deadline as Workers, never the protected verification
reserve and never a Pipeline call's Attempt bound. Each probe also allows one more Provider
admission Attempt after a verified resume. A catalog without the table captures nothing, and an
earlier capture keeps its exact bytes: its suspended Tasks report `blocked` with
`allowance_missing`, and a login cannot add an allowance.

## Recovering a suspended Task

```text
af task recover TASK_ID --json
```

For each still-unverified auth context, in key order:

- A token-free status that reports no signed-in account returns `login_required`, with no paid
  call. The coordinator's private host completes the official login
  ([provider-auth-host.md](../provider-auth-host.md)); the human only uses their browser.
- An authenticated status earns one bounded probe. Before it reserves and again before it starts,
  the Store rechecks the writer lease, the exact suspended revision and plan, the current
  approval and authorization, the original deadline, an unbreached ledger, and the principal,
  model, effort and invocation policy the machine would dispatch with now. Any drift is `blocked`
  with a closed reason and spends nothing.
- Only an acknowledgement whose exact charge fits its reservation verifies the context. An
  overrun keeps its charge, breaches the ledger and blocks the Task (`budget_breached`). A probe
  that still fails authentication returns `login_required` for every Task sharing the context.

When every required context is verified, the Store records `af/TaskAuthResumeClaim@1` and the
command continues the same Task through the ordinary runtime, under its original plan, limits and
deadline. Concurrent failures of one Provider context share one generation, one login and one
verification. A newer failure recorded by any Task invalidates an older claim at the next dispatch,
and the Task suspends again. A recovery interrupted after its claim is finished by running
`af task recover` again; it never claims twice or reruns finished work.

`--login-ref RECOVERY_ID` joins a completed Stage 1 login (`authenticated_unverified`) to the
open generation. It is recorded as unverified; verification is still required.

## Coordinator notifications

`af task start` and `af task run` take `--requester-ref REF --coordinator-ref REF`, opaque host
references recorded with the suspension. Each outcome (`login_required`, `verification_failed`,
`blocked`, `resumed`) is listed once under `notifications` for that participation's own
coordinator, never for whoever ran the login. After an accepted private send, acknowledge it:

```text
af task recover TASK_ID --acknowledge SEQUENCE --context CONTEXT_KEY \
  --coordinator-ref REF --delivery-ref MESSAGE_REF --json
```

Repeating an acknowledgement is idempotent; another coordinator's reference is refused. An
outcome without a recorded route is counted as `undeliverable` and never redirected.

## The `af/task-auth-recovery@1` document

```json
{
  "schema": "af/task-auth-recovery@1",
  "task_id": "release-notes",
  "state": "login_required",
  "contexts": [{"context_key": "sha256:…", "provider": "codex-personal",
    "provider_kind": "codex", "generation": 1, "status": "failed", "login_recorded": false}],
  "accounting": {"chargeable_tokens": "10", "verification_tokens": "5", "begun_attempts": 2,
    "limit_tokens": 40000, "deadline_unix_ms": 1900000000000},
  "notifications": [{"context_key": "sha256:…", "outcome_sequence": 2, "generation": 1,
    "outcome": "login_required", "requester_ref": "user-1", "coordinator_ref": "chat-1"}],
  "undeliverable": 0,
  "login": [{"provider": "codex-personal", "provider_kind": "codex"}],
  "exit_code": 3
}
```

`state` is `suspended`, `login_required`, `verification_failed`, `blocked` (with `reason`),
`resumed`, `not_suspended` or `terminal` (with `result` and `continuation`). Exit code 3 means
the private host or a human must act; 0 means nothing is pending. The schema is
[`task-auth-recovery-v1`](../../schemas/task-auth-recovery-v1.json). No document carries a URL,
code, token, native text or account identity.

## Continuing a finished Task

A Task that already finished, for example on an authentication failure before recovery existed,
is never reopened. Continue it with an explicitly linked successor:

```text
af task continue TASK_ID --task-id TASK_ID-2 --confirm-result RESULT_ID --json
af task run TASK_ID-2 --confirm-plan PLAN_ID --json
```

The successor keeps the predecessor's request, acceptance and authority and carries an
`af/TaskContinuation@1` link with its result, plan, original limits and exact charge. Its limits
are what the original limits left, with the original deadline. Each finished Task is continued
at most once, and the successor's plan needs the same confirmation as any captured plan.

## Live proof

Credential-free tests cover every path: the Store fences
(`crates/review-store/src/store/task/tests/auth_recovery.rs`), the runtime with the production
probe (`crates/review-pipeline/tests/it/task_runtime/auth_recovery.rs`) and the `af` binary with
a fake native client (`crates/af/tests/it/task_document/auth_recovery.rs`). A live proof against
a real Provider is a separate, explicitly authorized operator action: revoke a disposable
session, run a Task with a captured `provider_recovery`, complete the private login and run
`af task recover`. Keep only the `af/task-auth-recovery@1` documents, which hold no credential.
