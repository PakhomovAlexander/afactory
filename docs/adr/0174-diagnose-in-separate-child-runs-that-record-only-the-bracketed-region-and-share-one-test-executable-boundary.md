# ADR-0174: Diagnose in separate child runs that record only the bracketed region, and share one test executable boundary

**Status:** accepted (2026-10-20). Implements package p5 (traces, Linux profiles and diagnostic
counters) of the [continuous performance laboratory](../design/continuous-performance.md). It
first closes the surviving root finding of the unaccepted P4 closeout. Its decisions stand
except where this ADR amends them:

- [ADR-0173](0173-own-the-supervisor-a-test-runs-and-bound-admission-collection-scratch-and-cancellation-by-absolute-deadlines.md)
  decision 1: the test's lock and snapshots now belong to every crate's tests.
- [ADR-0162](0162-freeze-performance-plans-and-judge-them-by-exact-paired-verdicts.md): the
  profiles contract.

The contract versions are:

| Contract | Version | What changed |
| --- | --- | --- |
| Profiles | `af.perf-profiles/2` | replaces `/1`, which is refused |
| Diagnostic registry | `af.perf-diagnostics/1` | new |
| Phase trace | `af.perf-trace/1` | new |

## Context

**The surviving finding (p4-closeout-root-1).** ADR-0173 gave `review-process`'s supervisor
test a lock and snapshots. `review-pipeline/tests/perf_probe.rs` still built the same
`perf-supervisor` example, `perf-probe` and `af` with plain `cargo build`. It then started
cargo's uplifted paths, without that lock.

- **Two callers, two locks.** The pipeline caller could replace the supervisor test's path
  during its copy, or start a path while another test rebuilt it.
- **Observed here.** A fresh `cargo build --bin af`, as the pipeline test runs it, replaced
  `target/debug/af`'s inode. That is 686305608 before and 686401599 after, with the same bytes.
  `af`'s own integration tests start that path as `CARGO_BIN_EXE_af`, so a third caller met the
  same race.

**Package p5.** The design (§9, §14) asks for:

- separate diagnostic runs that never change a verdict;
- regressed-then-inconclusive selection;
- Linux perf at 997 Hz for 10 s per side, with one DWARF alternative;
- pinned inferno flamegraphs in both difference directions;
- default-off phase spans, with their overhead measured;
- optional counters and samply.

The task also requires that a capture never presents fixture setup as attribution of the timed
region.

**What the configured Linux executor showed.** This host's Docker Desktop VM runs the root's
pinned perf image (`sha256:7f04efb3…`, perf 6.1.190, aarch64, `perf_event_paranoid=2`). It runs
with no network, no capabilities and no new privileges, and these were real observations there:

- **The capability probe works.** An acknowledged capture of a shell loop decoded 263-308
  `task-clock:u` samples in its 0.3 s enabled window. The acknowledgement bytes were exactly
  `ack\n\0`.
- **A fresh `perf record` per repetition records nothing.** 1,639 repetitions of the small
  canonical case, each its own process, enabled about 2 s in all and recorded **0 samples**.
  Each new event starts its sampling period afresh, and these regions run for microseconds.
- **One process with fresh state does sample.** After the change below, the issue-255 large
  serialize case recorded 9,116 samples in 109 repetitions over 9.59 s of region. Its 135 s of
  setup was never sampled.
- **Small regions stay below any useful count.** The small case still reached only 22 ms of
  region in 2,000 repetitions, about 11 µs each, and 1 sample in 206.
- **A silent fallback.** `perf record -e cycles:u` exited 0 and recorded `cpu-clock:uH`
  samples. That event is unsupported there (`perf stat` prints `<not supported>`), and perf
  substitutes another with only a warning.
- **A blank build ID.** `perf buildid-list` pads a DSO without a build ID with blanks. Docker
  Desktop records the probe at its `/run/host_virtiofs/…` path, so perf finds its symbols and ID
  only when the binary is mounted at that path too.

## Considered options

1. **Keep each crate's own lock.** Rejected. Two locks order nothing between the crates.
2. **Copy `af` into each `af` test process's own snapshot.** Rejected as too costly. The binary
   is 118 MB, and there are hundreds of test processes.
3. **One boundary for builders and launchers, with each process's own hard link.** Chosen.
   Builders build under the exclusive lock and copy into byte-verified snapshots. A launcher
   that builds nothing takes the lock shared and links the file it finds.
4. **Profile verdict samples.** Rejected by the design: verdict binaries stay uninstrumented.
5. **Capture the whole process, then filter stacks to the region.** Rejected. Setup and region
   share frames, and a filter cannot prove what ran when.
6. **Start recording disabled and bracket each region with perf's acknowledged control.**
   Chosen.
7. **A new `perf record` per repetition.** Rejected by the observation above: it recorded
   nothing.
8. **One profiled process that repeats on fresh state the driver provides.** Chosen.
9. **Render flamegraphs ourselves.** Rejected by the design: pinned inferno renders them.
10. **Trust `--no-javascript`.** Rejected. The SVG still carries `onload` and a `<script>`.

## Decision

1. **One executable boundary for every crate's tests** (`crates/review-process/tests/support/
   executables.rs`, which `review-process`, `review-pipeline` and `af` tests include).
   - **The place.** One directory per profile, `<profile>/af-test-executables/`, with one lock.
   - **A builder** (`built`). It builds under the exclusive lock, exactly as ADR-0167 built.
     Still under the lock, it copies the output into a snapshot named by its bytes, verified
     before its rename. This covers both supervisor tests, the pipeline's probe and its `af`.
   - **A launcher** (`held`). It takes the lock shared and links the file cargo built. This is
     how `af`'s tests start `CARGO_BIN_EXE_af`.
   - **What a test starts.** Only this process's own hard link, `<snapshot>.<pid>` or
     `<name>-held-<dev>-<ino>.<pid>`, the same inode whatever replaces the name. A filesystem
     without hard links gets a verified snapshot instead.
   - **Clean-up.** A link of an exited process is removed by the next taker of the lock, and a
     snapshot no longer current by the next build. A running process keeps its inode.
   - **Unchanged.** A test binary started without cargo uses the file beside it. The
     cross-target layout, the assertions and the build provenance are as before.
2. **Separate child runs** (`cli.py profile`, `perflab.diagnostics`).
   - **The child plan.** A frozen plan of scope `diagnostic` with its parent's identities and
     build, and a diagnostic recipe. The recipe comes from the authority's
     `fixtures/performance/diagnostics.toml`.
   - **The recipe.** A verdict recipe with only features, symbols, frame pointers and
     `instrumented` changed. Only a diagnostic plan builds one, and its features reach the probe
     too.
   - **Selection.** Explicit `--case`, at most 8. Or the parent verdict's regressed cases, then
     its inconclusive ones, each class by instance ID, 3 by default and 8 at most.
   - **The budgets.** One wall deadline (15 minutes by default, 30 at most), a byte cap
     (128 MiB, at most the 512 MiB bundle cap), the case cap and a build cap. Each is checked
     before every step.
   - **Clean-up.** Only the run's own `.work` is removed, and it is kept when a scratch's
     removal was unconfirmed.
   - **The evidence.** `profiles.json` (`af.perf-profiles/2`) records `verdict_effect: none`,
     with every capture, render, trace, counter and omission. Each omission names its exact
     reproduction. A page and a bundle manifest sit beside it.
3. **Only the bracketed region is recorded** (the probe's `perf-control` feature, `perfcapture`).
   - **The bracket.** `perf record --delay=-1 --control=fifo:CTL,ACK` starts disabled. The
     probe sends `begin` and `end` around its region, outside its clock. The driver enables or
     disables, waits for perf's acknowledgement and answers `go`.
   - **Repetition on fresh state.** After each repetition's message the probe sends `next`. The
     driver replaces its scratch and seed copy with new ones and answers `again` or `done`, so
     one perf event spans every repetition.
   - **When it stops.** At the probe-measured region time, the repetition cap (2,000), the
     budget, or when enabled recording reaches the duration. A region still running then is
     disabled on time.
   - **What is judged.** Every repetition's message, against the golden entry.
   - **What is recorded.** Setup, region and enabled times, and why the capture stopped.
   - **Refusals.** An early exit, a missing or wrong acknowledgement, and a deadline each end the
     capture with its reason. The probe's stderr and stdout are kept in the omission.
4. **Captures and their honesty.**
   - **The defaults.** 997 Hz and 10 s, within the 1,999 Hz and 30 s caps. One declared event,
     `task-clock:u`.
   - **No substitution.** Every sample's event in the script must be the declared one. A
     capability check or capture whose samples name another event is refused as
     `event_substituted`: the real `cycles:u` check is.
   - **The unwinder.** Frame pointers first. The one DWARF alternative, 32 KiB of stack, runs
     only when the unknown-frame ratio is above the declared 0.25. Both captures are kept.
   - **Counted from the script.** Samples, frames and unknown frames come from `perf script`
     with explicit fields, never from folded names.
   - **The rest.** Lost-event records come from `perf report --stats` and build IDs from
     `buildid-list --with-hits`, with the binary's own GNU note checked among them. The
     effective frequency is samples per second of region.
   - **Symbols.** The binary is retained when the byte cap allows, else referenced by its
     artifact hash.
   - **Too few samples.** Below the declared minimum of 200, a capture is kept but rendered from
     nothing.
   - **The byte cap.** `perf record --max-size` gets the cap's remaining room, so perf ends its
     own session there. Such a capture is kept and recorded as `profile_bytes_exhausted`.
5. **Flamegraphs** (`flamegraph`, `svgsafe`).
   - **The tools.** inferno 0.12.8, pinned by its crate checksum. Its binaries refuse
     `--version`, so their provenance is `declared_pin_only` plus each executable's SHA-256.
   - **What is rendered.** Each side, and the normalized difference in both directions, the
     reverse with `--negate`, so stacks the candidate lost stay visible. Color is the change in
     CPU sample share.
   - **Sanitizing.** The original SVG is kept and hashed. The sanitized copy is rebuilt by an
     allowlist parse that drops scripts, handlers, foreign content, links and unsafe CSS. It
     refuses DTD subsets, entities and external references.
6. **Default-off phase spans** (`review_core::perf_trace`, feature `perf-trace`).
   - **The spans.** One closed `Phase` list across startup composition, canonicalization,
     envelope validation, CAS get and put, projection, the authority check, `TaskRuntime::new`,
     and the probe's setup and region.
   - **No product cost.** Without the feature a span is a zero-sized guard with no destructor.
   - **Bounded.** 65,536 spans. A span beyond the bound is dropped and counted, with everything
     nested in it.
   - **Metadata only.** The export holds the PID, the recorder's thread numbers, span and parent
     IDs, monotonic nanoseconds and the dropped and open counts, written once to a new file.
   - **Perfetto.** The driver checks nesting per thread. Spans that nest are complete events,
     and anything else goes on an explicit async track. Phase totals are unions, never sums.
     The offline page draws a timeline.
7. **Overhead is measured, and labelled.**
   - **How.** Paired, alternating runs of the verdict build's probe and the trace build's, from
     the same source, 12 pairs by default. Both recipe digests and binaries are named.
   - **The result.** The exact median of paired relative overheads. Above the 2% target a trace
     is labelled `perturbing` and kept.
8. **Optional counters and samply.** These are explanatory proxies, never wall time.
   - **Callgrind.** Instructions of exactly the bracketed region, its instrumentation switched
     with `callgrind_control`.
   - **DHAT.** Allocations and bytes of the whole process, labelled so.
   - **samply.** Saved without a viewer. Its capability reads its own `record --help` and
     refuses system-protected and hardened-runtime targets. It is labelled whole-process.
9. **Tools are configured, never installed.**
   - **Configuration.** Every executable is named explicitly (`--perf`, `--inferno-bin`,
     `--samply`, `--valgrind-bin`); nothing is searched for on `PATH`.
   - **Provisioning.** Each tool carries its instructions in the registry.
   - **Nothing elevated.** Nothing runs sudo, changes a sysctl, grants a capability, signs,
     opens a browser or uploads.
   - **No capability.** A missing capability yields an unavailable state, its reason and a
     reproduction command.

## Consequences

- **Gates.** `make lint` also lints the diagnostic features as the recipes build them and runs
  the recorder's feature-on tests. No other build or test enables them. The 15-second lease,
  the 8-MiB envelope cap and every verdict bound are unchanged.
- **Rust tests.**
  - **The boundary across crates.** A separate builder process holds the lock across a removal
    and copy. A launcher meets `NotFound` outside the boundary, and inside it waits and then
    links the finished file.
  - **Clean-up.** An exited process's link and a superseded snapshot are removed, and a live
    link keeps its bytes.
  - **The pipeline's executables.** Its supervisor, probe and `af` are all its own links in that
    directory, unmoved by a rebuild.
  - **The recorder.** Nesting, per-thread tracks, capacity drops with their children, out-of-order
    drops, payload exclusion, a write-once export, and a default build's zero-sized inert span.
- **Python tests.** The stand-in perf, inferno, valgrind, samply and probe speak the real
  protocols. Every child run they produce is synthetic.
  - **Selection and budgets.**
  - **The bracket and repetition.** A one-second setup is never sampled. Short regions repeat in
    one recording on fresh state, and a stale scratch or seed copy fails. Early exits, a silent
    perf and the recording cap are covered.
  - **Parsers.** Acknowledgements; script, lost-event and build-ID text, including the blank
    padding observed; malformed collapsed stacks; and bounded tools.
  - **Sanitizing and difference.** Thirteen kinds of active content are removed, and DTDs,
    entities and bad roots refused. Both difference directions keep a lost stack.
  - **Traces.** Track placement, union totals and exact overhead.
  - **Whole child runs.** CPU, seeded, trace, counter and unavailable child runs; the command's
    exit 4 with reproduction; and diagnostic builds' features, frame pointers and symbols.
- **Real evidence, not a performance claim.** The configured Linux executor described above
  ran actual AF probe builds made with the diagnostic recipes' settings in the root's shipping
  image. The capture path, the trace export and the overhead calculation are this ADR's own.
  - **Large serialize.** 9,571 and 9,470 samples on the two sides, over 9.95 s and 9.85 s of
    region. 0 lost, an unknown-frame ratio of 0.069, and every application frame symbolized; the
    unknown frames are libc's. The binary's own build ID was found, and no setup function
    appears in any stack.
  - **State projection, application cold.** The seeded case read a fresh copy of its side's
    written seed each time: 4,101 and 4,064 samples over 214 and 225 repetitions.
  - **The DWARF alternative.** Its 32-KiB capture took 2,707 samples with a 0.027 unknown
    ratio, against 0.069 with frame pointers. Its data reached 127.8 MB of the 128-MiB cap,
    which perf's `--max-size` enforces.
  - **Flamegraphs.** inferno 0.12.8 built by root rendered both sides and both differences of
    these real stacks, and the sanitizer accepted its real SVG.
  - **Trace overhead.** All 22 native instances were measured with 12 pairs each. Four are
    labelled `perturbing`: the small canonical serialize (+42%) and validate (+71%), whose
    regions last microseconds, the large raw CAS get (+3.8%) and the advisory persistence write
    (+2.4%). The other 18 are within the target, several below zero, which is noise on one
    virtual machine.
  - **The trace build.** `af --version` and `--help` exported their startup spans.
  - **Not retained.** This is not calibrated performance, a hosted runner or a dedicated
    executor. The numbers are in the task report.
- **Still pending.**
  - **The configured path.** The whole `profile` command, building in the pinned image on a
    configured executor, is an operational step of package p8. So is the shipping capability
    acceptance on a dedicated Linux executor.
  - **Optional adapters.** samply and valgrind are not installed here, so they are verified only
    against stand-ins.
