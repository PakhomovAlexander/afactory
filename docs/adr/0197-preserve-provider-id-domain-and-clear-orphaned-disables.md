# ADR-0197: Preserve the Provider ID domain and clear orphaned disables

**Status:** proposed (2026-10-23). Amends
[ADR-0196](0196-disable-providers-until-a-machine-local-expiry.md).

## Context

The Provider registry accepts IDs longer than 64 characters. The new disable sidecar imposed a
different limit, preventing an otherwise valid registered Provider from being suspended. Removed
Providers could also retain nonexpired entries in the bounded sidecar: requiring registration to
enable them made its 32-entry capacity impossible to recover before expiry. Unix timestamp
diagnostics required operators to convert the deadline themselves.

## Decision

Use the registry's established safe ID alphabet and reserved labels without a separate ID length
cap. Keep the 32-entry limit and apply the existing 64 KiB aggregate file bound on both reads and
publication. Reject an oversized candidate before preparing or publishing a replacement. The
sidecar and status schemas admit long IDs consistently with the writers. Their typed shapes and
version identities remain unchanged; these corrections align validation with the established
registry domain, without introducing a compatibility reader.

Disable still requires registration. Enable accepts either a registered ID or an ID present in
the validated sidecar, including an expired entry or one whose registry entry has been removed.
Check that condition against the state read under the existing update lock, before pruning
expired entries. An ID in neither place is refused clearly, without changing state. Clearing an
orphan never restores registration, starts a Provider process or changes credentials. Once
cleared, an unregistered ID is unknown and another enable is refused. Private
path validation, locking, atomic publication and fail-closed malformed-state handling stay binding.

Human command, status and refusal diagnostics render the deadline as a UTC calendar date and
time, reusing the existing Gregorian formatter with an entry point accepting Unix seconds.
Retain any nonzero nanosecond fraction in the displayed timestamp. Persist the exact Unix
seconds and nanoseconds as before; formatting does not affect expiry comparisons or accounting.

Document `distinct_providers` as comparing Provider kinds (adapter families such as `claude`
and `codex`), matching the existing trusted independence predicate. Its semantics and the
authorized model-diverse bootstrap policy remain unchanged.

## Considered options

- Limit IDs to 64 characters in the registry too: rejected because established valid IDs must
  remain usable, and the aggregate bound already limits state size.
- Automatically delete all unregistered IDs on every update: rejected because cleanup should
  name the operator's intended ID; a later registration may reuse another retained ID.
- Allow enable for every safe ID: rejected because a misspelling should be refused clearly.
- Change persisted timestamps to formatted strings: rejected because a presentation correction
  needs no new typed shape or loss of exact seconds and nanoseconds.

## Consequences

Long registered IDs support the full disable/status/enable lifecycle. Operators can recover
full sidecar capacity after removing a Provider, without waiting for expiry or editing private
state. Focused lifecycle, schema parity, aggregate-bound, UTC-format and captured-Task tests
cover these corrections alongside the existing hostile-path, concurrency and exact-expiry tests.
