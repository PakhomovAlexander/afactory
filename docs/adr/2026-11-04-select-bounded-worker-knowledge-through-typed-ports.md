# ADR-2026-11-04-select-bounded-worker-knowledge-through-typed-ports: Bounded Worker knowledge

Status: proposed; accepted on merge.
Date: 2026-11-04.

## Context

Issue #586 completes the three-step Worker local development design. Execution Bindings pin
packages; Cache Snapshots, Build Caches and private roots already have their own authority.
Knowledge must preserve declared inputs, exact context, independent slots and admitted lineage.
A Worker cannot grant trust to its own prose, nor can a Provider's unavailable internal context
be represented as observed evidence.

## Considered options

- A shared writable memory directory or repository-wide search index: rejected because private
  reasoning could cross slots and ingestion, eviction and restart would lack exact authority.
- Importing raw sessions or treating build caches as knowledge: rejected because neither is an
  admitted source-backed fact. Session Snapshots remain unsupported under ADR-0145.
- Provider-specific interception of internal model tool calls: rejected as a common protocol;
  command harnesses do not expose those calls and native Providers have different tools.
- Typed, bounded discovery and retrieval between Worker invocations: chosen. Existing model and
  command transports deliver the same context; the Task dispatcher owns scheduling and retries.

## Decision

An optional pinned `knowledge.toml` companion, `af.worker-knowledge/1`, declares explicit symbolic
source files and byte ranges, path dependencies, manifest/context/retrieval limits, Task retention,
freshness policy and same-slot private Notes. `af.worker/1` and the step-2 development companion
keep their shapes. Unsupported companion versions and fields fail closed. The package requests
only captured `read-source` authority; it adds no host path, network or credential capability.

The common host selects an immutable `af/KnowledgeIndex@1` and `af/KnowledgeContext@1` before
recording the invocation and reserving any Attempt. The reserved optional `knowledge` input is
kernel-only. An initial manifest has metadata and omissions, with no excerpt prose. A Worker
requests entry identities through an optional `af/KnowledgeQuery@1` output. A later invocation
binds that output on `knowledge_query`; this is on-demand retrieval through declared ports,
not an implicit additional model call or live tool service. A query names only entries in the
manifest actually delivered to its producer and retains that index as evidence.

Source facts are exact UTF-8 excerpts of explicitly named captured source files, not Worker
conclusions or assertions of truth. Entry identity hashes bytes, range and content-addressed
path dependencies, including absence and file kind. Rebuilding from authoritative source records
makes identical selections. A changed source dependency invalidates the entry even when its name
is unchanged; unrelated changes do not invalidate source facts. Symlinks, oversized files, invalid
text and credential-shaped material are omitted with explicit reasons. No directory or archive
is ingested.

Private Notes use existing `af/WorkerNotes@1` outputs and compiler same-slot wiring. The host
checks the Attempt producer against Task and effective slot and resolves dependencies against
the producing Snapshot, ignoring Worker-claimed inspected digests. It reuses **Head Delta** and
**Warm Set** artifacts, with retrieved Notes/delta in an `af.private-knowledge/1` wrapper.
Notes are always `hypothesis`, `same_slot`. Changes mark them `revalidate`
or omit them under `omit_stale`. Pathless reasoning needs the exact Snapshot. The raw Notes and
query invocation inputs are retained provenance, not additional prompt text; only requested,
bounded Notes are rendered. A retry inherits the recorded selection, never its failed sibling's
Notes. Only dispatcher-supplied admitted outputs enter selection; failed/quarantined raw evidence
has no lookup path.

Cross-Task retrieval defaults to disabled. Explicit `cross_task = "source_facts"` permits an
imported query only for public source facts from a kernel index whose entries revalidate against
the receiver's declared current source. This capability also governs cross-slot fact queries.
It grants no private Notes, transcript import, or trust in a foreign Worker's conclusions.
A private query fails closed even with that capability. There is no ambient cross-Task search.

Indexes are immutable derived records in the existing Task CAS, not an additional Store. All
artifacts and exact excerpt blobs participate in existing reachability, writer leases, machine
Storage Budget and Task collection. Manifest overflow drops entries in reverse name order and
records each omission. Retrieval applies stable entry-ID order, item/byte bounds, then the total
context bound. Missing, corrupt, evicted, stale and refused material is explicit; a bound too
small even for omission records refuses preparation. Recorded selections never silently requery
on retry/restart. Exact delivered UTF-8 bytes and source identities are in the context evidence;
command receipts already identify that context. A command harness's internal model prompts or
reads remain unobserved.

## Consequences

Language tools remain configuration. The same retrieval works for ordinary command Workers,
native model Workers and step-1 container command Workers; the latter retain their separate
machine admission and command evidence. Native model development environments remain unsupported.
No gate, reviewer independence, deterministic acceptance or publication authority changes.
Queries and knowledge records cannot claim acceptance or branching authority.

There are no Provider calls, mutable shared directories or filesystem scans for undeclared
knowledge. Enabled selection validates source integrity through the existing Snapshot reader and indexes at most
64 explicitly named bounded source files and one same-slot Notes artifact; Notes may compute the existing head-to-head diff. The next invocation
pays only its declared manifest and requested excerpts. There is no paid dogfood or savings claim.
Schema parity, captured Task retry/replay, native fake-CLI/command adapter, freshness, privacy,
bounds and missing-artifact tests supply deterministic protocol evidence, not live containment.
