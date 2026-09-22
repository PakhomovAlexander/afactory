# Afactory — the Store (placement; design is a separate step)

**Status:** v2 · part of [`overview.md`](overview.md). The log/ledger is "the heart of the
whole system" and is designed carefully as its own step (D5). This document fixes what that
step must honour and where the Store sits; it does not design the internals.

## 1. What the Store is

The kernel-level entity that owns all state: the append-only **log** of events per run, the
content-addressed **objects** (snapshots, inputs, outputs, evidence, contracts), the derived
**views** (Ledger projections), and the **leases** that make one engine the single driver of a
run. Everything the kernel records lives here; nothing lives in the repository.

```
                 ┌──────────────────────── Store ────────────────────────┐
  engine ──────► │ append(run, events) · read(run, from) · lease(run)     │
  af (reads) ──► │ put(bytes) → digest · get(digest)                     │
  af-tui ──────► │ view(run, name, at) · watch(run)                      │
                 │            backend: sqlite now · distributed later     │
                 └───────────────────────────────────────────────────────┘
```

Backends (D12): `sqlite` — embedded, one file under `$XDG_STATE_HOME/af/`, zero install, single
machine — is the only backend for now. A shared, consensus-based
distributed coordinator (Raft-class) comes later, and which database backs it is deliberately
undecided. The interface is backend-agnostic from day one; `af store migrate` moves a run
between backends once a second one exists.

## 2. Invariants the design step must keep

These come from the current kernel (proven by its tests) and from the research; the Store
design may change representation, never these.

1. **Append-only, single writer per run.** A run has one lease holder; appends carry the
   lease epoch; a stale epoch is refused and recorded (`LeaseFenced`).
2. **Dense sequence, derived ids.** Events are numbered densely per run; the event id derives
   from run + sequence, so replay reproduces identical logs. Random ids (ULIDs) name only what a
   human action creates: tasks, runs, attempts.
3. **Shape-only validation at the store; semantics at the machine.** The Store checks schema
   and referenced-object presence; transition legality is the state machine's job (fixing
   today's layering leak where the store re-parses pipeline TOML).
4. **Objects by digest and size.** SHA-256 over canonical bytes with a domain separator;
   references carry size; a missing referenced object on read is corruption, not an empty
   field; objects are immutable.
5. **Views are rebuildable.** `rebuild` is the only constructor of a Ledger; a view is cached,
   versioned by the reducer's version, and discardable.
6. **Replay is the proof.** Delete every view, replay the log and objects, get the same
   bytes; once a second backend exists, a Store conformance test must check this against
   every backend.
7. **No secrets, ever.** Redacted fingerprints, handles, ids, timings, spend, dispositions —
   never a token, code, or raw secret-bearing output.
8. **No wall clock in identity.** Timestamps are payload facts where they matter; envelopes
   carry none.
9. **Bounded growth is explicit.** Compaction is an event that names what stays authoritative;
   large blobs (transcripts) live in the cache tier and are referenced by digest.

## 3. Questions the design step answers

- The exact interface: append batching, read cursors, watch semantics, lease TTL and renewal,
  object streaming, view materialization (server-side vs client-side reducers).
- Consistency model per backend; what `sqlite` promises now and what a distributed backend must
  promise later; how a run behaves offline.
- Identity across machines and repositories: run ids, repository identity (`git-common-dir`
  today), user identity for multi-writer backends, authentication to a shared Store.
- Schema evolution: versioned events, reducer versions, view invalidation, `af store migrate`.
- Retention, compaction, export/import (`af export <run>` as a portable bundle), and audit.
- Performance targets: append cost, read of a 10k-event run under 200 ms from a view, cold
  rebuild under 2 s, and the same numbers for the shared backends.

## 4. What this replaces

Today's `events.sqlite` under `.review/runs/<campaign>/` and the CAS beside it move out of the
repository into the Store; the migration is an export of the existing rows — the envelope is
already what the log needs. No `.gitignore` entries, no `merge=union`, no state branches: git
is not involved (D15, D16).
