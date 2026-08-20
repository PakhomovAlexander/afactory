# Gate checks may reach host caches through a root-allowlisted passthrough

**Status:** superseded by
[ADR-0008](0008-safe-caches-are-sandbox-local-snapshots.md) (2026-08-20)

A green gate does not mean green CI. `.review/pipelines/heavy.toml` documents two checks it
cannot host, both observed rather than assumed: the smoke tests need a writable tree, and the
Rust workspace needs a warm registry because `Sandbox::environment()` sets `HOME` to the sandbox
root, so every cache is cold and cargo would refetch the world on every attempt.

The first half is nearly free — `Mode::EphemeralWrite` already exists and reviewers already use
it; the gate simply hardcodes `Mode::ReadOnly`. The second half is a real trade. We decided a
pipeline may declare cache directories that are mapped from the host, with three constraints: the
source path must fall under a kernel-owned allowlist of permitted roots, the mapping may be
declared read-only, and every passthrough is recorded in the run report so an operator reading a
verdict can see what was exposed.

The sandbox's declared isolation level is **not** downgraded by a passthrough. That is the
loosening being accepted here, and it is recorded rather than quietly done.

## Considered options

- **`EphemeralWrite` only, caches stay cold.** A one-line change unlocking the smoke tests, with
  no new configuration surface and no new way for a check to touch anything outside its sandbox.
  Rejected as insufficient: it leaves the most expensive check in the repository permanently
  outside the gate, which is the parity gap that motivated the work.
- **COW-clone the host cache into the sandbox.** `reflink-copy` is already a dependency and each
  sandbox is already a COW clone, so on APFS and btrfs a warm-but-isolated cache is close to
  free, with writes contained and discarded at seal. Rejected for this round on cost and failure
  mode: a cache and a sandbox temp dir on different filesystems degrade to a real multi-gigabyte
  copy, which needs a size guard and a clear diagnostic to avoid a mysterious stall. Worth
  revisiting — it dominates the chosen option on safety.
- **Downgrade the declared isolation whenever a passthrough exists.** Reuses the `admit`
  machinery, keeps `Policy::safe()` meaning what it says, and makes the weaker provider unable to
  claim the stronger property — the sandbox layer's own stated move. Rejected because it makes
  containment and a warm cache mutually exclusive, which puts the Rust check back outside the
  gate for any pipeline that demands isolation.
- **An unconstrained passthrough field (rejected).** Defensible on the grounds that whoever edits
  the pipeline can already name arbitrary check programs. Rejected because a check program is
  visible in the pipeline and reviewed as such, while one passthrough line silently changes what
  every check can reach — and nothing would distinguish a cache directory from a credential one.
- **Root-allowlisted, optionally read-only, recorded passthrough (chosen).** Reaches full gate
  parity, bounds what may be named, and makes the exposure visible in the artifact an operator
  already reads.

## Consequences

- The gate runs in `Mode::EphemeralWrite`. We lose the property that a passing gate proved its
  checks did not need to write to the tree.
- A gate check can write to a host directory that outlives the sandbox, the run and the campaign.
  Checks are declared by the repository under review, so this is a persistence path out of the
  sandbox, granted by configuration. Prefer `mode = "ro"` with `cargo --offline`, which keeps
  already-cached crates working and fails loudly on a new fetch.
- The permitted-roots allowlist is kernel-owned, not pipeline-owned; a pipeline that names a path
  outside it fails at load, as does any path containing `..`.
- `RunReport@2` records the passthroughs a run used. A verdict that does not say what was exposed
  is not a complete record of the run.
- The declared isolation level now means "contained, except where configured otherwise" — a claim
  no automated check can verify. This widens the gap between declared and verified isolation, and
  it does so alongside [ADR-0004's](0004-reviewers-author-verified-patch-proposals.md) decision to
  have reviewers author patches. The combined risk delta is recorded in
  [`../backlog.md`](../backlog.md) under M6; the containment probes stay open and labelled open.
