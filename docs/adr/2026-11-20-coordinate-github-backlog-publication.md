# ADR-2026-11-20-coordinate-github-backlog-publication: Coordinate GitHub backlog publication

**Status:** proposed (2026-11-20).

## Context

[Native backlog settlement](2026-11-20-settle-and-dispatch-verified-backlog-locally.md)
binds an independently verified assessment to a Project batch and a set of public fields. A
GitHub create can take effect even when the receiving Project cannot commit the response.

## Considered options

- Let the assessment Worker publish. This would combine independent verification with write
  authority and make the private evidence context available to the publisher.
- Retry a create after a timeout or an empty search. GitHub search can lag, so that retry can
  create another issue for the same item.
- Keep the write intent and reconciliation in the native receiver and use a narrow executable
  protocol for provider access.

## Decision

The receiving Project's private batch policy pins the destination configuration. The prepared
builtin Actions bind the Project and that destination. The configuration binds a GitHub
repository name and numeric ID, a numeric publisher ID and the adapter's exact executable
bytes. Credentials enter only the adapter's cleared environment. A Project writer lock
serializes intent admission, transport and receipt commits.

After checking the live policy, cancellation, capacity, verified revision and approved
fields, AF commits a Project publication intent and then an attempt before launching the
adapter. An attempt authorizes at most one create. Recovery from any committed create attempt
uses read-only reconciliation. An empty or conflicting observation leaves the intent pending.
AF requires the exact repository, stable Project and item marker, publisher identity, and any
previously mapped issue identity before recording a receipt. Material revisions get their own
intent and can observe the mapped issue using the same marker. Edits and closure stay with
the human operator. This protocol grants no update, reopen or comment operation.

The native public view continues to expose verified fields and item and revision IDs. It reads
no private intake evidence and makes no provider calls. The synthetic local sink remains
available through its separate receiver destination.

## Consequences

An interruption between the durable attempt and the remote create can leave an item pending
even when no issue exists. Recovery preserves the one-create bound. The batch audit, publication
journal and referenced state retain the reason and attempt history. Installing an adapter and
authorizing live credentials are separate operator actions. The offline Scenario regression
uses an executable fixture and a fake destination.

[Receiver configuration and wire protocol](../project-backlog.md#github-issues-destination)
