# ADR-2026-11-13-persist-checked-task-wake-pages: Persist checked Task wake pages

**Status:** proposed (2026-11-13); internal candidate implementation, not host acceptance.

## Context

The first [wake foundation](2026-11-13-project-checked-task-wake-foundations.md) could mistake
maintenance leases for resumed execution and refused every history longer than its 4096-row
scan allowance. Issue #765 also needs a Store transaction for extracted envelopes before any
external recipient can be enabled. This continuation changes source Snapshot
`sha256:cc17c6fc96503e32ab9a4adc0ae05f952965f7a4755be3eba542717339e41809`.
The accepted research Document is
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`,
from source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`.
Its recommendation remains common Store subscriptions/outbox plus authenticated original-session
bridges, with separate receipt stages and no blind repeat after uncertain native admission.

## Considered options

- Increase the history allowance: rejected; valid histories would still eventually stop advancing.
- Let any lease reopen a notification boundary: rejected; storage sweeps take maintenance leases.
- Trust a supplied semantic checkpoint: rejected; source and checkpoint provenance must be checked.
- Put notification envelopes in CAS or canonical Task events: rejected; notification retention
  must neither alter execution authority nor accidentally retain the entire source closure.
- Persist bounded control rows in the existing Store database: selected for transactional extraction
  and serialization with collection without adding a directory outside storage accounting.

## Decision

Keep CLI state rendering unchanged. When a stopped or waiting wake boundary already exists in an
episode, a lease without an execution marker does not reset that boundary. Explicit resumes and
later execution markers still establish later eligible boundaries. The canonical Task fold is
shared with incremental wake replay, including its reference and artifact validation.

Extraction examines at most 4096 new events per page, with the existing 1 MiB row and 16 MiB
buffer ceilings. It returns an opaque `af/task-wake-checkpoint@1` with exact configuration,
source fingerprint, checked boundary, prior semantics, lease cause and cumulative observation
count. A page boundary alone never observes lease expiry. A retained checkpoint is admitted only
from its exact Store row; updates/deletions to its covered canonical history invalidate it in the
same SQLite transaction. An external checkpoint is reconstructed and compared using checked
pages. Canonical Task/CAS validation still applies; this does not promise constant-time replay
of an arbitrarily long Task after restart.

Explicit owner admission allocates a Store incarnation, bound to the private database's Linux machine identity, owner,
device and inode. Reopening and same-filesystem relocation preserve the identity; a copied or
restored database cannot activate independently. Cross-filesystem rebinding is unavailable.
There is no legacy reader or migration. Registrations use closed `af/task-wake-registration@1`
records with explicit bounded catch-up from opening, a checked registration prefix and optional
launch intent. Exact repeated requests are idempotent; conflicting keys and foreign/collected
Task identities refuse. Neither recipient references nor launch intents grant an external effect.

A reconciliation page revalidates the checked source and CAS under the Store writer transaction,
inserts immutable uniquely keyed envelopes and advances its previous checkpoint together. The
previous checkpoint is a compare-and-swap fence. Count and byte exhaustion roll back the whole
page. Limits are 128 subscriptions, 4096 envelopes, 16 MiB of logical control data, 16 KiB per
record and seven days per subscription. SQLite files remain under existing Store accounting.
These bounds refuse admission rather than discard an undelivered envelope.

Collection checks subscriptions under its existing writer lock before appending any affected
Task tombstone. An unextracted current prefix blocks collection. A reconciled prefix can be
collected; envelopes remain inline historical metadata outside CAS reachability, and inspection
discloses source unavailability. Collection never interprets an envelope as current acceptance.

## Consequences

This candidate provides internal registration, reconciliation, durable outbox and a conservative
collection barrier. It does **not** implement authenticated route installation, an external
delivery driver/receiver, receipt reconciliation, safe retention compaction, user-facing wake
commands or notification-enabled detached dispatch. Expiry refuses further reconciliation; an
unextracted expired subscription can therefore continue to block collection. Do not activate an
external consumer or opt-in detached wake until these remaining boundaries are implemented.

Pi 1.4.0 and Codex 0.167.2 operator experiments are primitive evidence only; this candidate has
no live end-to-end acceptance. Claude activation remains unavailable and its live trials are
deferred. No service, scheduler, host session, authentication or publication is installed or invoked.
The [internal API notes](../design/task-wake-journal.md) state the precise supported boundary.
