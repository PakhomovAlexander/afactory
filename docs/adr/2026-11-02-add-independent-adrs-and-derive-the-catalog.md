# ADR-2026-11-02-add-independent-adrs-and-derive-the-catalog: Add independent ADRs and derive the catalog

**Status:** proposed (2026-11-02); accepted on merge. Supersedes the central index and sequential
allocation requirements of the ADR guide, and only the index-entry removal obligation of
[ADR-0113](0113-ga-reads-only-what-ga-writes.md), clause 6.

## Context

Parallel proposals repeatedly allocate the same next number and edit the same central index.
The merged Pi and Scenarios work both arrived as 0274. Renumbering each unmerged proposal fixes
one collision while leaving the next pair dependent on each other's timing. The decision files
already contain the title and status needed for discovery.

## Considered options

- Keep allocating the next number and repair conflicts during integration: rejected because
  unrelated changes still edit a shared append point and need repeated link repairs.
- Reserve numbers in a central manifest or commit a generated index: rejected because either
  retains a shared file that every proposal changes.
- Rename all historical decisions: rejected because accepted identities, exceptions and released
  references must stay stable.

## Decision

New decisions use `YYYY-MM-DD-descriptive-topic.md`, with the proposal date and a specific topic
that makes the basename unique. The identity is the whole basename without the extension and
stays stable after acceptance. Distinct same-day topics require no coordination or sequence
allocation. A same-topic collision calls for a more specific accurate topic, not a shared counter.

`docs/adr/README.md` is stable authoring and lifecycle guidance. The files are the authoritative
catalog; no per-decision index or generated catalog is committed. `python3 -I scripts/adr.py list`
prints titles, recorded statuses and relative links in basename order on stdout. GitHub directory
browsing supplies direct discovery. Adding a decision adds only its own file, without edits to
README, a manifest, scripts or test lists.
Older authoring instructions asking for assigned numbers or README entries are superseded,
including the wording retained in shipped Task prompts. Those packages remain unchanged.

Legacy identifiers and historical anomalies remain valid. Only the unmerged Pi proposal is
renamed to `2026-11-02-own-the-pi-sdk-host-and-confine-its-tools.md`; accepted Scenarios ADR-0274,
ADRs 0270–0273, the 0107 exceptions and all other accepted identities remain unchanged.

Acceptance, rejection and supersession keep their existing lifecycle. Accepted bodies remain
immutable, with status-only supersession notes and the existing deleted-link repair exception.
A fully superseded record is still removed unless its successor explicitly retains it; deleting
its index entry is no longer required because no such entry exists. ADR-0113's remaining clause 6
and clause 8 obligations and ADR-0145's lifecycle obligations are unchanged.

## Consequences

Independent proposals no longer share an allocation or index edit. Catalog output reflects the
files actually present, including proposals and historical exceptions, without installation or
writes. Focused script regressions merge two independent same-date additions on disposable Git
branches, discover both, and prove the shared guide stayed byte-identical. Runtime authority,
Scenarios, Ants, Provider contracts and validation boundaries are unaffected.
