# `af` v0.7.1-era Task records

Each file is one persisted Task record in the exact shape the released `af` v0.7.1 wrote it —
reconstructed field for field from `git show v0.7.1:crates/reviewctl/src/task.rs` (the
`serde_json::json!` blocks at `:1812`, `:1832`, `:1924`, `:2134`, `:2664` and the `WorkerEvidence`
/ `SnapshotReceipt` / `DeliveryReceipt` structs at `:237`, `:261`, `:410`).

`af/…@1` readers are permanent (ADR-0002), so every one of these must still validate against the
`@1` schemas this tree publishes. Four fields the current writer always emits were absent then —
`schema` on `af/worker-evidence@1` and `af/task-evaluation@1`, `size` on `af/derived-snapshot@1`,
`derived_snapshot_size` on `af/task-outcome@1` — as was `ignored_paths` on pilot
`af/task-delivery@1` receipts. They are therefore optional in the schemas and asserted present
in `crates/reviewctl/tests/task_contracts.rs` against what the current binary writes.

The file name before its first `.` is the schema file the record must satisfy; the rest
distinguishes cases of the same record. Digests are structurally valid placeholders — nothing
here is fetched, only validated.
