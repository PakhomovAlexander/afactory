# Share one bounded infrastructure executor

**Status:** proposed

Capture, materialization, sandbox cloning, permission changes, sealing, and cleanup can overlap
when the scheduler runs several nodes. Giving every phase its own threads multiplies host
parallelism by scheduler concurrency. A process-global RAII permit pool bounded active items, but
the permit was transferable while its re-entrant accounting was thread-local, and per-item
acquisition added a mutex and condition-variable operation to every filesystem syscall.

We decided that filesystem-heavy infrastructure submits borrowed or owned work to one bounded,
re-entrant executor in `review-parallel`. The executor is sized lazily from the host's available
parallelism on first use. Nested and concurrent phases share the executor's fixed worker
threads, so scheduler concurrency cannot multiply the CPU worker budget. A joined-phase primitive
lets one bounded phase progress while another uses otherwise-idle workers; sealing uses it to hash
one directory level while discovering the next.

## Considered options

- **Keep per-phase thread pools without a shared boundary.** Simple and locally configurable, but
  four concurrent reviewers can each create a host-sized pool. Rejected because oversubscription
  is a process property no individual phase can prevent.
- **Keep the global RAII permit pool and amortize permits over batches.** This reduces lock traffic,
  but every phase still spawns threads that mostly park, and a transferable permit cannot safely
  use thread-local re-entrancy accounting. Making permits non-transferable closes the accounting
  hole but not the excess threads or repeated synchronization.
- **Divide each phase by the scheduler's maximum concurrency.** Bounds the worst case, but a lone
  phase is permanently underutilized and scheduler topology leaks into infrastructure crates.
- **Use one bounded work-stealing executor (chosen).** It gives lone phases the whole host budget,
  lets overlapping phases interleave on the same threads, supports nested work, and removes public
  permit ownership from the contract.

## Consequences

- `review-parallel` owns the only CPU-oriented infrastructure worker threads in the process.
- Phase-specific worker-count parameters and public permits are not part of the API. Concurrency is
  the executor's process-wide capacity; the graph scheduler separately bounds active nodes.
- CAS durability synchronization remains an independent bounded I/O fan-out because waiting for
  `fsync` does not consume the CPU executor.
- Operations must submit deterministic indexed collections and restore canonical ordering where
  their result is persisted; work-stealing completion order is never artifact order.
- Directory discovery may run level-by-level and overlap hashing through the shared executor.
  Manifest construction and mutation lists sort afterward, so task completion order never becomes
  artifact order. Metadata lookup remains non-following even when that costs an extra path-based
  lookup: executor utilization is not authority to weaken sandbox containment.
- A task waiting on a resource budget may not hold that resource across a re-entrant executor
  submission. Nested fan-out starts only after sibling tasks that can wait on the same budget have
  drained, so work stealing cannot make a permit holder block behind itself.
