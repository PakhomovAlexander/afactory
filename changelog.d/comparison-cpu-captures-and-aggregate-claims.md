- A native release comparison's CPU lane is complete only when every case the aggregate
  verdict selects has a usable baseline and candidate capture (no omission, at least the frozen
  registry's `minimum_samples`, its own collapsed stacks) and both directions of its
  differential flamegraph name exactly those captures' collapsed stacks as inputs. Captures
  removed, a side missing or omitted, too few samples or a render of other inputs leave the lane
  failed with its reason in `comparison-report` and `comparison-verify` alike, however the
  surviving SVG and every receipt were resealed; an honest incomplete bundle still verifies as
  incomplete. The profile's registry must be the installed producer's.
- `executor comparison-report` and `comparison-export` derive the core aggregate's partition
  entries, run, verdict, result, coverage and regressions again from the admitted partitions,
  as `comparison-verify` does from a bundle. An aggregate altered in the journal, even before
  its diagnostic children were submitted with matching parent receipts, is refused: the report
  is incomplete with verification `refused`, and its export verifies as that same report.
  `comparison-status` still reads cheaply without verifying
  ([ADR-0240](docs/adr/0240-execute-performance-experiments-independently-on-a-locked-vm.md)).
