# Version manifest path encoding without changing Snapshot content identity

**Status:** proposed

The original source Manifest encoded a literal space inside percent mode and left leading or
trailing whitespace literal for UTF-8 paths. Live Reports now reject edge whitespace because
normalizing or dropping it can misclassify diff Scope, but Git permits such filenames. Changing
the single implicit alphabet would make older Manifests unreadable and could change an unchanged
tree's Snapshot identity.

We will add an explicit Manifest path-encoding generation. Artifacts without the field are
`legacy_v1`; new captures use `percent_v2` when its spelling differs, while ordinary trees keep
the omitted legacy marker. Materialization validates against the declared generation. Snapshot
content identity normalizes either representation to the legacy spelling before hashing, so it
continues to identify raw tree content rather than the current JSON alphabet.

## Considered options

- **Replace the implicit alphabet in place.** Smallest code change. Rejected because existing
  content-addressed Manifests would stop materializing and unchanged trees could appear changed.
- **Reject Git paths that live Reports cannot spell.** Preserves one alphabet. Rejected because
  capture would cease to be complete and the review would silently exclude legal repository
  content.
- **Version the encoding and preserve content identity (chosen).** Adds a durable discriminator
  and a compatibility reader while keeping Snapshot identity about raw tree content.

## Consequences

- Missing `path_encoding` permanently means `legacy_v1`; this default cannot be reassigned.
- Capture emits `percent_v2` only when a path needs its new spelling, avoiding CAS churn for
  ordinary trees.
- Materialization errors distinguish an encoding-generation mismatch from a sandbox escape.
- Both generations remain readable; future alphabets require another explicit generation.
