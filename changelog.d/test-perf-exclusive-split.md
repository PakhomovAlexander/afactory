- The self-optimizer light-strategy test no longer holds every test thread for its whole run
  (#203, [ADR-0124](docs/adr/0124-run-tests-in-parallel-processes-and-link-them-once.md)). It
  is split into nine tests in `crates/af/tests/it/self_optimizer.rs` that each build their own
  repository and Store with the same `af` commands through shared helpers. Only the one that
  measures real latency stays in the exclusive override of `.config/nextest.toml`. Every
  assertion of the original test is in exactly one new test, unchanged. The `git` exit-status
  checks the original repeated inline now live in one `commit` helper that every test that
  commits calls. Phase by phase (line ranges of the original test):
  `light_strategy_generates_one_candidate_without_exposing_source_to_author_workers` keeps
  the run to plan review and the check that no author Worker receives source (186-259);
  `light_strategy_runs_the_approved_candidate_once_and_replays_without_new_attempts` the
  signed approval, the five-Attempt run, the configuration, candidate package, proposal and
  replay (260-376); `light_strategy_delivers_the_measured_candidate_and_observes_its_adoption`
  the delivery, adoption receipt and equivalent, replayed and edited observations (378-530);
  `light_strategy_changed_source_invalidates_the_installed_recipe_identity` the source
  invalidation (532-571);
  `light_strategy_without_an_objective_exception_withholds_uneconomic_adoption` the uneconomic
  recommendation (573-669); `light_strategy_refuses_a_candidate_binding_it_cannot_install` the
  unexecuted binding (671-717);
  `light_cache_candidate_with_an_unknown_toolchain_is_inconclusive_and_undeliverable` the
  unknown cache toolchain and refused delivery (1092-1228), whose verdict is decided by the
  missing measurement before any timing; and
  `light_strategy_refuses_an_unsupported_recipe_before_any_protected_child` the unsupported
  recipe (1279-1322). The exclusive
  `light_cache_candidate_measures_real_latency_and_grounds_later_adoption_evidence` keeps the
  3 s baseline and the 8-trial latency comparison with the phases that read its Task: replay,
  cache delivery and adoption, the ordinary cache-consuming Task (719-1090), and the light
  adoption observed with the cache Task as evidence (1230-1277). It first delivers the light
  candidate without asserting on it, because that last observation needs an adopted Task.
- `setup_repairs_auth_directory_and_lock_modes_under_a_restrictive_umask` is no longer
  exclusive. Its only deadline is the 15 s Codex probe that its ordinary sibling runs under
  too, and its work takes 0.37 s locally and 0.07 s on CI. Its ~14.6 s was the first launch
  of a freshly linked `af`, which macOS assesses before it runs. The measurement is written
  beside the exclusive override.
