- Performance laboratory P10 repair. The kernel receipts reader (`af.perf-kernel-receipts/3`,
  shape and schema unchanged) now refuses a Task result whose `evidence` or
  `missing_obligations` is not strictly ascending in the kernel's string order, as `unique_set`
  does when the kernel deserializes it. Before, it accepted the root's reversed evidence. An
  output's `artifact_ids` stays valid in any order. Four generated fixtures cover an unsorted
  and a duplicated entry in each set. A Rust test also checks eleven variants built at test
  time, and the laboratory's reader and `review_core`'s own deserialization agree on every one.
  The retained evidence is byte-exact. `checks/p10-repair-reconciliation.json` reconciles the
  current source with the measured commit through the earlier repair's record, using the new
  `fingerprint.py rerepair`
  ([ADR-0189](docs/adr/0189-read-a-task-result-s-sets-in-the-kernel-s-order-and-reconcile-a-second-repair-through-the-first.md)).
