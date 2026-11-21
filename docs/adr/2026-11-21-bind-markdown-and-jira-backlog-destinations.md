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
- Using a create-only destination entry and a pinned Jira adapter gives both destinations
  read-only reconciliation and preserves human changes.

## Decision

The private Project Store provides each destination's exact configuration. The batch policy pins
its content ID and the prepared builtin grant names the Project and destination. The receiver
uses the same current revision, decision, policy, publication limit and cancellation checks for
all destinations. It persists the intent and attempt before performing an effect.

Markdown publishes one file per stable item ID under the receiver's private Project Store.
The receiver stages and syncs the file, installs it atomically without replacement and syncs
the directory. The marker binds the Project, item and first publication intent. A matching
entry can be observed after an uncertain result. A missing entry, ambiguous marker or
conflicting file remains pending. Reconciliation never edits the file. Checkout and bare
repository paths are outside this authority.

Jira Cloud publication pins the site, numeric project and issue type, publisher account and
exact adapter bytes. The adapter maps approved title and body to Summary and an Atlassian
Document Format Description. It searches a stable Project and item label and verifies the
creator, project, issue type and marker. A committed attempt authorizes at most one create.
A mapped issue is read by stable ID, and the receipt retains the observed display key, closure
and whether the observed fields equal the approved fields. No update or transition is authorized.
Only the adapter receives the separately provisioned credential.

## Consequences

One ambiguous attempt can block a batch while the operator restores scope or inspects the
destination. A missing observation never authorizes another create. The Project journal retains
attempt history and references versioned provider observations. The verified CLI view continues
to read without credentials or provider access.

[Operator runbook](../project-backlog.md#markdown-and-jira-destinations)
