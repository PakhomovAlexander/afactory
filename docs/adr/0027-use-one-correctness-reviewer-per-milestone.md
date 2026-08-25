# Use one correctness reviewer per milestone

**Status:** accepted (2026-08-25)

M2's two-specialist Campaign spent about four million chargeable tokens across four rounds while
successive architecture and performance reviews continued to open incremental follow-ups. The
review loop became the roadmap's dominant cost and latency even though every milestone must still
receive external CLI dogfood review and the local kernel gate remains mandatory.

We will replace the standard architecture and performance reviewer nodes with one correctness
reviewer using Claude Opus at high effort. The heavy pipeline reserves 300,000 tokens per attempt,
caps a run at 1,000,000 tokens, requires one clean round, and permits at most two rounds. The
reviewer reports concrete correctness defects of every severity and excludes architecture-only
refactoring and performance-only optimization. Every milestone still receives `af review`, and
local `make check` remains the non-negotiable correctness gate.

## Considered options

- **Keep both specialist reviewers and two clean rounds.** Maximizes independent scrutiny.
  Rejected because observed M2 cost and latency stalled the dependency-ordered roadmap.
- **Keep both specialists but lower their effort.** Preserves topic coverage. Rejected because
  two broad prompts still duplicate repository exploration and finding reconciliation.
- **Skip external review on some milestones.** Has the lowest aggregate cost. Rejected because
  the owner requires CLI dogfood review for every milestone.
- **Use one high-effort correctness reviewer (chosen).** Concentrates the external budget on
  behavioral and contract failures while retaining one post-fix verification opportunity.

## Consequences

- Each milestone has one reviewer and at most two external rounds; wall time and aggregate model
  context should fall materially.
- Architecture-only design improvements and performance-only optimizations are no longer part of
  the standard dogfood gate. They require an explicitly requested specialist audit.
- Independent corroboration is lost. The local gate, typed admission, durable prior findings, and
  a possible second verification round remain, but they do not replace a second reviewer.
- Campaign authority remains immutable. Existing Campaigns retain their captured reviewer
  packages and convergence policy; this decision applies to new Campaigns opened against an
  authority revision containing these files.
