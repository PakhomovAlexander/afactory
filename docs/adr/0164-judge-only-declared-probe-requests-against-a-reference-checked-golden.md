# ADR-0164: Judge only declared probe requests against a reference-checked golden

**Status:** accepted (2026-10-19); decisions 3 and 6 superseded by
[ADR-0165](0165-own-probe-scratch-in-the-driver-and-take-lease-expiry-only-from-the-store.md).
Tightens decisions 1, 3 and 6 of
[ADR-0163](0163-measure-native-workloads-through-a-public-api-probe-and-independent-golden-oracles.md)
(the p2 repair of the [continuous performance laboratory](../design/continuous-performance.md)
design); the rest of ADR-0163 stands. The native supervisor, process-group and Linux namespace
containment stay package p3a's.

## Context

Independent review of p2 reproduced six ways the native probe's trusted side could pass or
damage what it should refuse. `judge` validated a recorded request's shape but never compared it
with the registry, so a sentinel request of 1 repetition, with output copying the golden digest
and count of the declared 64, passed. `load` checked golden coverage and identity but not that
the document was what the reference generator derives, so a changed golden digest became the
authority. The probe removed `<scratch>/probe-work` even when its own `create_dir` failed, so a
directory another process created there was deleted. The driver buffered a probe's whole stdout
and stderr before checking the 64-KiB record bound. The persistence diagnostic declared logical
bytes but the judge never required them. The state persistence fixture took its 15-second lease
during setup and entered the timed renewals without refreshing it, so a slow setup surfaced as
`operation_failed` instead of `lease_expired`. The standalone `probe --timeout` also accepted 0,
negative values and values above the 120-second hard maximum.

## Considered options

1. **Judge recorded output as it is, trusting whoever recorded the request.** Rejected: the
   request names what was executed; a judge that accepts any request shape judges an execution
   nobody declared.
2. **Check the golden document only in `cli.py golden` and the repository checks.** Rejected:
   `probe` and `judge` use the document as authority, so they must refuse one the reference
   does not derive, every time, rather than rely on a separate check having run.
3. **Remove `probe-work` after any failure, as before.** Rejected: a path the probe could not
   create is not the probe's.
4. **Raise the lease or retry a lapsed renewal.** Rejected by ADR-0163 and still rejected: the
   production lease and its failure are part of what is measured.
5. **Truncate overflowing output and keep judging.** Rejected for this driver: an unbounded
   control stream is the side's failure. p3a's supervisor drains logs into capped files and
   counts truncation for the paired run.

## Decision

1. **Declared requests only.** Before any output is judged, `oracle.check_request` resolves the
   request's instance in the registry and requires its case, contract, typed parameter
   assignment and repetitions to equal what `request_for` builds, compared as canonical JSON. A
   request that differs is refused (`cli.py judge` exits 2), whatever its output says.
2. **Reference-checked golden.** `oracle.load`, which `probe` and `judge` use, checks coverage
   and then requires the golden document to equal `perflab.reference.golden_document` for the
   registry; nothing is launched or judged otherwise. Reference values are never learned from a
   candidate execution.
3. **Owned scratch.** The probe creates `probe-work` with `create_dir` and records its device and
   inode. A failed creation is the sample's `fixture_failed` and removes nothing. Cleanup removes
   the directory, without following symlinks, only while the entry at its path is still that
   directory; a missing or replaced entry is left in place and reported on stderr, and the sample
   exits 1.
4. **Bounded output.** `oracle.run_probe` writes the request and drains stdout and stderr together
   through one selector, reading at most one byte past either cap: stdout holds at most the
   64-KiB control record, stdout and stderr together at most the registry's `sample_log_bytes`
   (256 KiB). Exceeding the stdout cap is `oracle_mismatch`, the combined cap
   `lifecycle_violation`, the deadline `timeout`; each kills and reaps the direct child. This is a
   direct-child kill in a diagnostic driver, not process-group or namespace cleanup. An explicit
   `--timeout` must be a whole number of seconds from 1 to 120 and is checked before anything
   launches; an omitted one is the case's declared timeout.
5. **Logical bytes.** A successful `persistence_fs_v1` message must carry `logical_bytes` in
   `bytes` as a canonical positive integer. The CAS variant's must equal the reference's twice
   the 1-MiB object; the state variant's are the canonical bytes of the appended events, whose
   timestamps and identifiers vary, so they are checked for shape and never claimed as a golden
   value.
6. **State persistence lease.** The fixture's production lease is renewed as the last step of
   setup, immediately before the region; a renewal that fails, in setup or in the region, is
   classified by the Store's own lease state, ends the sample at the first failure and is never
   retried. The refresh is a recorded event, so the golden `prefix_events` of
   `persistence_fs__operation_state_append_read` is 4 (open, propose, admit, refresh), and its
   golden digest changed with it. No persisted shape changed; schemas and versions are as before.

## Consequences

- A recorded execution passes only for the request its registry declares and only against a
  golden document the reference generator derives; `probe` and `judge` each spend about a second
  regenerating the reference.
- A probe cannot delete a directory it did not create, and a cleanup it could not confirm fails
  the sample visibly.
- A runaway probe costs the driver at most its declared output caps and its timeout; descendants
  of the direct child remain p3a's supervisor's to contain.
- Product constants are unchanged: the 15-second lease, the 8-MiB envelope cap, validation, CAS
  limits, pairs, budgets and tolerances.
