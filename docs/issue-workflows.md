# Issue-oriented operator execution

Use the nine normal Pipeline selections and the isolated `bugfix/primitive-codex` experiment and staged replacement authority documented in the
[operator catalog and pilot runbook](../fixtures/issue-workflows/README.md). It includes native
accepted diagnosis/design stages, measured performance acceptance, local gates, shared read-only
verification, hard-migration inventory and cumulative issue/PR/Task pilot reconciliation.

The catalog is consuming configuration for Afactory's own work. Installation follows candidate
verification and review; changes to installed `.af/` require explicit Task scope or root operator
authority. Machine credentials, captured historical Tasks and remote references remain separate. The
[decision](adr/2026-11-12-refresh-issue-workflow-model-families.md) records the constraints and limits.

Pilot reconciliation binds the manifest issue URL to `requirements.issue_url` in the original
implementation's native structured requirements; research/design preserves it inside the complete
`original_issue` contract. Local unlinked Tasks remain valid outside pilot reconciliation.
Declared PR URLs must name the issue repository and are explicitly reported as unverified;
no remote lookup or external PR linkage verification occurs.

`design/astra-kimi` ends at accepted reusable design evidence; later default implementation is
an explicit separate composition. New launcher plans/compositions check latest available models
per family through token-free native catalogs. Historical inspection and lower-level native
planning retain their documented scope; no discovered model silently replaces a captured pin.

The primitive experiment requires its separately captured experiment catalog. Normal installed
authority retains `distinct_models = true` and refuses its same-model implementer/bug reviewer
pairing. The runbook gives a reproducible private authority procedure; it grants no execution.
