# ADR-2026-11-04-align-pi-light-current-input-contracts: Align Pi-light with current Task inputs

**Status:** proposed (2026-11-04); accepted on merge.

## Context

The merged Pi-light family retained retired Review and Implementation input identities while
its child pipelines and the current kernel require version 2. Its closed Worker schemas also
required an optional review requirements port and refused Snapshot-bearing predecessor artifacts
on some unbound ports. Catalog capture alone does not exercise those rendered input envelopes.

## Considered options

- Change only type strings. Rejected because closed schemas and package pins would still
  disagree with the native input interface.
- Add compatibility aliases or relax validation. Rejected because ADR-0145 requires current
  contracts and fail-closed admission.
- Adopt the current interfaces and regenerate pins together. Selected because it preserves
  exact package capture and existing runtime authority.

## Decision

Version the six Pi-light packages to 1.1.0. Use `af/ReviewInput@2` and
`af/ImplementationInput@2` consistently in Workers and all child slots. Require only declared
mandatory input ports. Permit the native optional Snapshot ID on unbound artifact envelopes,
while preserving required Snapshot IDs on source and same-source ports, closed objects, exact
artifact types and the optional typed Diff Subject. Recompute every changed catalog pin with
its package bytes. Keep the existing staged native contract fixtures checked without rewriting
unaffected expectations.

Add an integration regression exercising the shipped schemas against both predecessor envelopes
and absent optional inputs, including negative checks for unknown fields, wrong artifact types
and invalid or missing required Snapshot IDs. The existing complete-catalog, staged-contract,
Pi-family compilation and complete Scenario loop regressions remain required.

## Consequences

Pi provider/model policy, budgets, pipeline stages, owner performance roles and strict V3
Scenario admission remain unchanged. No legacy reader, publication authority or adapter behavior
is introduced. The generated harness inventory must match the final protected source.
