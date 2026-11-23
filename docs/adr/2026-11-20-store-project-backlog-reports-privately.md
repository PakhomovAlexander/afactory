# ADR-2026-11-20-store-project-backlog-reports-privately: Store Project backlog Reports privately

**Status:** proposed (2026-11-20), accepted on reviewed merge.

## Context

A Workspace user needs to capture what they observed and resume work before anyone decides
whether the observation should become a ticket. Workspaces already share a repository-derived
Project Store under [ADR-0158](0158-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md).
Project ancestry can identify the Project even across forks. Ticket-write permission and
receipt by the designated authority require separate admission.

## Considered options

- Immediate issue creation couples intake to provider availability and discloses raw evidence
  before verification.
- Workspace-local files split the inbox by checkout and provide no shared transaction for
  duplicate submission or recovery.
- A Project Store transaction with private Report streams keeps the fact and its evidence
  together, and lets the verified view replay a separate stream.
- The Task Wake outbox provides a useful commit-before-notify pattern. Its OS account and
  original-session fences do not identify a producer on another machine. A versioned signed
  file exchange gives the Project that binding without adding a service or live destination.

## Decision

1. Report identity is the Project ID paired with a stable producer-supplied Report ID. The
   CLI can generate an independent random ID when the producer supplies none. The canonical
   checkout determines the Workspace. The resolved Git commit determines the source revision.
   Task and Run IDs are explicitly unknown when trusted provenance is unavailable. The first
   durable capture time survives a retry. Changes to fact, evidence, Workspace or provenance
   under the same ID are a conflict. Identical facts under different IDs remain distinct.
2. Each Report and its bounded UTF-8 text evidence commit together in a single Project Store
   event. Every evidence value carries AF's opaque blob content identity. Collection retains the
   Project Store event database and counts its bytes in the machine Storage Budget. Survey CAS
   collection has no evidence object to unlink. Pending Reports, holds and publication mappings
   remain reachable through their streams. Each acknowledgement distinguishes durable local
   capture from the designated authority's receipt.
3. The BacklogRecordStored@2 payload family defines the versioned Report, Item Revision,
   Batch, Decision and Publication records. A stable item ID identifies the item across
   revisions. The canonical payload content ID identifies each immutable revision, batch,
   decision and publication record. A batch binds frozen Report membership and an authority
   sequence range to exact Project, authority, Scenario and policy identities. Decisions bind
   disposition, reasons, Task results and the exact revision and evidence. A publication record
   binds the item, revision and verifying decision to a destination, durable write intent,
   attempts and observed provider state or receipt. Intent identity stays stable through retries.
4. The machine account owning the Project Store is the private read boundary. Inbox and
   show --private explicitly select that surface. Ordinary list and show read only
   processing records and present only the stable item and revision IDs and the approved title
   and body. A revision becomes visible after a recorded verified decision naming independent
   verification of that exact revision. Supersession or a subsequent hold removes eligibility.
   Duplicate immutable records cannot roll that newer state back. Raw evidence and decision
   reasons stay private. Any later publisher must use the exact verified fields.
5. An independently configured, machine-local policy names one authority public key per
   Project. The opaque blob identity of its decoded Minisign public key is the authority ID.
   Every processed batch must name that configured identity. Private Report inspection can
   follow validated decisions to the linked Reports and show dispositions and reasons.
6. Local Report streams form the durable producer outbox. A signed submission binds the Project,
   producer, authority and complete Report. The authority checks its enrollment policy and
   evidence content identities, then commits the Report and signed receipt as one sequenced
   Store event. The receipt covers the exact Report, Project and producer. The producer removes
   the Report from its pending view after committing the verified receipt. Replay by an enrolled
   producer returns the original committed receipt. A conflicting payload or producer cannot
   replace a Report. Enrollment has no private read or publication effect.
7. Capture and intake open no Provider, run no Scenario, and perform no external writes.
   Operators transfer files through a private channel. Installed release dispatch still checks
   pinned bytes. Backlog commands skip on-demand installation and background update checks.

## Consequences

The owner can use the offline inbox from either worktree after a lost acknowledgement.
The verified view starts empty until a later authority workflow records independent
verification. A disconnected producer can resume transfer after restart or a lost authority
acknowledgment. Batch execution, portable Scenario packaging, publication and cutover require
subsequent Tasks.
