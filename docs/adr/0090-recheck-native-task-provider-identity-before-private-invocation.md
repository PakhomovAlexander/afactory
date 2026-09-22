# ADR-0090: Recheck native Task Provider identity before private invocation

Date: 2026-09-13
Status: Accepted (2026-09-23); acceptance recorded in [ADR-0113](0113-ga-reads-only-what-ga-writes.md)

## Context

A captured native binding contains a Provider principal, but the native client reads credentials
from a mutable local profile. Initial planning or admission does not establish that the account
is still current for a later Worker in the same process. Reusing the captured binding alone can
therefore send private input under an account that differs from the recorded principal.

## Decision

Wrap native Task adapters at the common CLI Provider factory. Before each private invocation,
recheck the same configured Provider, canonical authentication directory, absolute executable
path, captured environment and exact principal/authentication method. Use the existing fixed
Claude status or Codex account protocol. Do not resolve another binding, adopt changed local
configuration, replan, or create another paid capability-probe Attempt.

The account check and native invocation share the remaining original Attempt deadline and
the same optional cancellation control. Existing probe byte limits and process cleanup remain.
Failure refuses before private input is sent, with known-zero usage for that invocation; prior
admission and paid usage remain intact. Raw account responses, email addresses and detailed
local authentication diagnostics are neither Worker evidence nor ordinary error output.

Keep model/effort settings, credential mode, native sandbox flags and usage accounting
unchanged. Install the wrapper through the factory shared by Task files, planning,
Review and doctor. Finished inspection still needs no Provider call.

## Considered options

- Checking only on resume misses an account change between Workers within one CLI command.
- Silently adopting the new principal invalidates the captured binding and input authority.
- A new paid capability probe would spend resources without addressing the local identity gap.
- Copying credentials or claiming an atomic session would require different native-client
  capabilities and a separate credential-lifecycle design.

## Consequences

Each native invocation pays the bounded wall time of an additional token-free identity check.
An unavailable check refuses work under the original budget. Deterministic CLI fixtures cover
changes after admission and between Workers, preserved prior spend, doctor failure and normal
admission reuse. Source transport separately proves cancellation of a live owned process group.

This detects account drift before the check. Credentials may still change between the check
and their consumption by the native client; this is not an atomic credential-session guarantee.
Executable identity remains its resolved absolute path, not a newly captured content digest.
