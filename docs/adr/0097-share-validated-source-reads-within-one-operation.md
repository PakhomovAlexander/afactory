# ADR-0097: Share validated source reads within one operation

Date: 2026-09-13
Status: Accepted

## Context

`source_input` validated a Snapshot and all Manifest contents, then callers needing its
Manifest immediately read and verified the same Snapshot again. Candidate sealing similarly
read parent and candidate trees, repeated those reads during capture, and reread the new tree
to construct its SourceTree port. These are duplicate reads inside one trusted operation.

## Decision

`source_snapshot` returns the exact SourceTree identity, Snapshot and freshly validated Manifest
from one read. Materialization, named checks and seal validation consume that returned data.
The compatibility `source_input` function retains its existing validation and return type.

`derive_source_tree` verifies the parent and candidate trees, checks the canonical candidate
Manifest identity, then publishes the Snapshot and SourceTree metadata directly. Its private
publication helpers accept data validated inside the same call. They are not public shortcuts
for other callers and do not retain verified state across operations. Every later read,
validation and sandbox materialization still checks current bytes; no CAS digest is trusted
merely because a previous call verified it.

The serialized Snapshot, SourceTree, producer, parent lineage and reference identities remain
unchanged for valid candidates. Invalid sizes, missing content and changed source authority
still refuse. Materialization retains verification when copying actual bytes into a sandbox.

## Verification

A differential test compares the derived Snapshot/SourceTree IDs with the previous capture
and publication sequence. After a successful call it removes and corrupts parent and candidate
content, origin, Manifest and Snapshot objects, and requires each subsequent call to refuse.
Restoring the exact bytes restores the same IDs; an incorrect Manifest byte count also refuses.
Existing Task implementation and CLI compatibility tests cover the complete seal/check path.
This removes duplicate scans within operations; it does not claim a measured live-pilot speedup
or remove the necessary fresh checks at later authority and publication boundaries.
