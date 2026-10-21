# Real perf output over a synthetic workload

These files are retained output of a real Linux `perf` 6.1 over an owned synthetic C workload,
`fixture.c` (SHA-256 `2089b89b74d6ac20c663afee545c044f235860bdad38bbe30e659b1dad1ea19c`). The
workload is not AF, and the files are not a capture of the AF probe or of any AF workload. They
are not the missing `real-perf-script.txt` of ADR-0174 either, and they do not stand in for a
qualified Linux capture. That capture remains pending operational evidence (ADR-0176).

The workload ran as `baseline` and `candidate`. Both call `shared_work`, then the baseline calls
`old_work` and the candidate calls `new_work`. Each was recorded at `task-clock:u` with frame
pointers. Its `perf script --show-lost-events -F comm,pid,tid,time,event,ip,sym,dso` output was
kept, along with its `perf buildid-list`.

- `baseline.perf-script.txt` holds two whole sample records selected from the baseline script
  (8,861,363 bytes, SHA-256 `f0c873c2…8e2e`): the `old_work` record at byte 0 (901 bytes) and the
  `shared_work` record at byte 4,505 (904 bytes).
- `candidate.perf-script.txt` holds two from the candidate script (8,862,943 bytes, SHA-256
  `f34adcb9…f7b9`): the `new_work` record at byte 9,040 (901 bytes) and the `shared_work` record at
  byte 0 (904 bytes).
- `*.buildid-list.txt` are the two runs' `perf buildid-list` outputs, unchanged.

**One normalization.** In each record, the machine-specific directory of the workload binary,
`/run/host_virtiofs/<checkout>/linux-perf-stack-fixture/`, became `/fixture/`. That is 8
occurrences per file. Nothing else changed: the sample header format, the addresses, the
padding, the event and the libc frame perf left `[unknown]` are as perf wrote them. The original
excerpts were 1,805 bytes each:

| Side | Original SHA-256 | Committed SHA-256 |
| --- | --- | --- |
| baseline | `f8eab97e14949f3d93503cc091da7abd90e9e8ed406ebe13ce9053cb2ed10e19` | `c930ca38dbec5bd6a1705e6f5db894a1999119527ca8ac0bb1fb21535cbd4a5e` |
| candidate | `231c69ca22a3fc576a8f96c1f156072dd567cca7afc4c9c74015e51f35d616f6` | `b4b641dacf5a34388900e816802460f02c1c9ca2bb4ce0ddfab41693103471f1` |

`scripts/performance/tests/test_diagnostics.py` (`RealToolFixture`) reads them. It checks
samples, frames, the unknown libc frames, the event, the build-ID parser, and the collapsed
stacks and both difference directions. The collapse and the difference there come from the
stand-in inferno tools.
