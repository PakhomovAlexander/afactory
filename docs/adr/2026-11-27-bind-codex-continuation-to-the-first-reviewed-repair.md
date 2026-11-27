# ADR-2026-11-27-bind-codex-continuation-to-the-first-reviewed-repair: Bind Codex continuation to the first reviewed repair

**Status:** proposed (2026-11-27).

## Context

[ADR-0212](0212-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md)
admits the selected candidate and Findings into a new repair Task. A private implementer
transcript requires its own disclosure authority and recovery rules. The accepted research
Document `sha256:297126ca8cb6e89732f30cd21039e2c6fa600ed8db5426231cf059f271fc7056` recommends
an explicit plan opt-in and a fork of an immutable prefix. Its source is Snapshot
`sha256:43ef108512580b5a8c5a1a88d2e1eb5527f733f9c2f61eabc728b2b50ead042f`.

## Options

- A mutable native thread reference could inherit turns added after the original Attempt and
  settings outside the repair plan. Reject it.
- Treating Attempt abandonment as proof of non-dispatch could submit a duplicate paid turn
  after losing an acknowledgement. Reject it.
- Continue from a checked immutable prefix with a durable dispatch fence. Preserve the
  existing source and Findings admission and the independent verification of any repair.

## Proposed decision

An approved producer plan and its explicitly linked first repair opt into a capability-checked
App Server pin. The Store can select a checkpoint only after the producing Attempt has
settled successfully, the typed reply has passed its projected strict schema, and the
selected candidate has been sealed. The checkpoint binds the Attempt, context, sealed
Snapshot, native thread and completed turn, exact immutable history prefix, package,
Provider, principal, model, effort and runtime. The repair plan admits the checkpoint as an
explicit typed input only for the exact reviewed candidate and its selected Findings.
Ordinary Workers still receive fresh invocations.

Materialize the repair source in a new Attempt sandbox. Require conformance from the pinned
runtime that setup and fork are inert, later turns are excluded and the effective policies
permit access only to admitted resources before any tool execution. Use a private native
home and disable candidate-controlled configuration, approval escalation and undeclared
tools. Exercise the read and write boundary with the old sandbox still present. Keep history
in bounded private Store storage. Send the compact current repair request and its projected
reply schema only after checking the pinned account and effective settings.

Commit the Attempt start and the dispatch fence atomically before submitting the exact
turn once. A successor settles accounting under
[ADR-0209](0209-recover-a-lost-or-interrupted-attempt-without-charging-its-reservation.md).
The dispatch fence persists until checked evidence proves either non-dispatch with the
send channel permanently closed or a correlated terminal invocation with tools quiescent.
Ambiguity prevents any new submission. A fresh fallback requires plan authority, safe
reconciliation of any earlier launch and a separate funded reservation. Missing optional
history can use the authorized fresh path. Conflicting required provenance fails admission.
Only the Store's selected result advances the graph. New persisted shapes require current
schemas and replay parity under
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md).

## Consequences

AF checks the recorded Scenario repair inputs. A Task file that explicitly requests
`codex_session_continuation` is refused at planning, including on `af task start --execute`.
The opt-in remains unavailable while transport conformance and dispatch integration are pending.

The Store groundwork uses `af/TaskExecutionRecord@6` and inspection `af/task-inspection@23`.
A committed launch arm begins the common Attempt and identifies the exact submission.
The node stays fenced after writer loss and Attempt settlement. A separate resolution
requires dispatch evidence bound to the launch, thread and nonce. A completed launch can
support a checkpoint only after the producing Attempt's successful output is published and
its exact candidate is sealed. The Store checks the candidate envelope against the producing
Attempt and its admitted source. It also checks the Seal's source envelope, including its
references to the candidate and sealed Snapshot. Inspection shows record identities. The
history bytes and dispatch proof are CAS references.

The repeated token-free probe used Codex CLI 0.169.0, executable
`sha256:b452ca20f838a8855a88a41050ee4a694033fff4ea3ecdad5633e6e0e8d13f2b`,
with a private HOME and CODEX_HOME and no credentials. Over stdio, `initialize` succeeded,
`account/read` returned no account, and `thread/start` reported the requested cwd and
approval policy `never`. Its `workspaceWrite` response had no extra writable roots and left
both temporary-directory exclusions false. The default response schema exposes no
readable-root attestation. A fork of that empty thread returned `no rollout found`.
The experimental schema exposes a named permission profile and its provenance. Effective
read and write restrictions still require conformance testing. A separate local
`command/exec` probe of an old-directory read returned no response within its five-second
bound, so it provided no isolation evidence.

The generated default protocol declares `thread/fork.lastTurnId`, paginated
`thread/turns/list` and `thread/items/list`, and `turn/start.outputSchema` and
`clientUserMessageId`. The request catalog has no lookup by submission nonce. Evidence of
quiescence or definitive non-dispatch still needs verification.

A follow-up credential-free probe used the same executable with disposable parent and child
directories. After `thread/inject_items` added synthetic user and assistant messages, an
unbounded fork reported the child cwd and the synthetic user message as its preview. The
parent's `thread/turns/list` response was empty. A fork specifying a nonexistent `lastTurnId`
returned `turn not found`. This exercised fork setup and explicit boundary rejection. The
empty turn list alongside the inherited preview leaves completeness of the model-visible
prefix unverified. The public turn listing alone cannot attest that prefix. A separate
`command/exec` read of the still-present old directory's canary again returned no response
within the probe's eight-second bound and supplied no isolation evidence. This worker
environment refused even a local model endpoint when opening a loopback socket returned
`Operation not permitted`.

A separate probe configured the Provider to use an unavailable loopback endpoint with zero
request and stream retries. The server acknowledged `turn/start` with a strict output schema
and a synthetic `clientUserMessageId`. The probe saw error notifications and received no
`turn/completed` within 15 seconds. After closing stdio and starting a new App Server over the
same disposable native home, `thread/turns/list` returned the acknowledged turn with status
`interrupted` and the exact synthetic value as the user item's `clientId`. This establishes
correlation in one interrupted-turn restart. It supplies no evidence of tool quiescence,
definitive non-dispatch, or correlation for a successful producer turn. The probes made zero
paid calls and obtained zero successful model turns.

The immediately preceding review completed with two major findings against the unconditional
planning refusal. The following obligations remain open.

- Dispatch and record the opted-in producer and first repair with the pinned principal,
  model, effort and strict reply contract.
- Capture the immutable native prefix and wire checkpoint selection into the real producer
  and repair paths. Verify omission of later turns and bind the prefix to the current selected
  Snapshot and admitted Findings.
- Verify effective isolation in a new sandbox with the old sandbox present. Check the
  append-only repair turn and typed result under the same pin.
- Wire the launch fence and correlation checks to the App Server sender and its recovery
  path. Prove quiescence and definitive non-dispatch for the runtime pin, fund eligible fresh
  fallback separately and verify single selection through the normal Task entry points.
- Exercise the real feature in both arms with independent fix verification. The sample
  contains zero paired repairs. The proposed pilot is one matched pagination repair pair
  using the same App Server pin, candidate and Findings, with separate sandboxes and
  allowances. Record cached and uncached input, output, elapsed time, repeated reads,
  fallback costs and independently verified outcomes. One pair would provide feasibility
  evidence under its observed cache conditions and run order.
