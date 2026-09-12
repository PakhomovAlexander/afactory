# Model bindings for Task files

Task files accept captured command Workers and native Claude/Codex Model Workers. A Model
Worker's manifest declares its Provider family, versioned model ID and effort. The catalog
maps its package name to a machine-local Provider registry label:

```toml
[providers]
"team/implementer" = "claude-personal"
"team/reviewer" = "codex-personal"
```

The registry contains authentication directory selectors. Credentials and those directories
are not copied into shared packages or execution plans. The native identity adapters support
first-party Claude subscription accounts and Codex ChatGPT accounts exposing an email identity.
Unavailable identity or unsupported authentication types fail admission. An alias or directory
name never proves independence.

Planning probes only accounts used by the selected Pipeline. It obtains account metadata from
`claude auth status --json` or Codex's local `account/read` protocol, then records a
domain-separated principal digest. Raw account metadata and email addresses are not stored in
Task artifacts. Provider family, principal, explicit model/effort and invocation policy become
part of the exact binding. Bare model-family aliases and `-latest` selectors are refused.

Planning performs no model inference. The compiler adds a visible internal capability node
for each distinct effective Model binding and invocation policy. Slots with the same capability
share that node. Each reserves **4,096 tokens, one Attempt and 45 seconds** inside the Task's
limits. Admission serving a required verifier is protected alongside that verifier. Pipeline
and Task limits must have room for these Attempts; children do not create another allowance.

```text
account identity probe       planning: no model call
         |
   exact compiled plan
         |
 normal Task admission      generated plans still require developer approval
         |
 Provider capability probe  durable reservation -> start -> charge -> settle
         |
   passed receipt
      /       \
  Worker A   Worker B        same native binding, isolated invocations
```

The capability probe receives only the installed acknowledgement prompt in an empty working
directory. Its exact prompt and context manifest are persisted before dispatch. It uses the
same native adapter as downstream Workers. Failure retains reported usage and blocks their
dispatch. Unknown usage is conservatively charged under the common runtime's existing rule.
Successful receipts survive replay without another paid probe.

Before a planned Task resumes, the adapter checks the current account and recompiles against
the recorded binding. Account, model, package or policy changes cannot silently alter an
admitted plan. Finished Tasks remain inspectable without Provider calls.

Every native invocation also rechecks that exact local account, executable path and
authentication context before sending private input. The token-free check shares the remaining
Attempt deadline and cancellation control. Unavailable or changed identity refuses with zero
new charge while preserving earlier admission and spend. Credentials can still change between
the check and their consumption by the native client; this is not an atomic session guarantee.
See [ADR-0090](../adr/0090-recheck-native-task-provider-identity-before-private-invocation.md).

Native Task usage retains exact cumulative components and charge across multiple turns, even
when their totals exceed u64. Output decoding, timeout, unavailable CAS or a refused final-message
file cannot erase observed usage. The original Task budget still applies; an overrun never
authorizes another call. Wider values use additive usage/provenance and Review presentation
contracts, while representable values retain their previous encoding. See
[ADR-0085](../adr/0085-retain-exact-native-task-usage-across-multiple-turns.md).

Malformed native usage preserves known contributions in `TaskUsageObservation@1`. Its billing
completeness is separate from optional metadata validity. Incomplete billing refuses business
output and retains at least the original reservation and known charge floor; a fully reported
failure retains its exact charge. The sidecar preserves both facts before CAS publication and
through recovery. Valid native calls retain their previous artifact identities. See
[ADR-0088](../adr/0088-retain-native-billing-completeness-with-task-usage.md).

The native adapter's controlled invocation boundary can stop an owned process group and retain
cancelled usage through bounded output draining. Unsupported adapters refuse a supplied control.
Common CLI Task, planning, Review and doctor execution share that control with their writer
heartbeat. A failed exact-writer lease check interrupts supervised Workers, commands, Gates and
Integration checks; the runtime retains paid observations and blocks later work and selection.
This does not install CLI signal handlers or a domain-level Task cancellation command. See
[ADR-0087](../adr/0087-control-native-task-invocations-through-the-shared-supervisor.md) and
[ADR-0089](../adr/0089-interrupt-task-work-when-its-writer-heartbeat-fails.md).

Codex final-message capture reads a bounded regular file through the held private output
directory. Symlinks, FIFOs and other nonregular files refuse without blocking or falling back
to a different message; an absent or empty regular file retains the existing event-message
fallback. These checks preserve observed usage on refusal.

Deterministic native-CLI fixtures prove token-free planning, shared admission, account-change
refusal, typed output and replay. One fixture reports 36 synthetic usage tokens across a probe
and two reviewer calls in one Task; this is stub evidence, not a live-model measurement. Live
containment and supported-environment probes remain required before release.
