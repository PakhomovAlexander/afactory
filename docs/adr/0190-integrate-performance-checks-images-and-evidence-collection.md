# Integrate performance checks, images and evidence collection

Status: accepted, 2026-10-21.

## Context

The continuous laboratory needs an operational SDLC entry point. Its per-run aggregation is
advisory and may have no build or report. Requiring that matrix job would not establish a
performance guarantee. The initial installation also has a trusted base without a laboratory.
Hosted images and collection must exist before calibration can establish readiness.

## Options considered

- Require the existing aggregation job: rejected because a missing build can return success,
  and its matrix name is not a stable check.
- Run candidate scripts when the base lacks the laboratory: rejected because that changes
  the trusted control boundary.
- Keep artifact discovery as an operator-written script: rejected because an undocumented
  manual step can let Actions evidence expire before collection.
- Enable enforcement immediately: rejected because local and synthetic tests do not establish
  hosted readiness or a five-day calibration cohort.

## Decision

Add one stable `Performance check`, always evaluated after planning, building, sampling and
aggregation. It reads the trusted control's versioned enforcement policy. Required mode needs
every upstream job to succeed and the exact primary plan's complete, measured, full-scope
passing verdict under an active authority. Unavailable, missing, cancelled, partial, synthetic
and inconclusive evidence cannot satisfy it. Advisory mode reports that same qualification
without blocking merges. The installation starts advisory. Required policy applies to PR and
main-push checks; manual experiments, calibration, nightly and post-publication release runs
remain advisory so their workflow success is not confused with required-check qualification.

The initial installation reports that the trusted base has no laboratory, without executing
candidate harness code. An activation cannot be installed through that exception.

Publish GNU and musl images in a separate, main-only provisioning workflow. Pin the upstream
Rust image, verify platform and tool versions, verify anonymous pulls of the resulting digest,
and configure measurement jobs only with those immutable references. Registry credentials
are scoped to publication and never enter a measurement job.

Ship a bounded hourly collector over completed runs of the exact performance workflows. It
admits existing transport through the laboratory's archive reader, retains calibration evidence,
applies retention and generates history. The archive records successful imports, so a failed
scan never advances a cursor past missing evidence. Collection is separate from execution and
holds its own credential. Keep its service and archive outside disposable build worktrees.

Before enabling enforcement, retain real hosted evidence, complete the declared calibration
cohort, review the activation and configure owner approval at the exact PR head. Enable
`mode = "required"` in reviewed control and require the stable check only after that transition
is on main. No workflow silently changes branch protection or manufactures readiness.

## Consequences

PRs, main, nightly runs and published releases have one integration path. Release comparison
remains after publication and advisory; it is not a release publication gate. A local collector
requires an awake host and must catch up before artifact expiry. Retained evidence and health
reports distinguish working plumbing from calibrated performance guarantees.

Performance decisions originally numbered 0147–0174 in the unmerged candidate are numbered
0162–0189 on integration, preserving main's existing decisions. The sealed original candidate
and raw historical measurements are unchanged. Historical AF acceptance does not cover the
direct integration changes, authorized after the owner ended AF iteration.
