# Silence is not a Drop

**Status:** accepted (2026-08-20)

A required reviewer must explicitly dispose every prior Finding assigned to it on the current
Subject: corroborate/re-report it, mark it `not_reproduced`, or dispute it with a reason. Omission
is incomplete work, not evidence that the defect disappeared. Each selected disposition is an
immutable artifact bound to reviewer, Round, and Subject; trusted policy, not reviewer prose,
decides its effect on resolution.

## Considered options

- **Continue interpreting omission as Drop.** Rejected because oversight, truncation, and an
  intentional clean result become indistinguishable for each individual prior claim.
- **Require only the original reporter to answer.** Rejected as a universal rule because a
  pipeline may deliberately assign independent corroboration; assignment policy determines which
  reviewers owe a disposition.
- **Explicit dispositions for assigned claims (chosen).** It makes coverage machine-checkable and
  preserves the distinction between no defect, no opinion, and an active disagreement.

## Consequences

- Reviewer inputs identify exactly which prior Findings require a disposition.
- A result may contain zero new Reports and still be complete only when every assigned prior
  Finding has an explicit disposition.
- Missing dispositions suppress convergence and are shown in `RunReport@2` as structured missing
  semantic output.
