# ADR-0109: Re-base Warm Workspaces at stable roots with digest verification

Date: 2026-09-18
Status: Accepted

Implements package P3 of [`docs/design/worker-warm-layers.md`](../design/worker-warm-layers.md)
on top of [ADR-0107](0107-carry-worker-notes-and-head-deltas-as-declared-warm-layers.md) (the
Warm Set it extends), [ADR-0020](0020-stream-cas-materialization-and-clone-duplicates.md)
(verified CAS materialization and copy-on-write clones), [ADR-0035](0035-address-campaign-state-by-opaque-id.md)
(machine-local state addressed by opaque identity) and
[ADR-0002](0002-event-payload-changes-bump-the-type-version.md) (new events, not widened
payloads).

## Context

Every Round materializes the head Snapshot from the CAS into a fresh temporary template and
clones one sandbox per Attempt from it. Between two Rounds of one Campaign the head usually
differs by a handful of paths, yet the whole tree is written again, and the template's location
changes every Round, which package P4 cannot tolerate because harness session stores key by
working directory. The design asks for a stable workspace root per node per Campaign, re-based
to each new head by tree diff, with the result proven equal to the head's Tree Digest or
discarded.

## Decision

- **One stable root per node per Campaign, named by identity.** A reviewer node with
  `warm = { workspace = "rebase" }` gets `<cache root>/<workspace id>/` under
  `$XDG_CACHE_HOME/af/workspaces`, where the workspace identity is the hex prefix of a
  domain-separated digest over the Campaign run, its Campaign Manifest and the node. The
  identity, never the host path, is what `WarmSet@1` and the new event record. Workspace state
  never crosses nodes: two nodes of one Campaign have two roots.
- **Rebase on a clone, verify by scanning, swap only then.** On a changed head the kernel clones
  the trusted template copy-on-write, applies the manifest-level diff between the previous head
  and the new one (removals and modifications unlinked, emptied directories pruned, changed and
  added entries written from the CAS exactly as a materialization writes them), scans the clone
  back into a manifest with the head's path spelling, and compares that manifest's content
  digest with the head's Tree Digest. Only an equal digest swaps the clone in. The scan hashes
  every entry, so a template that drifted under its marker is caught even when the diff never
  touched the drifted path.
- **Fail closed into a full materialization and record why.** A rebase that cannot be applied,
  cannot be verified, or verifies to another digest is discarded, and the head is materialized
  from the CAS into the root instead. `WorkspaceRebased@1` records, per node and kernel run,
  the previous and current head Snapshot IDs, the basis (`full`, `rebased`, `reused`), the
  fallback reason when the basis is `full` (`no_verified_template`, `template_corrupt`,
  `apply_failed`, `digest_mismatch`), the digest the template was verified to hold, and the
  entries touched. The root's head marker is removed before any swap and written after the
  manifest, so a preparation that ends early leaves a root the next one refuses to trust.
- **An unchanged head materializes nothing.** When the marker's digest equals the head's Tree
  Digest the template is reused as it stands: no clone, no scan, no write, and a record whose
  basis is `reused` with zero entries touched.
- **The Warm Set names the workspace; the layer carries no artifact.** `WarmSet@1` gains
  `workspace` and `workspace_id`, present together or not at all, and the `workspace` layer joins
  `WarmSetSelected@1` when the basis is `rebased` or `reused`. The workspace's content is the head
  Snapshot itself, already in every manifest, so no separate CAS artifact is minted for it. A node
  with the policy selects a Warm Set in every Round, and the workspace is prepared and verified
  before the Warm Set that names it is recorded or read back, so a resumed Round rebuilds its
  template before its first Attempt exactly as a fresh Round does and refuses a recorded Warm Set
  that names another workspace.
- **Per-Attempt sandboxes remain fresh clones.** Every Attempt, retries included, clones the
  verified template into its own temporary directory and seals there. Nothing an Attempt writes
  reaches the stable root, a sibling, or the source; the sealed diff of a warm Attempt is what
  the reviewer wrote and nothing else. A node without the policy keeps today's temporary
  template, and a pipeline without `workspace = "rebase"` is byte-identical in every event,
  artifact and fixture to one written before this package.

## Considered options

- Re-base the trusted tree in place: rejected, a failure midway would leave a tree that is
  neither the previous head nor the new one, and the fallback would have nothing to clone.
- Verify only the touched paths after a rebase: rejected, the acceptance requires a corrupted
  rebase to fail closed, and a template that drifted under its marker cannot be caught without
  hashing the whole tree; reads are cheap next to CAS materialization.
- Record the host path of the root in the Warm Set: rejected, host paths never enter durable
  events or captured authority, and the path is a machine-local cache location.
- Share one stable root across the nodes of a Campaign: rejected, workspace state is node-private
  by design decision D2 and package P4 keys session state by working directory per node.
- Keep the template in the process-wide temporary directory and merely reuse it across nodes:
  rejected, that is today's behaviour and removes nothing across Rounds.

## Consequences

- A warm node's Round N+1 template costs one copy-on-write clone, the diff's writes and one
  read-only scan of the tree instead of a full CAS materialization; the dogfood comparison the
  design requires ships with the first warm Campaign, not with this package.
- Stable roots are bounded machine-local cache growth: one tree per warm node per Campaign,
  addressed by identity under the XDG cache directory. Collection of roots whose Campaign has
  ended remains a separate concern.
- One new event type joins the closed vocabulary; `WarmSet@1` and `WarmSetSelected@1` gain
  optional fields and one layer name, which ADR-0107 permits until the release that first ships
  them. Existing logs replay unchanged.
- Package P4 can rely on a working-directory-stable template per node; per-Attempt sandbox paths
  still change, which P4 must account for when it keys session stores.
