# Review Kernel — architecture decisions

Decisions about the kernel's own design. These are distinct from
[`../../../../docs/adr/`](../../../../docs/adr/), which is a generated hub's own decision log —
the kernel ships *into* such a hub, so its decisions travel with the code rather than with the
project using it.

Same rules as the hub's: one decision per file, numbered, **immutable once accepted**. A changed
decision becomes a new ADR marked *superseded by* the old one, with links both ways. Record the
options you rejected and why — that is the part future-you needs.

## Index

- [0001 — Compute the Change Set with `git diff`, reachable only through a typed
  method](0001-tree-diff-behind-a-typed-method.md)
- [0002 — A payload shape change bumps the event type
  version](0002-event-payload-changes-bump-the-type-version.md)
- [0003 — Gate checks may reach host caches through a root-allowlisted
  passthrough](0003-gate-caches-pass-through-to-the-host.md)
- [0004 — Reviewers author patch proposals; the kernel verifies, git
  applies](0004-reviewers-author-verified-patch-proposals.md)
- [0005 — Report artifacts are authoritative for finding
  projections](0005-report-artifacts-are-projection-authority.md)
- [0006 — Finding identity is independent of path and
  title](0006-finding-identity-is-path-independent.md)
- [0007 — Demands are independent blocking
  obligations](0007-demands-are-independent-blocking-obligations.md)
- [0008 — Safe caches are sandbox-local
  snapshots](0008-safe-caches-are-sandbox-local-snapshots.md)
- [0009 — Campaign authority is resolved before candidate
  capture](0009-campaign-authority-is-base-pinned.md)
- [0010 — Proposals are exported by ID and remain bound to their base
  Snapshot](0010-proposals-are-exported-by-id-and-base-bound.md)
- [0011 — Silence is not a Drop](0011-silence-is-not-a-drop.md)
- [0012 — Fixed requires current-Subject
  verification](0012-fixed-requires-current-subject-verification.md)
- [0013 — Scope is evaluated per active Report
  claim](0013-scope-is-evaluated-per-active-claim.md)
- [0014 — Non-fixed resolutions are scoped and
  challengeable](0014-non-fixed-resolutions-are-challengeable.md)
- [0015 — Safe attempts receive handles, not reusable
  secrets](0015-safe-attempts-receive-handles-not-secrets.md)
