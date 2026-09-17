# Self-optimizer design review record

Status: one completed light AF Round; draft revised against all ten findings.
The revised design has deterministic document checks, not a second reviewer verdict.

## Execution

- Engine: installed `af 0.9.0-rc.1`; design foundation: product source `166eca5`.
- Reviewer: `claude-fable-5-1`, effort `medium`, bound to `claude-personal`.
  The native usage receipt confirms canonical model `claude-fable-5-1`.
- Campaign: `self-optimizer-design-20260916-fable51-medium`.
- Task: `review-5ce95a04f448daa44d67fcfdb7d1fadef6b57d747fac3b91fd5e60ad72cd2479`.
- One closed Round. AF reports `fail (exhausted)` and
  `execution=completed`, `acceptance=unsatisfied`,
  `domain_conclusion=review_changes_requested`. The budget was not breached:
  exhausted refers to the one-Round convergence cap with findings remaining.
- Wall-clock, exactly as `af review report` prints it: **3m51s**.
- Findings: **7 major, 3 minor; no blockers or Demands**.
- Gate: required document-structure/conflict-marker check, passed. This was a
  design review, not an implementation build or optimizer execution test.

## Accounting

The canonical Task report records a cumulative charge of **86,178 tokens**:

| Attempt | Chargeable | Input | Output | Cache read | Cache write | Wall |
|---|---:|---:|---:|---:|---:|---|
| Provider admission | 475 | 2 | 4 | 5,504 | 469 | 2s |
| Document gate | 0 | — | — | — | — | 624ms |
| Fable 5.1 reviewer | 85,703 | 194 | 17,456 | 333,630 | 68,053 | 3m47s |

The earlier `claude-fable-5` invocation was refused at Provider admission because
its native response identified `claude-opus-5`; no reviewer ran. It charged
2,502 tokens. After the owner explicitly selected Fable 5.1/medium, new Worker
and candidate authority were captured in the same isolated consumer with new
Campaign state. Across both captures the recorded AF charge is **88,680 tokens**.
Parent-session authoring and local orchestration are outside that number. The
initial default-state filesystem refusal produced no recorded review Task.

## Finding dispositions

These are author dispositions of design changes, not kernel `fixed` resolutions.
The original ledger correctly remains at ten open Findings on the reviewed
Snapshot. No second Campaign was started after its completed light Round.
References below point to sections of the [revised design](self-optimizer.md).

| Finding key prefix | Severity | Finding | Disposition and revision |
|---|---|---|---|
| `531e9ede2c77` | major | `self` bypasses project pins | Accepted. Section 2 keeps the requested command but requires a command-specific dispatch exception, superseding the relevant ADR-0044 clause, with old-default/old-pin refusal and selector fixtures. |
| `0a4394832206` | major | Measured trials cannot fit default Attempts | Accepted. Section 12 distinguishes the deterministic default from measured mode and derives all child bounds under the original parent. The worked example requires at least 412 Attempts rather than 12. |
| `5bfd4e249754` | major | Holdout leaks into diagnosis | Accepted. Sections 4 and 7 seal family membership before diagnosis, expose development-only aggregates and retrieval, reject late splits, and retain exposure across runs. |
| `c02eca624efd` | major | Experimental child transition undefined | Accepted core finding. Section 6 defines a captured bounded slot, `ExperimentPrepared`, a new signed `ExperimentPlanDecision`, and protected child registration, with exact identity, accounting, resume and compatibility rules. The suggested shortcut of treating changed Worker instructions/models as mere data was not adopted: those remain executable authority and require approval. |
| `6388b6c07917` | major | Obligations and delivery mapping undefined | Accepted. Section 4 defines analysis and candidate profiles, fixed obligations, installed receipts, no-change/negative/incomplete mappings, and the explicit adapter needed for verified local delivery. |
| `dd433b353073` | major | Document output implies missing document acceptance | Accepted. Section 4 uses `OptimizationReport@1` with a pure typed Markdown renderer and no document-verification claim. |
| `f64c14787e47` | major | Trial source/configuration composition undefined | Accepted. Section 6 preserves exact historical product source and Requirements, binds arm authority separately, excludes incompatible baseline families, and restricts harness changes to explicitly derived fixture Snapshots in v1. |
| `33757da05924` | minor | Proposed ADRs overstated as accepted foundation | Accepted with precision. Sections 2, 9 and 10 distinguish code present at the target revision from ADR-0081/0104's still-Proposed status and require resolving those decisions before dependent increments ship. |
| `f9d8ac492afe` | minor | Protected paths can enter writable set | Accepted core finding. Section 5 unconditionally denies author writes to policy, keys, lock and protected oracle closure. The trusted finalizer may change only admitted package-pin entries before final sealing. Target project harness files remain editable; prohibiting every harness file would defeat the feature. |
| `20061ab2bcf7` | minor | Incremental capture hides recurrence | Accepted. Section 2 separates incremental collection from retained-chain analysis; the index binds contributing captures and completeness. |

## Subsequent owner requirements

After this review and its dispositions, the owner requested stronger token/time
economics and separate light routine versus heavy full-redesign strategies. The
[design](self-optimizer.md) was extended and a
[four-milestone implementation plan](self-optimizer-plan.md) was added. Those
extensions have local document checks only; the review above does not cover them.
The original review output and accounting remain unchanged.

## Validation and evidence

After revision: Markdown lint, relative-link validation and conflict/whitespace
checks passed. These verify the documents; no optimizer code was implemented and
no new acceptance claim is made about the revised design.

The isolated review consumer and Campaign artifacts are under the workspace's
ignored scratch directory, not part of the product patch. The original AF outputs
are preserved verbatim:

- Captured review plan: `self-optimizer-review51-plan.json` (retained in the local review evidence bundle)
- AF outcome: `self-optimizer-review51-outcome.json` (retained in the local review evidence bundle)
- Canonical AF report with complete finding bodies: `self-optimizer-review51-report.md` (retained in the local review evidence bundle)
- Earlier refused admission: `self-optimizer-review-outcome.json` (retained in the local review evidence bundle)

These local evidence links are machine-specific; portable design consumers use
this disposition record and may separately export authorized review evidence.
No branch was pushed and no PR was opened.
