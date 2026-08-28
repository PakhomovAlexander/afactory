# Surface partial results without granting Ledger authority

**Status:** accepted (2026-08-28)

A required reviewer may publish an admitted `ReviewerResult` before a sibling reviewer becomes
unavailable. The scheduler then correctly suppresses gather and Ledger nodes with
`UpstreamMissing`, but the ordinary operator views currently make the admitted result invisible.
An absent Ledger can therefore look like a produced, clean Ledger even though durable findings
remain available in the event log and CAS.

When a Round is incomplete before its authoritative reducer runs, Afactory projects admitted
reviewer results as **recorded, not gathered** evidence. The projection carries node ID, Attempt
ID, result artifact ID, severities, and charged spend. The deterministic diagnostic projection
distinguishes a Ledger produced clean, produced with Findings, or not produced because required
upstream output was missing. Ordinary text and machine-readable output adds this state only for
the absent-Ledger case, keeping fully gathered Campaign presentation stable.

Recorded, not gathered evidence is never a Ledger, Finding Set, or convergence input. It cannot
satisfy Semantic Closure, contribute a clean Round, or support a merge-ready verdict. Fully
gathered Campaign presentation remains unchanged apart from an explicit Ledger-state label.

## Considered options

- **Keep partial results visible only in raw events and CAS.** Rejected because ordinary reports
  then erase the operational distinction between no findings and no authoritative reduction.
- **Fold available sibling results into the Ledger despite missing required input.** Rejected
  because it weakens the gather barrier, creates a policy-dependent partial Ledger, and could let
  incomplete review masquerade as convergence authority.
- **Persist another authoritative partial Ledger type.** Rejected because the admitted result,
  Attempt provenance, and Run Report already contain the immutable authority needed for a
  deterministic diagnostic projection.
- **Project admitted results under an explicitly non-converged label (chosen).** This preserves
  evidence and operator visibility without changing the fail-closed verdict or reducer contract.

## Consequences

- `af review report` and machine-readable review output expose durable partial reviewer results
  and the absent-Ledger state when the latest Ledger reducer did not complete.
- `af review ledger` labels an absent latest-Round Ledger instead of rendering silence as a clean
  zero-Finding Ledger.
- A partial result reader must validate the referenced result artifact and retain its exact
  Attempt provenance; corrupt authority fails closed rather than fabricating findings.
- Issue [#15](https://github.com/PakhomovAlexander/afactory/issues/15) is part of M4.5 scope.
