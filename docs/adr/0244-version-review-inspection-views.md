# ADR-0244: Version review inspection views

**Status:** proposed (2026-10-27).

## Context

`af review ledger` and `af review show` exposed human text only. Agents had to parse that
text even though the kernel already holds structured Finding and Demand projections. A new
printed document shape needs a version and a schema; the persisted review records and Change
Set must keep their identities.

## Options considered

- Print the persisted Ledger directly. Rejected because it exposes internal reducer state and
  conflates read-only inspection with durable authority.
- Extend the report document to cover ledger and finding detail. Rejected because a report is
  a Campaign summary, while a finding inspection includes its attached reports and proposals.
- Add separate versioned inspection documents. Chosen because each command has one stable
  public result and the persisted contracts remain unchanged.

## Decision

`af review ledger --json` prints `af/review-ledger@1`: Campaign, Round, Finding views, Demand
views and the count of required open or stale Demands. `af review show KEY --json` prints
`af/review-show@1`: the selected Finding, its attached report artifacts and linked proposals.
Both have schemas under `schemas/`. `af review report --json` selects its existing
`af/review-report@4` document; combining `--json` with any `--format` is a usage error.
These are read-only views. No persisted event or canonical artifact changes.

## Consequences

Consumers can use a versioned JSON document and fail closed on a future version. Text output
remains for a person at the terminal. Schema and fixture tests guard the printed shapes.
