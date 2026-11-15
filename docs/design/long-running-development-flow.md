# Proposed long-running development-flow performance workload

The opt-in `e2e_long_running` tier ports the default-light development fixture onto current
native Task and Review contracts. It is available for manual proposed-authority and calibration
work only. Standard quick/nightly/release selection, minimums and statistical algorithm are
unchanged. The [proposed decision](../adr/2026-11-10-separate-long-running-e2e-performance-cases.md)
does not activate a policy or authorize publication.

## Workload and bounds

`dev_default_light_smoke` has cold and primed initial-cache instances. Each performs three real
Tasks with two Findings-driven continuations, using independent model bindings for implementation
and verification. Only the Codex Providers are mocked. Native checks compile and test three Cargo
crates; each correction changes the next dependency closure. Final application semantics are
support = 2, logic = 4 and feature = 5. Retained evidence includes the full public command sequence,
Task/check/Finding documents, Cargo artifact freshness and mock business-role counts.

Each instance freezes eight measured pairs, two warmup pairs and balanced AB/BA order. The
`paired_median_tolerance_long_running_v1` algorithm is separate from the standard algorithm.
Samples retain their 120-second deadline, shards 90 minutes, total execution 180 minutes and native
cold builds 45 minutes, explicitly declared only by `tiers.e2e_long_running.build_minutes`.
The global `limits.build_minutes` remains 20 for quick, nightly and release; only the opt-in
long tier may override it, up to the unchanged 45-minute hard maximum. Planning freezes the
selected allowance into the run's existing `limits.build_minutes`; serial admission and builds
consume that same value. These are bounds, not promised completion times; lifecycle overhead, setup,
sentinels and cleanup still consume their existing reservations. Incomplete schedules fail closed.
No minimum, deadline, threshold or Attempt allowance is relaxed for completion.

`AF_PERF_CALIBRATION_ITERATIONS` is absent or a canonical integer from 0 through `2**40`.
Absence and zero stay distinct. The timed driver removes it from every nested environment and
compiles one fixed-size Rust constant-evaluation source outside Task source and Build Caches.
Prime-only preparation performs no injection. Compiler output and evidence remain bounded; the
sample's ordinary supervisor owns interruption, lifecycle verification and scratch cleanup.

## Exact evidence handoff

Supervisor request `/5` and result `/6` frame a sorted declared closure of regular files after
measured child exit and verified-empty namespace, before scratch clearing. Capture refuses unsafe
paths, symlinks, hardlinks, missing files, changing lengths and overflow. Limits are 64 files,
1 MiB per file and 16 MiB total. A refused transfer contains no partial bytes. The host refuses
undeclared paths, mismatched lengths and unconfirmed lifecycle evidence, then applies the same
strict flow oracle used for local files. Both paths bind the recorded cache treatment to the
planned instance's `initial_cache`, independently of the stdout summary. It never substitutes
the stdout summary or reads live container scratch paths.

Run `/8`, verdict `/3`, report `/4`, suite `/6`, CLI registry `/8`, cohort `/5`, local cohort `/2`
readiness `/7`, local readiness `/2` and Scenario completion/verification `/2` travel with current readers, writers, generated schemas
and parity fixtures. Prior versions fail closed under
[ADR-0145](../adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md).
Root `.af/` and `.github/` authority are unchanged.

## Source validation and operator follow-up

Focused tests cover package capture and native compilation, independent Worker declarations,
continuation inputs, unchanged caps, standard consumers, strict flow failures, dose isolation,
framing, unsafe evidence files and Docker provisioning/cleanup. They do not require host
unprivileged user namespaces. The four actual-supervisor cases (cold, primed, missing evidence,
unconfirmed cleanup) run in a provisioned fixture-only Docker container through the selected
live perf smoke and `make perf-dev-flow-probe`. Its read-only fixture binds are explicitly readable;
its namespace capability is never granted to production sample containers.

Provisioned Docker execution, full host performance tests and measurements of this exact port are
post-delivery operator checks. The supplied reference branch completed its local records with an
inconclusive A/A verdict; those observations are reference behavior, not measurements of this
port. No speedup, readiness, owner acceptance or activation is claimed.
