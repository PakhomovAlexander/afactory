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

## Decision

Bare `af` targets its first browser frame **strictly below 200 ms**, from process launch to
the first frame painting `NORMAL` on the last row of a 100x30 pseudo-terminal. The case uses a
shipping binary, 20 fresh processes, a private HOME, warm OS page cache, and both an empty
registry and a Store registry naming 20 copies of the laboratory's 10,000-event Task seed.
Every sample of both instances must be strictly below 200 ms; equality fails. The named Linux
and macOS nightly and release budgets start `pending_calibration`, as the existing discovery
and state-read targets do. The case's primary `wall_ms` is the driver's versioned
`browser_ms` reported interval; `splash_ms` separately records first output. The setup copies
the side's seed privately and uses that side's `af task list --state` to record each Store.
The driver timestamps the browser's initial screen clear, then admits it only when the
last-row `NORMAL` status line appears; the product emits that clear immediately before the
first browser paint. A missing line, expired deadline or failed quit produces no sample metric.
The driver writes canonical decimal millisecond strings so the laboratory's strict JSON reader
can retain exact decimal values. Its document is `af.perf-tui-open/1`; the suite, CLI registry,
frozen run and capability documents become `af.perf-suite/4`, `af.perf-cli/6`, `af.perf-run/7`
and `af.perf-capabilities/3` respectively. Earlier shapes fail closed.
The current `af_cli_v2` adapter runs both instances; `af_cli_v1` declares them pending until
that older command surface has a reviewed registry setup.

## Consequences

A missed target creates optimization work and no automatic budget activation. Once Task A
merges, the same case measures its repaired browser. The driver and registry are part of the
laboratory's reviewed authority; their new timing and CLI contract versions fail closed on
earlier shapes. Calibration and shipping demonstrations remain separate work.
