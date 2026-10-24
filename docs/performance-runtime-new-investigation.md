# Investigation: the `authority_runtime_new` slowdown

The native case `authority_runtime_new` times the first `TaskRuntime::new` of a two-node graph
at both issue #255 package sizes, with a fresh valid lease
([`fixtures/performance/suite.toml`](../fixtures/performance/suite.toml)). This note separates
what was observed from what is hypothesized, gives the commands that reproduce every figure,
and names what was not measured. It claims no speedup: no optimization was made. The decision
to retain the entry point is
[ADR-0199](adr/0199-integrate-telemetry-into-the-performance-lab-as-diagnostics-and-advisory-experiments.md).

## Observed facts

**Historical comparison (given, not re-measured here).** A pull-request comparison
([Actions run 39732697041](https://github.com/PakhomovAlexander/afactory/actions/runs/39732697041))
of baseline `5d72fc22a3bac5d86a9c98fe9c4b1415feab8917` against candidate
`c39861e9b94b283ddbf1bfb234fe8ad949ed8345` matched all 1,872 records with none missing and
reported, for this case's medians:

| Package | Baseline | Candidate | Change |
| --- | ---: | ---: | ---: |
| `issue_255_medium` | 1,744.86 ms | 2,339.62 ms | +34.09% |
| `issue_255_large` | 4,345.32 ms | 5,783.22 ms | +33.09% |

That is historical pull-request evidence. It is not a measurement of current main, and it does
not identify a cause.

**The code path.** `TaskRuntime::new` (`crates/review-pipeline/src/task.rs`) calls
`with_store`, which first calls `begin_execution`. That takes the Store lock and calls
`EventStore::begin_task_execution` (`crates/review-store/src/store/task.rs`). It computes the
Task's whole projection (`task_projection`), returns early if this lease already recorded its
execution marker, and otherwise appends the one `TaskExecutionStarted@1` marker. The append
passes the projection it already computed (`append_batch_observed(..., Some(&state))`).
`begin_execution` then opens the run's telemetry (`RunTelemetry::open`), which returns a
disabled span without reading anything when no telemetry subscriber is installed, as in the
native probe. `with_store` then calls `check_current_task_plan_for_recording`, which projects
again, and then `task_projection` a third time. The marker and its projection run at every
telemetry level, by design (ADR-0153).

**Phase traces of the current source (measured here).** Six samples of the case were run, one
at a time, with a `perf-trace` build of the probe on this package's source. That source is main
`3bf55b26` as the Task names it, and this package changed no product code. The host was a macOS
arm64 development laptop with 14 logical CPUs, shared with other work, at load averages of 92
to 205. The probe was built by the host's Homebrew cargo 1.94.0, not the repository's pinned 1.88.0.
These are diagnostic builds on a heavily loaded host, not verdict samples. Their
absolute times say nothing about speed; only the shares within one region are used. The region
(`task_runtime_new`, 100% of the probe region) broke down as follows. The derived figures and
each raw trace's SHA-256 are retained in
[`performance-evidence/runtime-new-20261022/breakdown.json`](performance-evidence/runtime-new-20261022/breakdown.json).

| Sample | Region | 1st projection (`begin_task_execution`) | 2nd (plan check) | 3rd (`with_store`) | All projections |
| --- | ---: | ---: | ---: | ---: | ---: |
| medium, first | 4,939.9 ms | 2,047.8 ms (41.5%) | 1,044.3 ms (21.1%) | 1,607.2 ms (32.5%) | 95.1% |
| medium-1 | 2,219.4 ms | 908.4 ms (40.9%) | 502.2 ms (22.6%) | 622.1 ms (28.0%) | 91.6% |
| medium-2 | 1,560.8 ms | 537.8 ms (34.5%) | 372.8 ms (23.9%) | 425.2 ms (27.2%) | 85.6% |
| medium-3 | 4,976.1 ms | 1,735.3 ms (34.9%) | 1,433.7 ms (28.8%) | 1,423.2 ms (28.6%) | 92.3% |
| large-1 | 6,788.4 ms | 2,962.9 ms (43.6%) | 1,875.1 ms (27.6%) | 1,537.9 ms (22.7%) | 93.9% |
| large-2 | 7,167.1 ms | 3,311.0 ms (46.2%) | 1,119.5 ms (15.6%) | 2,235.4 ms (31.2%) | 93.0% |

- In every sample the region is three whole projections of the Task log and little else. Each
  projection reads and validates the package envelopes from the CAS (`cas_get`,
  `envelope_validate`, `canonicalize` spans below it).
- The first projection, the one `begin_task_execution` makes before the marker is appended,
  took 34.5% to 46.2% of the region.
- Between the end of the first projection and the start of the plan check, the marker's
  encoding, its append and the disabled `RunTelemetry::open` took 0.4 to 3.4 ms. The marker's
  write and the telemetry opening without a subscriber are not where the time goes.
- A seventh sample (large-3) failed with `lease_expired`. On the loaded host its first two
  projections took 14.9 s, past the production lease the probe had taken, and the Store fenced
  the next write. A slow runtime construction can therefore outlast the Task lease.

## Hypotheses (not established)

1. The historical +33–34% comes from the extra projection that `begin_task_execution` added
   when the execution marker was introduced: two projections became three. The arithmetic does
   not support this hypothesis on its own:
   - If the three projections cost the same and are nearly all of the region, three where there
     were two is +50%: removing one third of the current work gives a current/previous ratio of
     1 / (2/3) = 1.5, not the historical 1.33–1.34.
   - The recorded first-projection share of 34.5–46.2% would, if that projection alone were
     the added work, give a ratio of 1 / (1 − 0.345) ≈ 1.53 to 1 / (1 − 0.462) ≈ 1.86.
   - A ratio of 1.33 corresponds to added work of about one quarter of the current region
     (1 / (1 − 0.25) ≈ 1.33).

   So the first projection's share alone does not explain the historical change. The
   projections may not cost the same in both revisions, the older revision may have done other
   work this one does not, or the shares measured on a loaded laptop may not hold on the hosted
   runner. Measuring both historical revisions with the trace build is the check; it was not
   done ([Not measured](#not-measured)). The commits between them may hold other changes.
2. The third projection in `with_store` could reuse the state `begin_task_execution` already
   computed and advanced with the marker. That is only valid if nothing can append between
   the lock's release and its re-acquisition, and if `apply_execution_started` leaves exactly
   the state a fresh projection of the committed log gives. Neither has been proved.
3. A projection cache in `EventStore`, keyed by the Task log's latest committed sequence, could
   serve all three projections from one read. It would have to keep lease fencing, the
   marker's identity, durability and replay exactly as they are.

No optimization was made. Both candidate changes touch the Store's execution authority, and
neither has the semantic tests (projection equality after the marker, a concurrent writer
between the lock releases, replay of the marker) that would show it preserves behaviour.

## Reproduce

The bounded entry point is the trusted local probe with a phase trace (ADR-0199). It writes
the measured request's validated trace and prints its phase summary and the region's
breakdown. It never records a verdict sample.

```sh
cargo build --locked --release -p review-pipeline --example perf-probe --features perf-trace
cargo build --locked --release -p review-process --example perf-supervisor
mkdir -m 700 /scratch/af-runtime-new
python3 scripts/performance/cli.py probe --trusted-local \
  --supervisor target/release/examples/perf-supervisor \
  --binary target/release/examples/perf-probe \
  --instance authority_runtime_new__package_issue_255_medium \
  --scratch /scratch/af-runtime-new --timeout 120 \
  --trace-output /scratch/af-runtime-new-medium.json
```

To test hypothesis 1, use the laboratory's own path. Freeze a partial-scope comparison of the
two historical revisions for this case, build it, and run a separate diagnostic child with the
trace recipe. The child records the same breakdown for both sides and their trace-on/off
overhead against the verdict build:

```sh
python3 scripts/performance/cli.py plan --trigger manual --tier quick --scope partial_scope \
  --base 5d72fc22a3bac5d86a9c98fe9c4b1415feab8917 \
  --head c39861e9b94b283ddbf1bfb234fe8ad949ed8345 \
  --environment macos_arm64_local --case authority_runtime_new \
  --output /scratch/runtime-new/run.json
python3 scripts/performance/cli.py build --plan /scratch/runtime-new/run.json --repo .
python3 scripts/performance/cli.py profile --plan /scratch/runtime-new/run.json \
  --case authority_runtime_new__package_issue_255_medium \
  --case authority_runtime_new__package_issue_255_large \
  --recipe macos_arm64_trace --mode trusted_local --output /scratch/runtime-new/trace-child \
  --parent-build target/performance/RUN_ID/build
```

On Linux, plan for the Linux environment and use `--recipe linux_gnu_trace` with the
contained mode that environment declares. These commands were not run for this note
([Not measured](#not-measured)).

## Not measured

- Neither historical revision was rebuilt or traced. The sandbox that recorded this note has no
  Git history to check them out from.
- The six raw traces are not in the repository. Only their derived figures and SHA-256 are, in
  `breakdown.json`; the coordinator of the Task retained the raw traces separately, with
  matching hashes. The limitations of those local measurements above still hold.
- No sample ran on a quiet host, in the hosted Linux environment or with the pinned
  toolchain, and no uninstrumented timing of the current source is reported.
- The native probe installs no telemetry subscriber, so telemetry's own share of runtime
  construction at `spans` was not measured here. The CLI command `cli_task_run_check` does
  record telemetry and can be diagnosed with `profile --telemetry spans`
  ([`performance-telemetry.md`](performance-telemetry.md)). Its `af.run` span's self time
  covers the projections of its own runtimes, but telemetry names no `task_projection` phase.
