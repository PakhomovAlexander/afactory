"""AF continuous performance laboratory: contracts, registry, frozen plans, verdicts, oracles,
builds and supervision.

Packages p1, p2 and p3a of docs/design/continuous-performance.md. Planning reads Git objects and
comparison reads retained evidence; neither builds or executes a product. The reference
generator derives golden entries without running anything. `builds` builds a frozen plan's
products and its one authority supervisor; `supervise` launches that supervisor for one sample,
in trusted local mode or as PID 1 of a Linux sample container; the oracle's explicit `probe`
driver runs one diagnostic sample at a time. `report`, `history` and `archive` (package p4,
ADR-0172) render runs offline and keep their evidence, and `admission` bounds every file a
measured side wrote. `diagnostics` (package p5, ADR-0174) creates separate diagnostic child
runs: `perfcapture` records exactly a probe's bracketed region with Linux perf, `flamegraph` and
`svgsafe` render and sanitize inferno flamegraphs, `phasetrace` exports and checks phase spans,
`counters` runs the optional Callgrind, DHAT and samply adapters, and `toolrun` bounds every
external tool. Every document is closed and versioned (ADR-0162, ADR-0163, ADR-0166, ADR-0174).
"""
