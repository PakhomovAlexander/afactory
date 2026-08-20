# Review Kernel workstream log

## 2026-08-20 — M0 and M1

Closed the append-only event vocabulary, structural run reports, typed invocation/output ports,
and canonical report projection. Added complete operator rendering through `show`, long ledger,
and markdown report commands.

## 2026-08-20 — M2.1

Added versioned pipeline Subject configuration and digest-pinned reviewer capability declarations.
Legacy pipeline and package formats remain whole-tree-only. Unsupported diff execution, inline
diff reviewers, empty reviewer sets, unsafe registry names, symlink/non-regular package entries,
and lossy package paths fail closed. Shipped reviewers remain whole-tree-only until M2.2–M2.4
provide pinned authority, Base, Subject, and Change Set artifacts.
