- The self-optimizer light-strategy test no longer holds every test thread for its whole run
  (#203, [ADR-0124](docs/adr/0124-run-tests-in-parallel-processes-and-link-them-once.md)). It
  is split into two tests in `crates/af/tests/it/self_optimizer.rs`. Each builds its own
  repository and Store with the same `af` commands through shared helpers. Of the two, only
  `light_cache_candidate_measures_real_latency_and_grounds_later_adoption_evidence` stays in
  the exclusive override of `.config/nextest.toml`. It first runs, delivers and adopts the
  light candidate without asserting on it, then runs every phase from line 719 to line 1277
  of the original test in order: the cache latency comparison (8 trials, 3 s baseline) with its
  replay, delivery, adoption and ordinary cache-consuming Task (719-1090), the unknown-toolchain
  comparison and refused delivery (1092-1228), and the light adoption observed with the cache
  Task as evidence (1230-1277).
  `light_strategy_generates_one_candidate_without_exposing_source_to_author_workers` runs in
  parallel and keeps every other phase in the original order on one repository: plan review
  without source (186-259), the approved five-Attempt run and its replay (260-376), delivery and
  adoption observations (378-530), source invalidation (532-571), the uneconomic recommendation
  (573-669), the unexecuted binding (671-717) and, after the same cache configuration and
  toolchain removal, the unsupported recipe (1279-1322). Every assertion of the original is in
  exactly one of the two, unchanged. The original repeated its `git` exit-status checks inline;
  they now live in one `commit` helper, so both tests check the setup commits they both make.
  No sleep, trial count, budget, retry or slow-timeout changed. The previous seven-test split
  had dropped the original's restore of the baseline worker without its sleep before the
  unknown-toolchain comparison. That comparison slept 3 s on every baseline trial and took
  20.2 s instead of 8.1 s. `remove_cache_toolchain` now restores the baseline as main does.
- Measured with temporary per-phase timers, since removed. Each test ran alone under
  `cargo nextest run --profile ci` on a 14-core macOS host after one warm-up launch of `af`.
  Figures are medians of three runs unless marked otherwise.
  - Main's single test took 60.3 s. Setup up to the adopted light candidate took 8.8 s:
    fixture 0.2, plan review 4.0, approval and run 3.5, delivery and adoption 1.1. Phases that
    read no comparison state took 13.3 s: replay 0.1, adoption observations 0.7,
    invalidation 2.2, uneconomic 5.7, binding 2.2, unsupported recipe 2.5.
  - The two real comparisons took 28.4 s: the cache `task run`, with its trials and 3 s
    baselines, 20.2 s, and the unknown-toolchain `task run` 8.1 s. Their configuration, plan
    and signed approval took 5.5 s.
  - The non-timed work that only reads the comparisons' state took 3.7 s: cache assertions 0.1,
    replay 0.1, delivery and adoption 1.5, the ordinary Task 1.3, unknown-toolchain assertions
    and refused delivery 0.1, and the light observation with cache evidence 0.6.
  - The previous split's exclusive test took 60.2 s: light setup 9.6, plans 5.5, cache
    comparison 20.5, unknown-toolchain comparison with the sleeps 20.2, reads 4.0. Its seven
    moved tests, each run once alone, took 37.0 s together: 10.7, 7.8, 6.0, 5.7, 2.4, 2.2 and
    2.2 s.
  - In the chosen pair, the exclusive test takes 45.5 s: light setup 9.6, cache plan 2.4,
    cache comparison 19.7, its reads 2.8, unknown-toolchain plan 2.5, its comparison 7.6, its
    reads 0.7. The parallel test takes 19.8 s.
- The decision uses the expected wall time W(T) = E*(T-1)/T + S/T, where E is the exclusive
  seconds and S the sum of all the light-strategy tests:

  | Variant | E (s) | S (s) | W(4) (s) | W(7) (s) |
  | --- | --- | --- | --- | --- |
  | Main's single test | 60.3 | 60.3 | 60.3 | 60.3 |
  | Previous seven-test split | 60.2 | 97.2 | 69.4 | 65.5 |
  | Chosen pair | 45.5 | 65.4 | 50.5 | 48.3 |

  The pair beats the single test by 9.8 s at T = 4, clearing the 5 s bar, so the split is kept
  in this form.
- `setup_repairs_auth_directory_and_lock_modes_under_a_restrictive_umask` stays in the
  exclusive override. Its work takes 0.37 s, but running first and alone it pays the first
  launch of a freshly linked `af`, which macOS assesses before it runs: 11.2 s measured
  straight after relinking, and every Task gate starts from a fresh build. Outside the override
  that cost lands on whichever parallel test launches `af` first, under the 15 s provider status
  probe deadline. In a before/after bench (one warm-up, then three alternating full-suite runs
  per side, 7 threads, 14-core macOS host), one of three runs without it failed that test and
  `status_keeps_a_default_context_whose_status_probe_failed` with "provider status probe timed
  out after 15 seconds"; no run with it failed. Its gain outside was about 1.3 s. The reason
  is written beside the exclusive override.
