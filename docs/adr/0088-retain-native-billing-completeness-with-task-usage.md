# ADR-0088: Retain native billing completeness with Task usage

Date: 2026-09-12
Status: Accepted (2026-09-23); acceptance recorded in [ADR-0113](0113-ga-reads-only-what-ga-writes.md)

## Context

An exact counter does not establish a complete bill. A native invocation can report valid turns
and then malformed usage, or valid billing counters with invalid optional metadata. Treating an
invalid number as zero undercounts work; discarding the invocation's usage loses earlier paid
work. Charging every failed invocation its entire reservation also overstates a fully reported
failure. These facts must survive output-publication failure and process recovery.

## Decision

Add the closed `af/TaskUsageObservation@1` contract with optional `reported_usage` using
`TaskTokenUsage@3`, and required `charge_complete`. A complete observation requires reported
usage, including an explicitly known zero. The observation describes native reporting; it cannot
grant a reservation, extend a resource limit or authorize another Attempt.

Native Task adapters distinguish absent optional counters from present invalid values. Required
or present counters reject null, strings, negative or fractional numbers and values outside their
native range. Keep every unambiguous contribution and preceding valid turn. Billing completeness
uses the Provider's captured accounting convention: Codex needs valid input, cache discount and
output; Claude needs input, output and any declared cache-creation charge. Malformed nonbilling
metadata still refuses business output without making an otherwise known charge incomplete.

Preserve the existing valid native path and its artifact identities. Malformed reporting adds an
observation and refuses business output. Incomplete billing charges at least the original Attempt
reservation and every known native, owner, Broker or late-usage floor. A fully reported failure
retains its actual charge, subject to already recorded floors. Cancellation cannot erase usage.

Before fallible CAS publication, atomically retain effective cumulative charge and the optional
observation in the Attempt wall sidecar. Later measurements cannot lower a counter or charge;
incompleteness remains sticky for that Attempt. There is no contract for replacing an incomplete
Attempt observation with an authoritative complete bill. Absence preserves prior facts, and a
malformed newest sidecar value refuses interpretation instead of falling back to older data.

Capture the observation as an Attempt-produced artifact bound to the original context. Existing
usage records reference it through `raw_artifact_ids`; Store admission validates its producer,
context, absence of a business Subject, and the charge floor. Recovery reconstructs this evidence
from the sidecar after CAS returns. Existing usage formats and execution event variants remain
unchanged. Raw stream bytes that could not enter CAS are not reconstructed by this contract.

## Considered options

- Defaulting invalid counters to zero turns malformed paid output into a free observation.
- Dropping the whole observation loses prior valid turns and known contributions.
- Charging every failure its reservation confuses failure with incomplete billing.
- Encoding uncertainty in existing frozen usage formats changes historical artifact identities.

## Consequences

Accounting can distinguish an observed native floor from the effective common charge. Provider
admission, ordinary Task Workers and the legacy Review host forward the same observation to the
common runtime. Explicitly incomplete native reporting prevents successful business publication
while retaining the original resource authority. Entirely absent native usage retains the previous
absence/reservation convention; it is never converted into a reported zero. The new artifact adds
evidence, not a second spending ledger.

Focused conformance covers malformed first and later reports, wide retained totals, optional
metadata, known zero, exact failed charges, sidecar reopening and CAS recovery. Full checkpoint
verification, caller cancellation forwarding and the requested specialist reviews are recorded
separately; this decision does not claim those gates have completed.
