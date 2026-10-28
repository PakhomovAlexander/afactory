# ADR-0258: Bound bare af's first browser frame

**Status:** proposed (2026-10-27). See [issue #402](https://github.com/PakhomovAlexander/afactory/issues/402).

## Context

The continuous performance laboratory already targets CLI discovery below 50 ms and state reads
below 200 ms. Bare `af` had no browser target. On the owner's machine on 27 October 2026, a
100x30 pseudo-terminal first painted the browser after 40.6 seconds with 166 Task event logs
totalling 4.0 GB; an empty private HOME took 0.13 to 0.33 seconds. Synchronous Store replay
before the first browser frame explains the gap. Task A of the splash campaign moves Store reads
behind that frame; this decision measures either implementation without assuming its repair.

## Considered options

1. Time the driver's child lifetime. That includes Python startup and quitting the browser,
   so it cannot apply the target to the interval the user sees.
2. Report the launch-to-frame interval from a reviewed pseudo-terminal driver. Chosen: its
   versioned output has a checked interval and a separate first-output time, while the existing
   supervisor still bounds and records the driver's lifetime.
3. Time the frame from the screen clear before it. Rejected: the clear's timestamp cannot be
   bound to the frame that later paints the status line, so a splash's clear could end the
   interval long before the browser is shown.

## Decision

Bare `af` targets its first browser frame **strictly below 200 ms**, from process launch to
the first frame painting `NORMAL` on the last row of a 100x30 pseudo-terminal. The case uses a
shipping binary, 20 fresh processes, a private HOME, warm OS page cache, and both an empty
registry and a Store registry naming 20 copies of the laboratory's 10,000-event Task seed.
Every sample of both instances must be strictly below 200 ms; equality fails. The named Linux
and macOS nightly and release budgets start `pending_calibration`, as the existing discovery
and state-read targets do. The case's primary `wall_ms` is the driver's versioned
`browser_ms` reported interval; `splash_ms`, launch to the first byte of any output, is the
case's declared optional metric `splash_ms`, which every successful sample records beside it
with the scope `probe_reported` and the run report carries in its verified sample evidence.
The setup copies the side's seed privately and registers each copy through that side's own
`af task gc --older-than 36500 --keep 1000 --state DIR` preview: every command that resolves a
`--state` Store records it (ADR-0144, `task_execution::state_path`), and the preview writes
nothing else. `af task list --state` only reads its Store and records nothing, so a registry it
was meant to fill stays empty; the setup checks that the registry names exactly 20 Stores.

The interval ends at the read that completes the last-row `NORMAL` status line among the bytes
after the screen's last clear. A pseudo-terminal carries no write boundaries, so the first byte
of the frame that paints the status line cannot be told apart from an earlier frame's: a clear
and a splash followed later by a status line, written without another clear, are one stream.
The status line's arrival is the earliest instant the browser frame is shown, and it is never
earlier than that frame's first byte, so a sample is never below the target because of the
driver; the product writes each frame in one write, so the difference is the transfer of at
most one frame. A status line an earlier clear erased never counts. A missing line, expired
deadline or failed quit produces no sample metric. The driver writes canonical decimal
millisecond strings so the laboratory's strict JSON reader can retain exact decimal values.

The browser needs a controlling terminal, so the driver starts it as the leader of a new
session, outside the supervisor's process group; the driver owns that session's lifecycle under
the supervisor. The child holds a duplicate of the driver's stderr, the supervisor's pipe, so
the supervisor's drain cannot end while any process of the session that kept it runs, and a
driver killed outright leaves the supervisor's cleanup uncertain rather than clean. The driver
observes the child's exit without reaping it (a kqueue `NOTE_EXIT` on macOS, `waitid` with
`WNOWAIT` on Linux), kills and settles every live member of its group while the leader's ID
still names only that group, reaps the leader only after that, and refuses the sample when a
member outlived the browser or a process still holds its terminal. A process of the browser's
doing that left its group by `setsid` and closed every descriptor it inherited is found all the
same: the browser runs with a token of the run in `AF_PERF_TUI_OPEN_SESSION`, and once the
session is settled every live process whose environment carries the token (`/proc` on Linux,
`ps -E` elsewhere) is killed and fails the sample. SIGTERM, SIGINT and SIGHUP, the supervisor's
cancellation, end the session the same way before the driver exits with 128 plus the signal. A
cleanup it cannot confirm, the listing included, exits 3, which stops the shard and keeps the
sample's scratch, as any uncertain cleanup does (ADR-0171). A Linux sample container's PID
namespace ends every process regardless.

The supervisor runs only an absolute program, so the driver's interpreter is the placeholder
`{python}`, named only as that driver's program: the laboratory's own interpreter in trusted
local mode and the sample image's `/usr/bin/python3` in a sample container. The driver's
document is `af.perf-tui-open/1`; the suite, CLI registry, frozen run and capability documents become
`af.perf-suite/4`, `af.perf-cli/6`, `af.perf-run/7` and `af.perf-capabilities/3`
respectively. Earlier shapes fail closed. The current `af_cli_v2` adapter runs both instances;
`af_cli_v1` declares them pending until that older command surface has a reviewed registry
setup.

## Consequences

A missed target creates optimization work and no automatic budget activation. Task A's repair
(ADR-0240) is in the tree this case first measured, and the case measures whatever binary it is
given. The driver and registry are part of the laboratory's reviewed authority; their new timing
and CLI contract versions fail closed on earlier shapes. Calibration and shipping demonstrations
remain separate work; the first real local run of both instances is retained in
[`performance-evidence/tui-open-local/`](../performance-evidence/tui-open-local/README.md) as
functional evidence, not a demonstration.

The driver's own wall, which the supervisor still bounds, includes its interpreter's start, the
quit and the session's cleanup; the budget never reads it. On macOS, as for any process-group
sample, a process that left the browser's session, closed both the supervisor's pipe and the
terminal and replaced its environment before `exec` is not observable; a Linux sample
container's namespace ends it.
