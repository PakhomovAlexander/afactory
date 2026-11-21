# ADR-2026-11-21-bind-markdown-and-jira-backlog-destinations: Bind Markdown and Jira backlog destinations

**Status:** proposed (2026-11-21).

## Context

Native settlement binds an independent verdict to an exact revision and its approved title and
body. The receiving Project owns the verified publication journal and chooses which destination
can receive those fields. Markdown and Jira need stable identities and recovery after an
interrupted write.

## Considered options

- Editing a shared Markdown document would require a concurrent compare-and-swap protocol with
  every editor. Committing that document would also need repository publication authority.
- Retrying Jira creation after a missing response or search result could duplicate a ticket
  while its search index catches up.
- Storing one private file per item makes publication inaccessible from the team's chosen
  backlog document.
- Appending to a receiver-selected unversioned file preserves surrounding prose without
  replacing the document. A pinned Jira adapter provides scoped issue creation and observation.

## Decision

The private Project Store provides each destination's exact configuration. The batch policy pins
its content ID and the prepared builtin grant names the Project and destination. The receiver
uses the same current revision, decision, policy, publication limit and cancellation checks for
all destinations. It persists the intent and attempt before performing an effect.

Markdown pins the Project, unversioned-file scope and absolute path in receiver-owned
configuration. The portable Scenario carries the destination digest. The receiver resolves
the path without symlinks and refuses checkout and bare repository paths. Repository
publication requires separate native authority over the repository, ref, path and expected
revision.

The receiver locks the file, validates existing managed entries and checks the exact file
snapshot and identity immediately before one append. The entry binds the Project, stable
item, first intent and approved content digest. A synced append can be observed after a
lost receipt. Reconciliation accepts an exact matching entry and preserves human changes.
Edited, missing or ambiguous entries remain pending. AF never replaces the document.
Concurrent editors can coordinate with the file lock. A confirmed conflict before the
append can retry within the existing attempt bounds. An uncertain append may only be
observed.

Jira Cloud publication pins the site, numeric project and issue type, publisher account and
exact adapter bytes. The adapter maps approved title and body to Summary and an Atlassian
Document Format Description. It searches a stable Project and item label and verifies the
creator, project, issue type and marker. A committed attempt authorizes at most one create.
A mapped issue is read by stable ID, and the receipt retains the observed display key, closure
and whether the observed fields equal the approved fields. No update or transition is authorized.
Only the adapter receives the separately provisioned credential.

## Consequences

One ambiguous attempt can block a batch while the operator restores scope or inspects the
destination. A missing observation after a potentially committed write never authorizes another create. The Project journal retains
attempt history and references versioned provider observations. The verified CLI view continues
to read without credentials or provider access.

[Operator runbook](../project-backlog.md#markdown-and-jira-destinations)
