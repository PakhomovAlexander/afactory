# ADR-0199: Disable Providers until a machine-local expiry

**Status:** proposed (2026-10-23). Extends
[ADR-0146](0146-refuse-a-provider-cli-that-does-not-accept-afs-invocation.md) with the v3
status document; its capability checks remain binding. Amended by
[ADR-0200](0200-preserve-provider-id-domain-and-clear-orphaned-disables.md) for long IDs,
orphan cleanup and human-readable UTC deadlines.

## Context

An operator needs to stop using one registered Provider for a fixed time without removing its
registry entry, changing credentials or rebinding captured Tasks. Registry v1 remains the
cross-release bootstrap contract of ADR-0111. Captured Task identity alone cannot authorize a
private send after the operator disables that Provider.

## Decision

`af provider disable ID --for DURATION` and `af provider enable ID` are deterministic,
token-free machine-local bootstrap commands, exempt from project pin dispatch like setup and
remove. Disable requires a registered ID; enable also accepts an ID retained in disable state
after registry removal. Neither probes its account, model or executable. Durations
are positive integers suffixed `s`, `m`, `h` or `d`, bounded to 30 days with checked arithmetic.
Disable replaces the ID's deadline; enable removes it immediately and is idempotent for
registered IDs. After orphan cleanup, the ID is unknown and another enable is refused.

Keep registry v1 unchanged. Store `af.provider-disables/1` separately in
`$XDG_CONFIG_HOME/af/provider-disables-v1.json`, falling back to
`$HOME/.config/af/provider-disables-v1.json`, independently of `AF_PROVIDERS_FILE`. The closed
shape records at most 32 unique IDs and absolute Unix seconds plus nanoseconds, under the
existing 64 KiB aggregate file bound, without a separate ID length limit.
A Provider is disabled exactly while the wall clock is less than its deadline. Every lookup
reads the file afresh; expiry needs no daemon, process memory, restart or cleanup. Clock changes
therefore affect the remaining suspension. Explicit updates reclaim expired entries.

Use the existing private-directory validation, descriptor-bound lock and bounded no-follow file
reads. Serialize updates, prepare and sync a private temporary file, and publish through one
handle-relative atomic rename. Reject malformed versions, duplicate IDs, unsafe paths, hard-linked
or non-private state and timestamp overflow. Readers fail closed on unsafe or changing state.
An absent state file means no suspension; an unreadable file never does. Post-publication sync
errors are warnings because the update is already visible.

Check suspension before status, authentication, catalog and capability probes, Task admission,
and every captured or resumed Task private send. The Task recheck refuses before an account
probe and keeps its zero-charge refusal and existing cumulative accounting. Recheck immediately
before native invocation too. Calls admitted before a concurrent disable may finish; disable
neither kills them nor revokes bytes already sent. This is a local admission control, not a
credential revocation or a promise that an older pinned release honors the new state.

The changed status shape is `af/provider-status@3`: disabled rows carry an exact
`disabled_until` timestamp and `enable_command`, unprobed authentication, unusable status and no
usage claims. Current writers and schemas move together; v2 is no longer written or read.
No Task artifact or event changes shape.

Independence remains governed by the trusted catalog's existing scoped booleans. Preserve the
operator-authorized `distinct_principals = false`, `distinct_models = true` bootstrap and distinct
Worker packages and fresh isolated sessions. It lasts until the operator changes trusted policy;
it has no implicit two-day timeout and grants no general runtime bypass.

## Considered options

- Add fields to registry v1: rejected because older pinned readers must keep reading the same
  registry contract.
- Remove the Provider: rejected because it loses the binding and requires manual restoration.
- Cache the deadline or schedule a re-enable process: rejected because restarts and downtime
  would change behavior, and every private send needs current machine-local authority.
- Cancel running calls: rejected because suspension cannot unsend private context and does not
  own a cross-process cancellation protocol.
- Relax all independence checks: rejected because the existing principal and model booleans
  express the authorized temporary policy without weakening unrelated contracts.

## Consequences

A two-day suspension expires automatically across restarts. Status remains useful without
starting the disabled harness. Captured identity cannot bypass a later suspension. Supported
older binaries still read registry v1 safely, but operators must run a release implementing this
control for execution; normal project pin dispatch continues to select that release.
