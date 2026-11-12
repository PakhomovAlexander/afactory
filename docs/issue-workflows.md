# Issue-oriented operator execution

Use the seven concrete Pipeline selections and staged replacement authority documented in the
[operator catalog and pilot runbook](../fixtures/issue-workflows/README.md). It includes native
accepted diagnosis/design stages, measured performance acceptance, local gates, shared read-only
verification, hard-migration inventory and cumulative issue/PR/Task pilot reconciliation.

The catalog is consuming configuration for Afactory's own work. Installation follows candidate
verification and review; no Task Worker changes `.af/`, machine credentials, captured historical
Tasks or remote references. The
[decision](adr/2026-11-11-issue-oriented-operator-pipelines.md) records the constraints and limits.

Pilot reconciliation binds the manifest issue URL to `requirements.issue_url` in the original
implementation's native structured requirements; research/design preserves it inside the complete
`original_issue` contract. Local unlinked Tasks remain valid outside pilot reconciliation.
Declared PR URLs must name the issue repository and are explicitly reported as unverified;
no remote lookup or external PR linkage verification occurs.
