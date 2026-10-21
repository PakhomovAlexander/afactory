# ADR-0175: Supervise every diagnostic execution, admit what it wrote, and record what it used

**Status:** accepted (2026-10-20). Closes the recovered package p5 (traces, Linux profiles and
diagnostic counters) of the [continuous performance laboratory](../design/continuous-performance.md).
Its decisions stand except where this ADR amends them:

- [ADR-0174](0174-diagnose-in-separate-child-runs-that-record-only-the-bracketed-region-and-share-one-test-executable-boundary.md)
  decisions 2 to 5 and 9: the evidence contract, the repetition, losses, budgets, sanitizing
  and tool runs.

| Contract | Version | What changed |
| --- | --- | --- |
| Profiles | `af.perf-profiles/3` | replaces `/2`, which is refused |

## Context

The p5 implementation was recovered from a Worker that timed out. Its source was captured
uncommitted and never reviewed. Root checks of that exact source found nine gaps:

1. **A forked supervisor.** `perflab.toolrun` started tools with its own `Popen`. A child that
   never read 1 MiB of stdin ran 1.03 s past a 0.05 s deadline and returned success. So did a
   parent whose descendant held its pipes (1.05 s past 0.2 s).
2. **Clamped bytes.** `Budget.record` reported 8 bytes used when 16 were held under an 8-byte cap.
   The bracketed stdout cap was 64 KiB per repetition, up to 131 MB at 2,000 repetitions.
3. **Unadmitted files.** `run_probe` read a trace symlink the candidate made, returning the
   bytes of a file outside its namespace.
4. **No coordination, no containment.** `run_profile` took no machine lock. Every execution
   was a host `Popen`, even for a plan whose environment is the contained Linux profile.
5. **Mixed losses.** `parse_lost` summed `perf report --stats` record counts (1 + 1) with the
   per-event payload (333): 335. The retained injected fixture holds 777 lost events and 333
   potentially lost samples, one record each. 1110 is only their arithmetic sum.
6. **A blocklist sanitizer.** It kept a CSS `image-set("…")` string URL and an element in the
   XHTML namespace.
7. **Warm repetitions.** One probe process looped `protocol::run`, while the case declares
   `process = fresh` and its message says so.
8. **CLI cases.** Every CLI case was omitted as `cli_case_unsupported`, which mixed a missing
   implementation with an unavailable host.
9. **Zero meant default.** An explicit `0` for frequency, duration or minutes became the default.

The tree also lacked `fixtures/performance/diagnostic/real-perf-script.txt`, which a test of the
recovered source read. That is a real capture of the AF probe, so it cannot be regenerated here.

## Considered options

1. **Fix `toolrun`'s own deadline, stdin and drain logic.** Rejected. ADR-0026 puts process
   supervision in one leaf. A second copy is what produced finding 1.
2. **Every tool and candidate as a request to the authority `perf-supervisor`.** Chosen. This is
   the same hashed binary that brackets verdict samples.
3. **Extend the supervisor request for long or large tool output.** Rejected for now: a new
   request version for every shard. A tool whose output exceeds a stream cap writes a new file
   instead (decision 3).
4. **A new `perf record` per fresh repetition.** Rejected again: it records nothing for short
   regions (ADR-0174).
5. **One recording whose repetitions are fresh executions of the probe's image.** Chosen. The
   kernel task and its perf event survive `execve`, so the sampling period accumulates. No
   process state survives it.
6. **Count losses from `perf report --stats`.** Rejected. Its aggregate section counts records,
   its per-event section shows only the LOST_SAMPLES payload, and neither shows LOST's payload.
7. **Decode the loss records from `perf.data` itself.** Chosen.
8. **Run the profiler in the sample container now.** Not chosen. It needs a writable output
   bind beside the cleared scratch, the profiler in the image and the FIFOs across the bind. None
   of that can be verified on this host, which has no qualified 2-GiB scratch filesystem.

## Decision

1. **One supervisor** (`toolrun`).
   - **Every execution is a request.** Each diagnostic tool, probe, seed writer, profiler,
     Callgrind and DHAT run, and each capability or version check, is one
     `af.perf-supervisor-request/4` to the authority supervisor of the verified build.
     `toolrun` builds the request, launches it through `supervise.launch` and reads the reply.
   - **One absolute deadline.** A request gets the time before the child run's deadline, less the
     supervisor's cancellation reserve. Nothing launches without that time. A reply after the
     deadline is a failure whatever it says.
   - **Lifecycle and streams.** A truncated stdout, a refused lifecycle, an uncertain cleanup or
     a cancellation is a failure. An uncertain cleanup keeps the namespace.
   - **Cancellation.** A refused handshake or a budget stop cancels a running request with
     SIGTERM to its supervisor, which ends the run (ADR-0171).
   - **Large outputs.** A tool's stdout beyond a stream cap goes to a new file: `/bin/sh -c 'set
     -C; exec "$@" >"$0"'`. The supervisor and everything it launches get `RLIMIT_FSIZE` set to
     the evidence room left, so an output cannot outgrow that room.
2. **One machine, an explicit mode** (`cli.py profile --mode`).
   - **The machine lock.** The child run takes `hostlock`'s machine lock before its directory
     exists and holds it to the end. Another holder refuses the run.
   - **The mode is required.** `trusted_local` runs everything on this host under the
     supervisor's process-group containment. `profiles.json` records the mode, the
     supervisor's SHA-256, the lock, the containment the plan's environment declares and whether
     they match.
   - **Contained Linux.** `linux_container` is refused as `contained_diagnostics_not_implemented`.
     It never becomes host execution under another name.
3. **Admission and actual bytes.**
   - **Confined reads.** Every file a tool or candidate wrote is opened through
     `admission.open_confined` before any read, hash, copy or render. That covers perf data,
     scripts, collapsed stacks, SVGs, traces, Callgrind, DHAT and samply outputs. A symlinked file
     or ancestor, a FIFO, a device or a logical size beyond its bound refuses. A sparse file
     counts at its logical size. Evidence directories are enumerated with `admission.scan`.
   - **The evidence cap.** It holds an 8-MiB administrative reserve for `profiles.json`, the page
     and the manifest. `finish` checks that they fit.
   - **Charged before writing.** The driver charges every file it writes first, including
     messages, symbols, folded copies, sanitized SVGs and traces. Profilers and tools are bounded
     by the room (`--max-size`, `RLIMIT_FSIZE`).
   - **Usage as measured.** `budgets.bytes` records the evidence bytes actually held, the reserve
     and any overage. Nothing is clamped or deleted to fit.
4. **Fresh executions, judged as they arrive** (probe `perf-control`, `perfcapture`).
   - **The channel.** A driven probe names a third variable, the request file. It sends each
     repetition's message on the handshake channel as `record N` and its bytes, never on stdout,
     then `next`.
   - **Each message.** The driver judges it against the golden entry, appends it to
     `messages.jsonl` charged first, replaces the scratch and seed copy, and answers `again`.
   - **A fresh execution.** On `again` the probe executes its own image with the request file as
     stdin.
   - **A failure before the region.** A probe that fails before its region delivers its
     message instead of `begin`. That message is judged and the capture fails with it.
   - **The record.** A capture records `process: fresh_exec`. A recording stops at the duration,
     the repetition cap, the budget, or when another repetition could outlast the supervised
     window (`supervised_window`).
5. **Losses from the recording.**
   - **The decoder.** `decode_losses` reads the little-endian `perf.data` header and walks its
     data section record by record. It keeps `PERF_RECORD_LOST` records and their lost events
     apart from `PERF_RECORD_LOST_SAMPLES` records and their potentially lost samples, and never
     sums them.
   - **Unknown, not zero.** Another format, an unfinished header, compressed records or a
     malformed record makes all four counts unknown with the reason.
6. **A safe SVG subset** (`svgsafe`).
   - **Namespaces.** The parse is namespace-aware. Only SVG-namespace elements of the allowlist
     are kept, and an element of any other namespace goes with its content.
   - **Attributes.** Only unqualified attributes and inferno's `fg:x`/`fg:w` are kept, each
     matching its own value grammar.
   - **Declarations.** The output declares only the SVG namespace and, when used, `fg`.
   - **CSS.** A `<style>` is kept only when all of it parses as plain selectors with allowlisted
     properties whose values are keywords, numbers, colors, `rgb()` or a quoted plain font name.
     Any other function, at-rule, escape or comment removes the block.
7. **Explicit bounds.** An explicit frequency, duration, total minutes, case count or byte cap of
   zero, a negative value or one beyond its hard maximum is refused before anything is created.
   Only an absent value takes the default.
8. **CLI cases.** They are omitted as `cli_case_not_implemented` with that reason. An
   unavailable host capability keeps its own reason.

## Consequences

- **Tests.**
  - **The real supervisor** (`crates/review-process/tests/diagnostic_tools.rs`).
    `perflab.toolrun` runs through the built supervisor. A child that never reads its stdin, a
    child whose descendant holds its pipes, a child that leaves a live descendant, output over
    the cap and a cancellation each end within the deadline. None leaves a descendant, and none
    but an accepted lifecycle succeeds.
  - **The stand-in supervisor** (Python). It forwards a cancellation to the child's session.
  - **Python cases.** Repetitions as fresh executions, and a warm repeating probe refused.
    Losses decoded from 777 and 333 payloads, with unknown on undecodable input. Admission of
    symlinks, ancestor symlinks, FIFOs, devices and sparse sizes. A symlinked or FIFO trace never
    read. Actual byte usage with its overage.
  - **SVG and CSS.** Namespace switches, prefixed script, `image-set` string URLs, escapes and
    comments.
  - **Commands.** Explicit zero and over-limit bounds, the required mode, the contained refusal
    and the machine lock.
- **Fixtures.** The profile fixtures are version 3, with five new refusals. The test of the
  missing real-capture fixture is removed, not faked; a real symbolized capture remains an
  operational acceptance of package p8.
- **The real probe, driven.** A `perf-control` build of the actual probe ran 5 repetitions of
  the small canonical case on macOS, under the built supervisor and without a profiler. Each was
  a fresh execution, each message passed the golden oracle, and stdout stayed empty. A driver
  that skipped the refresh had its second execution refuse the stale scratch, and the probe's
  pre-region message was judged and reported.
- **The real loss fixture.** `decode_losses` read root's retained injected perf 6.1 recording
  (`a41023…95b4`): 1 LOST record of 777 lost events and 1 LOST_SAMPLES record of 333
  potentially lost samples. That file is not committed.
- **Pinned inferno.** Real inferno 0.12.8 output, both sides and both difference directions,
  passes the new sanitizer with its style kept and its script removed. The reverse direction
  keeps the stack that disappeared.
- **Still pending.**
  - **Contained Linux diagnostics.** No qualified 2-GiB scratch filesystem exists on this host.
  - **CLI-case diagnostics.** Startup tracing and the state-read cases are not implemented.
  - **Operational evidence.** Real captures and overhead remain package p8's.
- **Unchanged.** The 15-second lease, the 8-MiB envelope cap, every verdict bound and the
  supervisor's request and result contracts.
