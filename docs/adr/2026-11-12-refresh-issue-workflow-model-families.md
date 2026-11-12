# ADR-2026-11-12-refresh-issue-workflow-model-families: Refresh public operator routes

Status: proposed
Date: 2026-11-12

## Context

The staged issue catalog uses superseded Sol6 judges and treats design as an implementation
route. The owner requires nine public selections, Kimi light bugfix by default, independent
latest-family judges and a design artifact reusable without automatic implementation. This
revises the proposed issue-oriented catalog; captured historical authority remains immutable.

## Options

- Upgrade every Sol judge to Sol6.1: rejected because Sol-written candidates require distinct
  models for verification. Terra high bugs and correctness preserve independence from the
  source writer while retaining separate Workers and prompts.
- Prompt a verifier to imagine sibling challenges: rejected because missing data must fail closed.
- Add a combined report/code runtime: rejected; existing Report and Scenario composition suffice.
- Add two required typed evaluation ports to a final design verifier: selected, with native
  provenance admission and a new `af/DesignAcceptanceInput@1` package input contract.

## Decision

Stage exactly the nine public selections listed in `fixtures/issue-workflows/selections.json`.
Remove `impl/design-codex` without an alias. Change only staged authority, never source `.af`.
Use Sol6.1, Terra6.1, Astra6 and ordinary Kimi3.1 Pro on personal Providers. Keep all Attempt
caps, reserves, fast kernel/Markdown checks and approved policy bytes unchanged. The public design
successor retains the retired public design route's 32-Attempt cap (the smaller internal report
cap cannot hold four model admissions and four model roles); each Worker retains two Attempts.

Design is report-only: Astra medium authors one sealed Document; Terra high and Kimi high
independently challenge it using existing `af/ReportEvaluation@1` outputs. Sol medium accepts
that same original Document with both required evaluations. Runtime admission authenticates
both exact graph producers, checks/document/requirements/Snapshot identities, distinct evaluation
IDs and passed outcomes. All three verification Workers remain independent of the design author;
final acceptance also declares independence from both challengers. Existing persisted shapes do
not change. Report acceptance's evaluation provenance retains both challenges. Default
implementation optionally consumes the accepted design through native accepted-Document binding;
composition and reconciliation preserve original issue/source/producer/accounting checks.

Small known bottlenecks use Kimi research, independent Terra verification, accepted research and
the existing real release-build experiment graph. Sol medium implements; Terra high bugs and
Terra high correctness review; Astra medium evaluates the real comparison. The broad uncertain
research route retains Sol high research. No broader workload or measured improvement is claimed.

New launcher planning/composition reads native token-free Provider catalogs and compares stable
numeric versions within each selected family. Ordinary Kimi Pro excludes highspeed variants.
Superseded, unavailable or ambiguous selections refuse; no pin is rewritten. Historical
inspection/reconciliation and lower-level native planning remain available without this guard.

## Consequences

Root installs exact staged packages/catalog/settings only after independent verification and
review. Model freshness may refuse new plans until versioned authority is deliberately refreshed.
Three-field native defaults select Kimi light; launcher scope attestations still refuse sensitive
or unknown-cause light use. Light implementation instead requires specified requirements and
deterministic acceptance. No execution, credential change, remote publication or paid experiment
is authorized by catalog discovery or accepted research.
