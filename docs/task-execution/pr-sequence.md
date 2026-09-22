# How the Task increment is structured

The Task runtime was delivered as one increment of numbered packages. The package numbers are an
acceptance checklist, not separate releases: each package's contracts are covered by the linked
walkthroughs and decision records, and later packages extend earlier evidence without rewriting
earlier fixtures or treating a generic Task completion as review approval.

| Package | Scope | Reference |
|---|---|---|
| P00 | Unchanged-source baseline gate and frozen fixture identities | Retired at GA ([ADR-0113](../adr/0113-ga-reads-only-what-ga-writes.md)) |
| P01 | Versioned Task/Pipeline contracts, schemas and parity fixtures | [ADR-0046](../adr/0046-add-versioned-task-contracts-with-exact-plan-approval.md), [ADR-0047](../adr/0047-preserve-task-wire-identity-and-review-completeness.md) |
| P02–P03 | Common Store lifecycle, developer decisions and typed compilation | [ADR-0048](../adr/0048-compile-task-ports-and-fence-developer-plan-decisions.md) |
| P04–P06 | Worker execution through durable Attempts, Task-file CLI, local delivery and Review Tasks | [Task file](task-file.md), [Review Tasks](review-task.md), [ADR-0049](../adr/0049-run-task-workers-through-shared-durable-attempts.md), [ADR-0050](../adr/0050-reduce-review-tasks-with-the-canonical-domain-ledger.md) |
| P07–P09 | Local bindings, Git catalog sync, Task-kind packages, embedded Review, bounded repair and heavy continuation | [Local bindings](local-bindings.md), [Shared catalogs](shared-catalogs.md), [Embedded Review](embedded-review.md), [Bounded repair](bounded-repair.md), [Heavy Review](heavy-review.md) |
| P10 | Captured Pipeline selection before generation | [Selection](selection.md), [ADR-0055](../adr/0055-select-captured-pipelines-before-generation.md) |
| P11 | Bounded generation, shared planning accounting and signed developer decisions | [Generated plans](generated-plans.md), [ADR-0056](../adr/0056-share-planning-accounting-and-authenticate-generated-plan-decisions.md) |
| P12 | Portable export, contract tests and the starter catalog | [Export](export.md), [Starters](starters.md), [ADR-0057](../adr/0057-export-portable-task-definitions-without-execution-authority.md), [ADR-0060](../adr/0060-generate-working-starters-from-supported-contracts.md) |
| P13 | Document Tasks, issue capture and refresh, Campaign Review commands | [Documents](document.md), [Issues](issues.md), [Campaign Review](campaign-review.md) |
| P14 | Consumer compatibility and release | [ADR-0045](../adr/0045-one-release-train-and-a-pin-that-binds-bytes.md) |

## Compatibility obligations

Compatibility obligations start at the GA release; state written by a 0.x release is unsupported.
See [ADR-0113](../adr/0113-ga-reads-only-what-ga-writes.md).
