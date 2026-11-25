# ADR-2026-11-25-observe-fleet-and-project-origins-with-pinned-readers: Observe fleet and Project origins with pinned readers

**Status:** proposed (2026-11-25).

## Context

A Mac operator needs infrastructure headroom and waiting native Tasks before a distributed
executor exists. The management broker returns host availability and allocation state. It has
no Project authorization, and its response omits several resource measurements.

## Decision

- The machine-local fleet context binds an owner-provided scope to the management SSH
  destination and host key. Authenticate with a local identity and obtain both management reads
  before atomically creating the binding. The same binding can be verified again. A different
  binding must choose another alias.
- Use two fixed management reads through the pinned transport. Keep both availability views on
  the same physical host identity. The broker reports VM membership and host SSD headroom, so
  storage pool inventory remains unknown. Missing totals, charges, placement and revisions
  remain unknown. Broker Task labels are outside the infrastructure view.
- A Project reader is a separate server-side grant. A restricted SSH key selects a forced command
  that names one Project, registered Workspace and principal. The native reader opens that
  Workspace's Task Store read-only and derives waiting Tasks from checked replay. A pending plan
  Decision requires an explicit grant for the principal and the Task's captured developer policy.
  Taking the Decision continues to require the native signed authorization.
- A briefing exposes freshness, discovery readiness and an operator action. An infrastructure
  observation may be recalled from its matching private cache when the authenticated reader
  explicitly reports temporary unavailability. Project responses require live authorization for
  every read, and a stale authorized response carries no available Decisions.
- Existing release pins and Store formats retain their rules. Fleet commands use their own
  machine-local context and versioned documents. They do not admit or dispatch distributed work.

## Considered options

- A combined infrastructure and Project broker would give a capacity credential access to work
  identities and bypass Project-side grants.
- Fetching Task Store files would disclose full private histories and need compatibility readers.
  A narrow observation from the native release on the origin keeps replay and disclosure there.
- Combining nominal capacity, static availability and allocations would require data the broker
  does not supply and could double-count shared physical resources.

## Consequences

Operators install the read grants and exchange host pins through a trusted channel. An origin can
report stale or unavailable while its siblings remain current. A deployment may report an absent
cluster and still serve authorized observations. None of the new commands creates a worker
registration, submits a Decision or mutates an allocation.
