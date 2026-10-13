# Changelog

Every release has a section here before it is tagged: `scripts/release.sh X.Y.Z --compat "…"`
writes it from the pull requests merged since the previous release, and the release workflow
publishes the section as the release notes. The **Authority compatibility** line is mandatory:
it says whether committed `.af/` policy keeps working as is, needs `af onboard --refresh-lock`,
or needs a documented hand edit.

This file includes the public alpha and recent release candidates. Earlier releases are described on their
[GitHub release pages](https://github.com/PakhomovAlexander/afactory/releases), and git history
keeps their sections. In short: `0.7.0` and `0.7.1` reviewed from a `.review/` directory;
`0.8.0` moved project authority to `.af/` and made a project pin the `af` release it runs; the
`0.9.0` release candidates brought the common Task runtime, Task files and the Worker warm
layers. `0.9.0-rc.5` was tagged but never published — its macOS check leg failed before the
publish step — so everything it carried shipped in `0.9.0-rc.6`.

## [Unreleased]

Changes since the last release are notes under [`changelog.d/`](changelog.d/), one file per
pull request; the release pull request collects them here.

## [0.12.0-rc.2] - 2026-10-09

### Authority compatibility

Committed .af/ policy keeps working as is relative to 0.12.0-rc.1; no authority migration is required.

### Changes

- chore: pin af 0.12.0-rc.1 in .af/af.lock (#227)
- Keep a long path's reason on the TUI status line (fixes a test that failed every Task gate) (#233)
- Report slow tests and suite totals from every nextest run (#218)
- Run only the self-optimizer's timed phase alone (#222)
- Wait once, not per case, in looping lease and deadline tests (#204) (#234)
- Judge the same Task writer at its later recorded time everywhere (#231) (#235)
- Fix main: check the real nextest config against its own text, not a pinned exclusive list (#236)
- Give non-timeout tests a load-safe wall so a loaded machine cannot fail them (#206 part 1) (#237)

- `make test` now prints where test time goes after every nextest run, passing or failing:
  nextest's wall time and test count, summed test-seconds, achieved parallelism, the time spent
  in the exclusive block that `.config/nextest.toml` runs alone
  ([ADR-0124](docs/adr/0124-run-tests-in-parallel-processes-and-link-them-once.md)), failures,
  a duration histogram and the slowest tests. CI also writes it to the step summary. A Task
  gate (`scripts/verify.sh`) records every `make check` step's time beside its nextest reports
  and ends its output with the same summary. The JUnit an earlier run left is removed before
  nextest starts, and a JUnit whose testcases do not add up to its declared counts gets a
  warning instead of totals; the report never changes the test step's exit status. It runs
  under Python 3.9 and reads `.config/nextest.toml` without `tomllib`. `scripts/test-time-report.py compare BASE_JUNIT... -- HEAD_JUNIT...` compares runs
  by their per-side medians and lists per-test changes of at least 1 s and added or removed
  tests, as a Markdown table for a pull request description. `--base-nextest-config PATH` and
  `--head-nextest-config PATH` give each side its own nextest config (each defaults to
  `--nextest-config`, which still sets both), so each side's exclusive block counts exactly the
  tests its own config runs alone, and the output names both configs when they differ.

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
  | Chosen pair | 45.5 | 65.3 | 50.5 | 48.3 |

  The pair beats the single test by 9.85 s at T = 4, clearing the 5 s bar, so the split is kept
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

- Tests that loop over cases bound by a real lease or deadline now prepare every case, wait once
  on the real clock until every case's lease or deadline has passed, and then run each case,
  which first asserts that its own lease or deadline has passed (issue #204). No production
  timing constant, real clock or assertion changed; tests whose subject is the wait are untouched.
  `task_runtime::usage_recovery::worker_and_provider_cas_failure_recover_full_reported_usage_without_another_call`
  and `incomplete_native_observation_survives_cas_outage_and_both_admission_and_worker_recovery`
  went from two real lease waits each to one (about 30.5 s to 15.5 s), and each case now also
  asserts that the Task's absolute deadline has not passed when it recovers.
  `task_campaign_review::host::an_expired_review_records_incomplete_without_inventing_unstarted_gate_facts`
  went from three real deadline waits to one (5.2 s), and each mode now asserts the exact
  dispatch refusal `Task plan deadline expired` instead of any error. In review-store,
  `recording_resume_rejects_revoked_decision_or_replaced_latest_report` first gained exact refusal
  causes for every case: the expired decision and the revoked decision each refuse with the
  approval conflict, pinned to expiry or revocation alone by the decision's state; the replaced
  report's resume refuses for carrying no publication failure; and its stale-report append refuses
  at the expired-pause gate, whose other conditions the preceding refusal proves. A temporary
  variant without the wait then failed: the expired-decision and replaced-report resumes refused
  with `Recording recovery requires the exact expired admitted publication pause` instead. The
  revoked case refuses the same way with or without the wait, because revocation is checked
  before the deadline, so its own past-deadline assertion is what catches a missing wait. Only
  then were its three deadline waits coalesced into one (9.2 s to 3.2 s). No target was left
  unchanged.

- `af` browser: a message on the status line that names a long path, such as the `:cd` refusal
  `<path>: not a directory`, keeps its reason visible. The path is shortened from the left with
  `...`, whole leading components first and then, when its final component alone is too wide,
  inside that component, instead of the line being cut before the reason. A quoted path with
  spaces is shortened as one path. A message that fits is unchanged (#229).

- A live Task writer is no longer fenced when its own heartbeat records a renewal between one of
  its operations reading the clock and that operation observing its lease or writing (#231). Each
  place that compares the current writer's clock with the last recorded time now judges the same
  writer and epoch at the later of the two: the heartbeat's lease observation
  (`EventStore::task_lease_state`), the projection's clock check when it applies a transition
  (`TaskProjection::apply`, which also records that time and ends a released lease there), and the
  review integration append (`append_integration_atomic`), which now stamps its transition before
  it is persisted, as every other Task append already did. The lease check of each Task write
  (`TaskProjection::check_lease`) and the append stamp already used this time. Another writer or
  epoch, a premature takeover, Task collection's own clock check, an expired lease and a finished
  Task are refused as before
  ([ADR-0128](docs/adr/0128-renew-a-live-task-writer-lease-through-its-own-connection.md)).

- Tests: a test whose subject is not a deadline now gives its fixture processes a named,
  documented two-minute wall, `LOAD_SAFE_WALL`, instead of a short fixed one, so a loaded gate
  (seven nextest threads, load 7-28) no longer fails it on a timeout its subject never asked
  about. Only a hung fixture waits for the larger wall; a passing test takes as long as before.
  Tests whose subject is a timeout keep their exact walls, every elapsed-time assertion keeps its
  bound, and no production constant or nextest setting changes. A debug-only, raise-only test
  setting lifts the provider probes' timeouts, and one fixture grows its reserve with its walls;
  no reserve shrinks (#206,
  [ADR-0114](docs/adr/0114-budget-cli-task-fixtures-for-loaded-machines.md), amended).
- Changed, `review-runner-codex` (5 s provider wall): `task_auth_failure::` all seven tests
  (`authentication_failure_survives_missing_usage_without_retaining_challenges`,
  `non_auth_native_failures_keep_their_evidence_and_classification`,
  `plaintext_auth_failure_is_private_even_without_a_json_event`,
  `network_failure_does_not_publish_device_challenges_from_either_stream`,
  `successful_output_about_device_authentication_is_unchanged`,
  `model_text_about_revoked_credentials_cannot_replace_a_network_failure`,
  `a_failure_event_without_usage_names_its_classified_cause`);
  `task_capture::held_output_retains_reported_overrun_without_admitting_the_message`;
  `task_final_message::native_final_message_objects_are_bounded_and_keep_exact_usage` (also its
  15 s child-harness wall); `task_worker::review_role_keeps_the_legacy_workspace_write_sandbox`,
  `execute_checks_runs_workspace_write_rooted_at_the_sandbox`,
  `typed_document_and_malformed_or_failed_results_retain_the_same_provider_usage`,
  `multiple_native_turns_retain_exact_components_and_uncached_charge`,
  `malformed_native_usage_refuses_message_and_survives_raw_capture_outage`;
  `task_cancellation::native_cancellation_retains_full_turn_usage_and_reaps_owned_process`
  (shared fixture: 10 s wall, 5 s readiness window).
- Changed, `review-runner-claude` (5 s provider wall): `task_auth_failure::`
  `refresh_contention_survives_missing_usage_without_retaining_challenges`,
  `non_auth_native_failures_keep_their_evidence_and_classification`,
  `successful_output_about_authentication_is_not_classified_as_failure`,
  `network_failure_does_not_publish_device_challenges_from_either_stream`,
  `model_text_about_revoked_credentials_cannot_replace_a_network_failure`;
  `task_capture::held_output_retains_reported_overrun_without_admitting_the_message`;
  `task_worker::review_role_keeps_the_legacy_read_only_tool_grant`,
  `execute_checks_grants_bash_and_ends_shell_children_with_the_attempt`,
  `typed_document_and_malformed_or_failed_results_retain_the_same_provider_usage`,
  `malformed_native_usage_refuses_message_and_survives_raw_capture_outage`;
  `task_model_usage::title_suppression_reaches_the_child_without_replacing_personal_auth_grants`,
  `top_level_and_model_usage_charges_persist_and_reopen_exact`,
  `a_breakdown_above_the_top_level_summary_keeps_the_reply_and_is_charged_in_full`;
  `task_structured::` every test through its `invoke` helper
  (`typed_native_command_preserves_input_schema_and_role_permissions`,
  `execute_checks_command_derives_bash_from_the_access_alone`,
  `typed_reply_never_falls_back_and_failed_outputs_keep_accounting`,
  `malformed_or_oversized_typed_requests_do_not_spawn_and_legacy_stays_textual`,
  `nested_payload_references_keep_their_own_roots_and_literal_values`);
  `task_cancellation::native_cancellation_retains_full_usage_and_reaps_owned_process`.
- Changed, `review-runner` `model_supervision::`
  `controlled_capture_preserves_redacted_evidence_after_cancellation` (10 s wall, 5 s readiness),
  `a_model_descendant_holding_output_is_charged_not_empty_evidence`,
  `a_model_descendant_holding_only_stderr_preserves_the_complete_answer`,
  `settled_held_output_retains_redacted_bytes_without_admitting_a_message` (1 s),
  `a_worker_may_ignore_its_stdin`, `large_stdin_and_stderr_are_drained_concurrently`,
  `a_briefly_lingering_descendant_cannot_truncate_a_large_answer` (5 s),
  `a_granted_secret_is_redacted_from_everything_kept`,
  `an_ungranted_variable_never_reaches_the_child` (10 s).
- Changed, `review-process`: unit tests
  `a_descendant_holding_only_stderr_does_not_destroy_complete_stdout` (1 s),
  `capture::tests::stream_input_failure_keeps_both_outputs_and_its_error_type` (5 s),
  `capture::tests::held_stdout_capture_and_compatibility_wrapper_keep_the_same_failure` (1 s),
  `drain::tests::a_read_failure_retains_the_prefix` (5 s grace) and
  `drain::tests::cancelled_drain_waits_for_buffered_prefix_and_keeps_cancellation_primary`
  (1 s readiness and grace), which assert a read failure or a cancellation, never a timeout;
  `cancellation::cancellation_retains_prefixes_and_stops_running_stdin_and_each_held_drain`
  (5 s readiness, 10 s wall).
- Changed, `review-sandbox` `container::tests`: the runtime-fixture writer takes its wall, so
  `an_installed_but_broken_runtime_is_unusable_not_usable` and
  `cancellation_still_confirms_container_removal_without_cancelling_cleanup`, which assert no
  timeout, write their fake runtime under the load-safe wall instead of 5 s; the deadline tests
  keep the writer's 5 s.
- Changed, `review-source-task` (5 s deadline): `transport::`
  `native_transport_owns_protocol_flags_and_keeps_credentials_off_argv`; `sources::`
  `jira_refuses_incomplete_changed_or_unsupported_sources_without_leaking_response_text`,
  `adf_list_continuations_preserve_nesting_and_ordered_marker_width`.
- Changed, `af` unit tests: `providers::task::currentness_tests::`
  `sandbox_environment_passes_the_identity_recheck_before_it_can_reach_the_native_client`
  (5 s Attempt wall); `providers::auth_handoff::adapter::tests::` the 5 s synthetic-login waits of
  `synthetic_claude_process_accepts_exactly_one_code_and_reaps`,
  `synthetic_claude_callback_completes_without_returned_code`,
  `native_nonzero_exit_never_presents_a_stale_challenge`,
  `synthetic_codex_device_cli_is_reaped_on_successful_exit`,
  `dropping_device_login_reaps_the_owned_native_cli_without_json_requests`,
  `codex_native_failure_cannot_complete_or_publish_a_stale_challenge`,
  `private_guard_codes_are_decoded_only_for_owned_guard_children`,
  `native_stderr_never_enters_the_private_challenge_buffer`,
  `native_output_limit_is_cumulative_and_never_a_diagnostic`.
- Changed, `review-pipeline`:
  `review_domain::integration::tests::legacy_check_order_and_one_writable_clone_survive_the_shared_sequence`
  (10 s Check wall and 10 s deadline option), which runs the same two successful Checks three
  times and asserts only their order and equal receipts, never a timeout or a deadline refusal;
  `task_runtime::control::captured_command_cancellation_retains_both_streams_and_never_retries`
  (3 s readiness),
  `task_runtime::domain_observes_started_attempt_and_persists_through_the_runtime_store` (2 s
  Store wait), `review_domain::integration::tests::`
  `controlled_check_sequence_retains_interrupted_raw_result_and_stops_before_the_next_check`
  (20 s Check wall, 3 s readiness).
- Unchanged, subject is a timeout or deadline: each test here asserts a timeout, a deadline
  refusal or an elapsed-time bound, and keeps every wall it sets, including the
  fixture-preparation walls inside it. Codex and Claude `task_worker::`
  `timeout_and_cas_failure_preserve_reported_overrun_without_admitting_the_message`, which now
  also asserts that its message error names the 500 ms wall's `TimedOut`; Claude
  `task_model_usage::synthetic_native_multi_model_usage_survives_refusal_timeout_and_cas_outage`,
  whose message error is the model-identity refusal, so its timeout scenario now asserts it
  returns before the fixture's 10 s sleep could end it; `model_supervision::`
  `a_hung_reviewer_is_killed_at_the_deadline`, `a_killed_reviewer_keeps_what_it_wrote_so_far` and
  `a_model_parent_exit_cannot_leave_the_stdin_writer_unbounded` (`TimedOut` and elapsed);
  `review-source-task` `native_source_cancels_inflight_process_and_bounds_output` and
  `replacement_sources_have_equivalent_requirements_and_exact_field_provenance` (`TimedOut`);
  `review-check` `deadline::` `a_hung_check_is_killed_and_recorded_not_run`,
  `a_backgrounded_grandchild_does_not_hang_the_deadline` and
  `a_passing_check_with_a_backgrounded_child_returns` (elapsed); `review-source-git`
  `git_deadline` (deadline error and elapsed); `review-process` `concurrent_pipes` (`TimedOut`
  and elapsed) and `drain::tests::held_pipe_cleanup_keeps_a_chunk_delivered_after_group_termination`
  (its 1 s grace ends in a held stdout, and the shared cleanup deadline); `review-sandbox`
  `container::tests::` `runtime_detection_stops_at_the_callers_deadline`,
  `a_wedged_runtime_is_bounded_and_unusable` and `a_wedged_container_execution_is_bounded`
  (deadline error and elapsed), and `a_failed_reap_is_distinct_from_a_safely_stopped_timeout`
  and the live-runtime `a_timed_out_container_is_removed_before_execution_returns`, which now
  also assert that their container command did not finish; `af`
  `providers::installation::a_working_cli_and_a_slow_or_cancelled_check_are_not_installation_failures`
  (elapsed) and `identity_rechecks_use_remaining_attempt_deadline_and_prespawn_cancellation`
  (timed out and elapsed); `review-pipeline`
  `an_absolute_attempt_deadline_refuses_setup_and_bounds_the_running_check` (deadline refusal
  and elapsed) and `unconfirmed_container_cleanup_preserves_the_writable_sandbox_and_stops_checks`,
  whose container is ended by its 100 ms Check wall and which now also asserts that its Check's
  reason names that timeout.
- Unchanged, no process starts under the wall: `model_supervision::`
  `a_missing_provider_is_unavailable_not_silent`,
  `an_untrusted_option_is_refused_before_the_model_starts`;
  `cancellation_before_spawn_cannot_execute_the_command`;
  `capture::tests::spawn_failure_does_not_invent_output`;
  `a_removed_executable_with_no_installed_client_refuses_by_name_before_any_probe`;
  the 1 s execution wall of `an_installed_but_broken_runtime_is_unusable_not_usable`, refused
  before launch; `drain::tests::cleanup_retains_a_prefix_even_when_its_reader_failed`, whose
  in-process reader fails at once under the production 5 s grace of `collect_after_kill`, the
  function it tests, so no test parameter sets that bound; and `task_model_transport`, whose
  1234 ms is a sentinel forwarded to a fake adapter.
- Unchanged, the bound is the asserted promptness: the elapsed-time assertions of the changed
  capture, final-message, held-pipe and cancellation tests; the waits in which a killed process
  must disappear (`native_cancellation`, claude
  `execute_checks_grants_bash_and_ends_shell_children_with_the_attempt`, `review-process` and
  `review-source-task` cancellation); `review-sandbox`
  `cancellation_still_confirms_container_removal_without_cancelling_cleanup`, whose readiness
  window is its 3 s elapsed assertion; `review-graph` `independent_reviewers_run_concurrently`;
  the lease and heartbeat tests of `task_runtime::control`, `review-store` and `af`
  `campaign_loop::heartbeat`, `common_review_summaries::recovery` and `task_interrupt`.
- Unchanged, the wall is already long: `af` `tui` (30-300 s), `provider_auth_handoff` (30 s),
  `task_report_command` (60 s), `task_command_process_group` (30 s) and the container probes'
  60 s execution wall.
- Changed, `af` integration tests (15 s Codex, 10 s Claude usage and 30 s Claude status
  provider probes): a debug build's provider probes now honor `AF_TEST_PROVIDER_PROBE_TIMEOUT_MS`, which can only raise
  their timeouts (a value at or below production, outside 1 ms to ten minutes, or not a number
  changes nothing), never reaches a Worker or provider CLI, and is never read by a release
  binary. The shared `crate::common::af()` helper, and the PTY spawns of `provider_registry` and
  `tui` that clear the environment, set it to the two-minute load-safe value, so
  `provider_registry::status_keeps_a_default_context_whose_status_probe_failed` and every other
  `af` integration test that meets a probe without being about its timeout no longer race it.
  The probe-deadline unit tests (`providers::installation`) keep the production value.
- Changed, `af` `provider_auth_handoff`: `Fixture::command_kind`, `Fixture::begin_kind` (through
  its Python host) and the inline `af` spawn of
  `duplicate_stdout_and_named_fifos_are_not_private_capabilities` clear the environment, so their
  `af` never got the setting and a completed handoff's fake Codex or Claude status probe ran
  under the production 15 s or 30 s. Each now sets it again after `env_clear`; no test there is
  about a probe's timeout. The fake CLIs record the setting if it reaches them, and
  `Fixture::no_secrets` and `claude_private_code_and_callback_paths_complete_without_task_dispatch`
  assert it reached neither a status probe nor a login.
- Checked, no other change needed. Every other `env_clear` or `env_remove` before `af` or a
  provider probe in a test: `tui`'s `af()` helper and `Browser` PTY spawn, and
  `provider_registry`'s `in_terminal` (through `setup_in_terminal` and the umask setup test),
  already set it after clearing; `common::af()` callers that only remove other names
  (`task_warm_checks`, `task_experiment`, `task_remote_checks`, `campaign_loop`, `cli_surface`,
  `storage_budget`, and `provider_registry`'s status runs) keep it; the umask `provider add`
  runs of `provider_registry`, `onboarding_quickstart` (`af onboard`, `af review plan`) and
  `common::Layout::command` (self-management) reach no provider probe and clear nothing; the
  `af` unit-test `env_clear`s (`providers::auth_handoff::adapter` and `guard` synthetic logins,
  `tui::panes::pipelines` git) start neither `af` nor a probe; and no other crate's tests start
  `af` or a provider probe.
- Changed, `af` `task_file` native-model tests (the four `native_client_*`,
  `native_model_cli_*`, `native_codex_multiturn_*` and `native_task_account_change_*`): the
  reviewers' 5 s Attempt walls are now the two-minute load-safe wall, the verification reserve
  grows from 60 s to the 290 s it now encodes (45 s Provider admission, both reviewers and the
  5 s check), and the Task wall keeps ten minutes above that reserve; token and Attempt limits
  are unchanged. Their admission and rechecks run under the raised probe timeout above, and
  they assert the setting reached neither a probe nor a model call. The pinning test, renamed
  `native_model_fixture_widens_only_its_walls_and_the_reserve_that_covers_them`, asserts the
  reserve equals that sum instead of the old literal.
- Not a wall: `optimization_configuration`'s failures are #149 (reflink cache materialization);
  its tests set no wall, and nothing here changes them.

- `scripts/test-test-time-report.py` no longer pins the live exclusive list of
  `.config/nextest.toml`: it reads the expected filter and its `test(/.../)` clauses from that
  file's raw text, independently of the TOML subset parser it checks, and keeps the exact-list
  assertions on the committed fixture `fixtures/test-time-report/nextest.toml`. Changing which
  tests run alone no longer breaks `make preflight-check`. It still requires every live
  exclusive pattern to be a plain test-name pattern anchored with a trailing `$`, and fails
  naming the offending pattern otherwise, since an unanchored one would silently run more tests
  alone: a temporary local edit removing the `$` from one live clause made the script fail,
  while dropping or renaming anchored clauses left it passing.

## [0.12.0-rc.1] - 2026-10-08

### Authority compatibility

Committed .af/ policy keeps working as is. Machine files: the remote-checks mapping no longer takes `checks` (a pipeline's `remote_checks` chooses), and `[storage]` is new machine-only configuration (budget-only by default).

### Changes

- release: v0.11.0 (#183)
- Remote Checks: the pipeline chooses where a check runs (#186)
- Leave out a logged-out CLI default context once its kind is registered (#182)
- Bump jsonschema, num-bigint, minisign-verify and minisign; test damaged signatures (#188)
- build(deps): bump minisign from 0.9.1 to 0.10.0 (#177)
- build(deps): bump minisign-verify from 0.2.5 to 0.3.0 (#176)
- build(deps): bump num-bigint from 0.4.8 to 0.5.1 (#175)
- Bump jsonschema from 0.57.0 to 0.58.4 (#174)
- Add af task report, and require its block in every pull request (#192)
- Pin af 0.11.0 in .af/af.lock (#184)
- Charge zero and record unknown usage when a Provider reports none (#165) (#195)
- Hold af's disk use to a machine budget (#213)

- Remote Checks are chosen by the pipeline, not by the machine. A Task pipeline's check node lists
  `checks` (run on this machine) and `remote_checks` (run through the check's declared remote
  executor); a project that wants both gates keeps two pipeline variants, and there is no
  per-machine switch. The machine-local mapping (`$XDG_CONFIG_HOME/af/remote-checks.toml` or
  `AF_TASK_REMOTE_CHECK_POLICY_FILE`) now names only where this machine may push gate branches
  for a repository: **its `checks` key is gone, and a file that still carries it is refused**. A
  pipeline with remote checks cannot be planned without a target, and its plan carries the effect
  `publish-gate` and the destination `github:<owner/name>`, which `af task plan` prints on
  `EFFECTS` and `SEND`, so confirming the plan is the consent; the target is read again before
  the check Attempt pushes. A pipeline without `remote_checks` plans and runs exactly as before
  ([ADR-0140](docs/adr/0140-run-a-declared-check-through-a-gate-pull-request.md), amended).

- `af provider status` and the browser's Providers pane no longer list the Claude or Codex CLI's
  default context (`claude-ambient`, `codex-ambient`) when it has no login and a Provider of its
  kind is registered; it still shows on a machine with no Provider of that kind yet, and when it
  holds a login ([ADR-0141](docs/adr/0141-list-a-logged-out-default-context-only-until-its-kind-is-registered.md)).

- `af task report TASK_ID...` summarizes how recorded Tasks ran and what they cost, as one
  Markdown block between `<!-- af-task-report:v1 -->` and `<!-- /af-task-report -->`, ready for
  a pull request description, or with `--json` one `af/task-report@1` document
  (`schemas/task-report-v1.json`). The block leads with one line per pipeline the Tasks ran, its
  steps in dependency order with each step's Worker by Provider kind and model and the gate's
  check names, parallel steps of one role grouped
  (`review (bugs, correctness: codex gpt-6-sol/high)`), and `**unknown pipeline**: not
  retained` once for Tasks whose plan was collected or never made; then one row per Task, each
  a round, with its outcome, its review findings by severity (`6 major, 1 minor`, `none`,
  `unknown` when a round has no complete finding set, `gate failed`, or `—` without a review,
  and how many reviewers failed), tokens with thousands separators
  (`205,295`) and active time, and a `Total:` row of Attempts (failed), tokens and active time;
  then per round its runs, wall time, failed Attempts by reason class and charged tokens, and per
  node the Worker, Attempts, tokens, elapsed time and check results. Findings are counted once
  each as the round's reduce step recorded them, and review rounds come from the Task's log, so
  a round recorded before `af task refresh` still counts. Active time sums the Task's runs, the
  leases in which an Attempt started, so waiting between `af task run`s, a refresh, the recovery
  of a pending Attempt and a resume that only publishes a settled result are not counted; a
  figure the Store does not record is shown as unknown. Each Attempt is charged to the Worker of
  the plan it ran under, so a node that `af task refresh` moved onto another Worker has one row
  per Worker. It only reads the Store and never prints a Provider label, path, credential,
  prompt or Worker output: a Worker's model is copied only when it is a model identity (at most
  96 characters of letters, digits and `._:+-` with at most one `/`, and no `@`, URL, drive
  letter or `..`) and is `unknown` otherwise, and every cell encodes `\` and `|` so a value stays
  in its cell. This repository now requires the block in every pull request description: changes
  are made through af Tasks, and the `PR report` workflow, which runs on `pull_request_target`
  from the base branch's checker so a pull request can change neither the check nor its
  workflow, refuses a description without exactly one well-formed block — a pipeline line, the
  six round-table columns Round, Task, Outcome, Findings, Tokens and Active in order, at least
  one round row and a last `Total:` row; placeholder, short and long rows (with or without their
  outer `|`), a Round, Task or Outcome cell that is empty or holds only an HTML comment, a
  separator row of the wrong width, and a block inside a code fence or indented as code, even
  right after a heading, a thematic break, a fence, an HTML block or a list item, fail —
  apart from Dependabot and `release/` pull requests
  ([ADR-0142](docs/adr/0142-carry-the-af-task-report-in-every-pull-request.md)).

- An Attempt whose Provider reported no usage is charged 0 tokens and its usage is recorded as
  unknown, with its cause, instead of being charged its whole reservation: a Codex Attempt that
  failed with `Selected model is at capacity` no longer costs 400,000 tokens, nor does one
  recovered after its writer's lease expired while the machine slept. The settlement's
  `af/TaskExecutionRecord@5` carries `unknown_usage: { cause }` (`capacity`, `rate_limit`,
  `authentication`, `model_unavailable`, `network`, `lease_expired`, `interrupted` or
  `unreported`); such an Attempt adds nothing to the Task's, node's or verification reserve's
  charged tokens and still counts against the Attempt limits, and Attempts with reported usage
  are charged exactly as before. A Codex Attempt that ends with an `error` or `turn.failed`
  event names its classified cause, such as `Provider model at capacity (capacity)`, instead of
  the bare exit status. `af task show`, `af task list`, the browser's Tasks and Workers panes and
  `af task report` show such usage as unknown, never as 0 spend: a Tokens cell reads
  `1,200 (+1 unknown)`, the round's details name the cause, and the JSON documents carry the
  count. A usage observation, even one that reports 0 tokens, makes an Attempt's usage known,
  and `af task gc --apply` keeps the count in the collected Task's `af/TaskCollected@1`
  tombstone, so a collected Task still lists and reports `(+N unknown)`. Stores written by earlier releases read as before, with their recorded charges
  ([ADR-0143](docs/adr/0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md)).

- af now keeps a bounded, visible amount of disk
  ([ADR-0144](docs/adr/0144-hold-afs-disk-use-to-a-machine-budget.md)). A machine-only `[storage]`
  table (20 GiB `max_bytes` and a 10 GiB `min_free_bytes` floor by default; `AF_STORAGE__<KEY>`
  overrides it, and a repository's `.af/af.toml` cannot) bounds warm build keys, warm Workspaces,
  review campaigns, Task Stores and installed versions, evicting the least recently used entry first
  and never one in use or used within the hour, after every `af task run` and `af review run`.
  Age-based collection (`keep_days`, `keep_tasks`, `keep_campaigns`) runs on `af storage prune
  --apply`, or after every run with `auto_gc = true` (off by default), and reaches Stores this
  release cannot read and Stores made with `--state`, which `$XDG_STATE_HOME/af/stores.toml` now
  records; every removal opens its target from its configured root through descriptors, never
  following a link and never removing a directory that changed since it was measured, and an
  installed version is checked again under the lock `af self` takes to change the default or a pin.
  The sweep that ends `af task run` is recorded on that Task as a `storage_sweep` observation, shown
  by `af task show`. Below the floor on any volume af works on, or when one cannot be measured, a
  check reports `insufficient_disk`, a measurement fails with `insufficient_disk` without running
  its command, and a Worker Attempt is refused before any token is spent. `af storage` shows what af
  holds and `af storage prune [--apply]` reclaims it. A warm check with a native toolchain mapping
  now reuses one key instead of making a new one every Attempt (the key domain moves to
  `af.task-build-cache.toolchain/2`, so old keys are evicted once). Every check, review gate checks
  included, gets its own empty `HOME`, `TMPDIR`, `AF_CHECK_SCRATCH` and `XDG_CACHE_HOME`, removed
  after it: write to `$AF_CHECK_SCRATCH` or `$TMPDIR`, never `/tmp`. Provider probes run in a
  directory af makes for them, and af removes each Claude Worker Attempt's and probe's history from
  the Claude config directory (`keep_worker_transcripts` keeps it). It closes a finished Task's gate
  pull request and deletes its `af-gate/` branches (`keep_gate_pull_requests` keeps them) only while
  they still equal the Task's recorded repository, refs and commits, recording the result as
  `gate_cleanup` without changing the Task's result, and collects such a Task only after a cleanup
  is done.

## [0.11.0] - 2026-10-05

### Authority compatibility

Committed `.af/` policy from v0.10.0 keeps working as is. Remote Checks are opt-in twice: a check's `[checks.<name>.remote]` table is optional, and without an operator's machine-local mapping every check still runs locally. To move a project pin to v0.11.0, run `af onboard --refresh-lock --af 0.11.0`. This is a public alpha: compatibility guarantees begin at 1.0; persisted pre-GA Task and Campaign state is unsupported across upgrades, so finish or inspect a Task with the release that started it.

### Changes

- release: v0.10.0 (#154)
- Add af provider remove, and a d key for it in the Providers pane (#168)
- Sweep the sandbox directories a killed af process leaves in $TMPDIR (#169)
- Show TUI state in the brand's colours, as chips (#172)
- Pin af 0.10.0, and pin every release the workflow publishes (#173)
- Pin af 0.10.0 in .af/af.lock (#164)
- Fix executable onboarding and first-run guidance (#156)
- Open the TUI on a splash of three workers while it loads (#181)
- Add private-host Provider authentication (stage 1 of #122) (#171)
- Remote Checks: run a declared Task check in CI through a draft gate pull request (#178)

- Make the first review walkthrough executable with reviewed, locally committed authority and
  explicit policy, Base and candidate selectors; no remote push or Provider login is needed to
  inspect a plan. Clarify empty Change Sets and source-build versus signed-release pins.
- Select the software catalog profile explicitly in implementation entrypoints and keep tutorial
  Task requests outside committed source Snapshots. Exercise the documented Quickstart and
  generated tutorial through token-free planning in regression tests.

- Sandbox directories are named `af-sandbox-<pid>-…` under `$TMPDIR`, and every command that
  runs review or Task work (`af review run`, `af task start --execute`, `af task run`,
  `af provider doctor`) begins by removing the ones whose process no longer exists, finishing the
  removal before it exits. A sandbox is removed by the handle that owns it, so only a killed or
  aborted process leaves one behind, and a leftover tree that a Gate built into carries a whole
  `target/`: one development machine held 15 GB of them. The sweep keeps every directory whose
  process is still running and every directory the kernel preserved on purpose after an
  unconfirmed container cleanup (now marked `preserved` beside the tree), removes a tree only
  through directory descriptors opened without following links, touches nothing else under
  `$TMPDIR`, and reports what it removed in one stderr line.

- `af provider remove ID...` drops named Providers from the machine-local registry through the
  same locked, atomic publication `add` uses. Auth directories and their logins are left
  untouched, and the previous registry is preserved. It also repairs a registry made invalid by a
  deleted auth directory, runs in the invoking release from any repository, and refuses ambient
  IDs by name. In the browser, `d` on a registered Provider fills the `:` line with the command
  ([ADR-0136](docs/adr/0136-remove-a-registered-provider-by-id.md)).

- Add explicitly permissioned, private-host Provider authentication for browser-only setup and
  reauthentication, with Codex device approval, Claude code/callback support, bounded session
  state, recipient fencing and secret-free status. Generic setup login remains terminal-only;
  login never implicitly spends a model budget or resumes a Task
  ([ADR-0137](docs/adr/0137-permit-provider-logins-through-private-host-capabilities.md)).
- Include a runnable, consent-bound personal-chat host for validated one-time browser challenges,
  native Codex headless device login and Claude 2.1.289's exact unterminated prompt; reusable
  credentials remain native and ordinary af status stays challenge-free
  ([ADR-0139](docs/adr/0139-deliver-native-login-challenges-to-a-verified-private-requester.md)).
- Preserve native authentication failure categories alongside usage diagnostics and suppress
  credential-bearing failed output before ordinary capture, retaining exact parsed usage.
- Classify native login failures into closed response/proxy/TLS/rejection/transport states while
  keeping stderr private and bounded; retain the native lifetime guard and credential boundary.
  An absent or unstartable official CLI reports `provider_cli_missing` (exit 4).

- Remote Checks: a declared code check may add a `[checks.<name>.remote]` table (`executor =
  "github-pr"`, `workflow`, `required` job names), and an operator's machine-local mapping
  (`$XDG_CONFIG_HOME/af/remote-checks.toml` or `AF_TASK_REMOTE_CHECK_POLICY_FILE`) selects it per
  repository. Local checks run first; the kernel then builds two `af-gate/<task-id>/` branches
  from the Task's Snapshots in a private repository, pushes them without force, opens one draft
  pull request between them, waits on the declared workflow's `pull_request` run for that head
  commit, and accepts the result only after reading `refs/pull/<n>/merge` back as the candidate
  tree. Every remote fact is one `af/RemoteCheckEvidence@1`; a remote `CheckResult@1` names it
  instead of a command; every refusal is `not_run` with a named reason; the last 256 KiB of each
  unsuccessful job's log (1 MiB per check) is kept as the result's `stdout`; no record holds the
  push URL or the mapping's path. `af task show` prints the pull request, run, jobs, kept log
  excerpt and cleanup commands, and `--json` carries the evidence under `remote_checks`. Without
  a mapping nothing changes. This is the one operator-authorized exception to "publishing is a
  human action"; delivery still never pushes
  ([ADR-0140](docs/adr/0140-run-a-declared-check-through-a-gate-pull-request.md)).

- The TUI shows state in the brand's colours: chips of ink on blue (running), green (passed) and
  pink (failed or awaiting approval) on a Task's STATE, its stage marks, the progress in the bar,
  a Provider's STATUS and the Workers pane's Attempt counts, and an `error` chip before every
  error row. The status line turns pink while it carries an error, and the help header's worker
  is drawn in solid pink. Text on the terminal's own ground is never coloured, so every colour
  reads on a light or a dark terminal; `NO_COLOR` still leaves attributes only.

- This repository's own `.af/af.lock` pins `0.10.0` (it still pinned `0.9.0-rc.6`). From now on
  the release workflow pins each release it publishes, and `make check` fails when the pin falls
  more than one release behind `CHANGELOG.md`
  ([ADR-0138](docs/adr/0138-the-repository-pins-its-newest-release.md)).

- Bare `af` paints a splash a few milliseconds after it starts: the three workers on a conveyor
  belt, the one a Task block reaches lit in its colour, while the scope and its panes load behind
  it. The browser replaces it as soon as it has loaded, so `af` is never slower; outside a
  repository, where reading every Task Store takes seconds, the terminal no longer sits blank.
  `q` or `<C-c>` quits from the splash, and other keys typed meanwhile reach the browser.

## [0.10.0] - 2026-10-02

### Authority compatibility

Committed `.af/` policy keeps working as is.

### Changes

- release: v0.9.1 (#152)
- Fix Task writer lease under load, interrupted Workers, and unstartable Provider CLIs (#134, #135, #136) (#153)
- Research pipelines: warm checks, measure and compare, report Tasks, port bindings, store hygiene, and two verified research Tasks (#132)

- Ctrl-C (SIGINT) or SIGTERM during `af task run`, `af task start --execute`, `af review run`
  or `af provider doctor` no longer leaves Worker processes running after af exits. af stops
  and reaps every Worker process group, then ends by the same signal, so the shell reports 130
  (SIGINT) or 143 (SIGTERM). The interrupted Attempt is recorded as a failed, cancelled Attempt
  and keeps its charge. The Task is not finished; stderr says to resume it with
  `af task run TASK_ID`. A second Ctrl-C while stopping kills the remaining Worker groups and
  exits at once ([ADR-0129](docs/adr/0129-stop-task-workers-when-af-is-interrupted.md)).

- A live Task writer no longer fences itself with `Task writer lease is expired or fenced` on a
  loaded machine. The lease heartbeat now observes the lease through its own Store connection.
  It renews through that connection when the work still holds the shared one with 4 s of lease
  left. That connection waits at most 1 s for SQLite's write lock and retries with backoff, so
  neither a slow Store operation nor its write lock can outlast the lease. A renewal that
  commits after its lease expired is now refused inside the write transaction. A successor or an
  expired lease still fences and cancels the old writer, and is now seen without waiting for the
  held Store
  ([ADR-0128](docs/adr/0128-renew-a-live-task-writer-lease-through-its-own-connection.md)).

- A Provider CLI that cannot start is reported as a Provider installation failure instead of
  surfacing later as an unrelated Worker or credential error. A CLI cannot start when it is
  missing, not executable, or exits non-zero on its own `--version` check. `af provider status`
  marks the context `installation_failed` and exits 4, and `af provider setup` returns
  `provider_cli_missing`. Task Provider admission refuses the binding before any Worker is
  dispatched or any Attempt is charged. Each report names the Provider ID, the program path, the
  CLI's own first error line and the fix it suggests. A CLI that breaks after admission fails its
  Attempt as a Provider environment failure, not a model or credential failure
  ([ADR-0130](docs/adr/0130-report-a-provider-cli-that-cannot-start.md)).

- A Worker's draft reply may spell its `citations` and `repository_citations` in any order: the
  runner admits both sets in canonical order (sorted, unique) before validating the reply, so an
  author is judged on what it cited rather than on the order it listed it. This repository's
  `kernel/report` gives its author two Attempts.
- Store hygiene and the warm cache's two bounds (ADR-0135, package R5 of
  `docs/design/research-pipelines.md`; amends ADR-0131). `af task list --sizes` prints each Task's
  CAS bytes — those only it reaches and those it shares with another Task or Campaign record —
  and the Store's total; `--json` adds `sizes` to each entry and `store` to the document. `af
  task gc --older-than DAYS --keep N` previews, writing nothing, which finished Tasks beyond the
  newest `N` it would collect and why every other Task stays (`running`, `unfinished`,
  `writer_lease`, `bound_by`, `kept_newest`, `kept_recent`); `--apply` takes the Store's writer
  lock, is refused while any writer lease is live, appends one `task_collected` transition
  carrying `af/TaskCollected@1` per Task — referencing no artifact — and then removes every CAS
  object no uncollected record reaches, by a conservative walk through every digest a record
  spells. A stopped sweep is finished by the next run. A collected Task's projection stops at
  its tombstone: `af task list` and `af task show` print `collected <time>` with the retained
  summary (`af/task-collected-inspection@1` for `show --json`, a `collected` member on
  `af/task-list-entry@2`), `af task output`, `deliver`, `run` and `explain` refuse it, and replay
  never calls its removed objects corrupt; the browser lists the Tasks it can open. An append now
  rechecks, under the writer lock, that every object it references is still filed. `[warm]
  max_bytes` is now the eviction bound, applied before a check and after one whose result
  stands, and a new `[warm] hard_max_bytes` (twice `max_bytes` by default, at most 32 GiB) is
  the only bound that ends a running check with `warm_cache_bound_exceeded`;
  `TaskCacheObservationV1` records the acting `bound` and `af task show` prints it. New schemas
  `task-collected-v1.json`, `task-collected-inspection-v1.json` and `task-gc-v1.json`;
  `task-transition-v5.json`, `task-list-entry-v2.json`, `task-runtime-evidence-v1.json` and
  `code-task-policy-v1.json` gain the optional members. A Store without a tombstone and a policy
  without `hard_max_bytes` behave as before, except that such a policy's check now fails only
  above twice its `max_bytes`. A reader that loses an artifact to a concurrent sweep reports the
  Task as collected, a listing projects the uncollected Tasks before reading the tombstones, an
  opening Task re-checks every Task its bindings name under the writer lock, and a collected
  Task's row under `--sizes` carries a zero footprint.
- Bind any declared root port (ADR-0134, package R4 of `docs/design/research-pipelines.md`;
  amends ADR-0117). A Task file's `inputs` table may bind any root input the selected Pipeline
  declares — the one the Task file names, or else every captured Pipeline accepting its kind,
  alike — to a recorded, finished Task's result output whose artifacts verify in the CAS and
  whose type and cardinality equal the port's exactly; `requirements`, `base` and `continuation`
  stay refused by name, and `source`, `history` and `sources` keep ADR-0117's rules. Only result
  outputs bind: naming an Attempt's raw artifacts, runtime evidence or any other record is
  refused with a message saying so, and an exact `{ "artifact" }` reference binds only
  ADR-0117's three ports. A list of one to sixteen `{ "task", "port" }` references binds a `many`
  port in order, so `measurements` can take an experiment's `baseline` and `candidate`; a `one`
  port keeps its output's Snapshot ID, a `many` port bound from several outputs names none while
  each artifact keeps its own, and a list into a `one` port is refused. Every refusal names the
  port and both types before any Worker or Provider admission. The Store, the executor (for a
  `many` port the consuming contract declares `unbound`) and the Worker renderer accept such a
  Snapshot-less `many` input port; output ports keep the one-Snapshot rule. A port bound without
  naming the Pipeline must be declared alike by every Pipeline accepting the kind, else the Task
  file is asked to name one; a list bound to a `one` port has each reference judged before its
  shape; a bound `many` output records every artifact it holds; and `source`, `history` and
  `sources` are checked against the selected Pipeline's declaration at resolution. This repository's `kernel/analyst` and `kernel/report-verifier` schemas take the
  `snapshot_id` a bound Measurement or comparison carries.
  `af/TaskInputBindings@1` records a port's further outputs in an optional `also` list, absent
  for every single-reference binding, and `af task explain` and `af task show` print their
  existing binding rows once per output. `task-file-v1.json` gains the list form and
  `task-input-bindings-v1.json` the `also` list. The `builtin/report` starter's Worker schemas
  admit a `snapshot_id` on `comparison` and `measurements` values. A Task file without `inputs`
  is unchanged.
- Report Tasks (ADR-0133, package R3 of `docs/design/research-pipelines.md`). The built-in kind
  `report` selects a new installed profile: an author reads the source Snapshot, the kernel
  renders its draft and resolves its repository citations against that exact Manifest, and an
  independent verifier on the same Snapshot accepts the report. Its captured policy is
  `af.report-task-policy/1`, named by `report_policy` in `.af/task-catalog.toml` (a newly
  declared `.af/report-policy.toml`). A Task file's `report_sources` captures
  `af/ReportSources@1` — the `af.document-sources/1` shape with zero to 256 entries, 256 KiB each
  and 512 KiB in total in a file of at most 640 KiB, the empty set when absent; the `sources`
  port is optional, and a Pipeline that binds nothing there seals against the empty set. An
  execute-checks Worker may add beside the source, never under a name the source holds. `af/DocumentDraft@2` adds
  `repository_citations` of `{ path, line? }`, spelled exactly as the Manifest spells them and
  rendered as `path` or `path:line`. The installed `report_seal`, `report_check` and
  `report_accept` operators record `af/ReportCheckReceipt@1` (with each failed citation's
  reason: `absent`, `directory`, `symlink`, `binary`, `line_out_of_range`),
  `af/ReportEvaluation@1` and `af/ReportVerification@1`, every one naming the source Snapshot;
  a verifier whose checks judged another Snapshot is refused at admission. An author whose
  effects are `read-source` and `execute-checks` gets ADR-0118's shell in a clone that seals
  nothing back. A report Task allows no `write-source`, has no `snapshot` output and is refused
  by `af task deliver` with a message naming `af task output --port report`; `af task show`
  prints the report's title, the verifier's outcome and the cited Snapshot. `af catalog init
  --profile report` emits the credential-free `builtin/report` starter, and this repository's
  `kernel/report` Pipeline with `kernel/analyst` and `kernel/report-verifier` is staged in
  `fixtures/kernel-report/` for installation into `.af/`. New schemas: `report-sources-v1.json`,
  `document-draft-v2.json`, `report-check-receipt-v1.json`, `report-evaluation-v1.json`,
  `report-verification-v1.json` and `report-task-policy-v1.json`; the catalog, Task-file,
  Task-kind and operator schemas gain the new fields, profile and operators. Document and
  implement Tasks are unchanged.
- Measure and compare (ADR-0132, package R2 of `docs/design/research-pipelines.md`). A code
  policy may declare `[measures.<name>]` — a command, 1 to 16 `repetitions`, `warm`, `wall_ms`
  per repetition and `metrics` of `{ key, unit }` in `ms`, `bytes`, `count` or `ratio` — and
  `[objectives.<name>]` — a measure, a metric, `lower` or `higher`, `min_improvement_ratio`
  (decimal text such as `"0.1"` or the integer 0 or 1; a float is refused because the parser has
  rounded it) and `min_repetitions`. The installed `measure` operator runs a measure against a
  fresh read-only Snapshot per repetition with a private `HOME`, `TMPDIR`, `XDG_CACHE_HOME` and,
  unless `warm = true` binds the Warm Check Cache, `CARGO_TARGET_DIR`. It re-verifies the source
  after every repetition and records `af/Measurement@1`: every run's elapsed time, exit status,
  output digests, the cache condition it actually had (warm or cold, bytes, and why when cold)
  and the metrics the command reported on an `af.measure-report/1` last line with the declared
  keys and units. A repetition the kernel ends at a time bound is recorded from the supervisor's
  typed ending as `timeout` or `deadline`, with what it printed kept, and a command a signal
  ended records no exit code. A failure (`exit`, `timeout`, `deadline`,
  `malformed_report`, `unit_mismatch`, `source_mutated`) stops the measurement and leaves no
  summary. The installed `compare` operator folds two Measurements into
  `af/MeasurementComparison@1` in exact decimal arithmetic: medians with an exact even-sample
  mean, signed improvements, ratios in lowest terms, and `improved`, `below_threshold`,
  `unchanged`, `regressed` or `inconclusive` per metric. It is `passed` only for `improved` on
  the objective's metric. The plan compiler refuses a measure node whose repetitions exceed
  `check_wall_ms`, undeclared measures and objectives, and mixed comparisons. `af task output
  --port comparison --format markdown` renders one table, `af task show` prints medians and
  conclusions, and `af catalog init --profile experiment` emits the `builtin/experiment`
  starter, whose evaluator runs only after passed checks and a passed comparison.
  `scripts/measure-release.sh` and this repository's `release_build` measure,
  `release_build_time` objective and `kernel/experiment` packages are staged in
  `fixtures/kernel-experiment/` for installation into `.af/`. New schemas:
  `measurement-v1.json` and `measurement-comparison-v1.json`; `code-task-policy-v1.json` and the
  operator schema gain the new tables and operators. A policy without them is captured, planned
  and shown exactly as before.
- Warm Task checks bind the kernel's rustup home and keep Cargo's home warm (ADR-0131 amended,
  package R1 of `docs/design/research-pipelines.md`). Under `[warm]` a check and its toolchain
  probe receive `RUSTUP_HOME`, from the kernel's own, else its `HOME`'s `.rustup`, and
  `RUSTUP_AUTO_INSTALL=0`. A rustup proxy therefore answers from the installed toolchain
  instead of downloading one into the check's fresh `HOME`. That download is what made the
  probe exceed its 30 s bound. `RUSTUP_HOME` joins the toolchain key, and where it came from,
  or why it is unset, is recorded. `build_cache` admits `cargo_home`, bound as `CARGO_HOME`
  beside `cargo_target` under one toolchain key and one shared `max_bytes`. A `cargo_home`
  holding `credentials.toml` is suspect. A declared `caches = ["cargo"]` supersedes it
  (`cargo_home:superseded`). Four verification findings are closed. First, the directories are
  measured again after every check, so a fast check that wrote past the bound fails with
  `warm_cache_bound_exceeded` too. Second, traversal is descriptor-relative
  (`openat`/`fstatat`, `O_NOFOLLOW`) and fails closed: an unreadable subtree makes a
  directory suspect before reuse and fails a check that left it. Third, `ensure()` reports a
  discard, and bytes are measured only after it, so a recreated directory is cold, never its
  old size. Fourth, every warm check, started or not, keeps one evidence group naming it
  (`TaskRuntimeEvidence@1` gains an optional `check` binding) with one observation per
  declared kind, such as `deadline_exhausted` or `cache_refused`. `af task show` never prints
  an unnamed line. A checked-in golden recorded by a kernel without this package pins every
  document of a Task without `[warm]`, and a fixture proves from the implementer's sandbox
  manifest, the sealed candidate, the derived Snapshot and the delivered tree that no cache
  byte reaches them.
- Warm Task checks (ADR-0131, `docs/design/research-pipelines.md` package R1). A code policy
  may declare `[warm] build_cache = ["cargo_target"]`, `caches = ["cargo"]` and `max_bytes`
  (default 8 GiB, at most 32 GiB). A `trusted_local` Task check then builds into
  `$XDG_CACHE_HOME/af/task-build-cache/<project>/<toolchain>/cargo_target`. The toolchain key
  digests the Snapshot's `rust-toolchain.toml`, `rustc -vV`, `cargo -vV`, the host triple and
  the check's fixed environment. The directory has one exclusive lock; a check that waits 60 s
  for it runs cold. It is bounded before, during (`warm_cache_bound_exceeded`) and after every
  check, and removed rather than repaired. Declared Cache Snapshots bind `CARGO_HOME` from the
  check's runtime directory. `[warm]` with `require_container = true` is refused at load. Each
  warm check records its own `TaskRuntimeEvidence@1` with one cache observation per kind, whose
  `kind` may carry a `:reason` suffix. `af task show` prints `check <name>: <ms> ms, cargo_target
  warm <bytes>` or `cold <reason>`. `schemas/code-task-policy-v1.json` is new. `scripts/verify.sh`
  honours a `CARGO_TARGET_DIR` that is already set. A policy without `[warm]` is captured, run
  and shown exactly as before. A check holds an exclusive lock over its whole toolchain key from preparation to the end of
  its removal step, the bound is measured over the whole key, a directory that is suspect once
  the check ended (a link, a special file, a forbidden `credentials.toml`, a root swapped for a
  link) fails the check with `warm_cache_suspect` and is removed under the lock, and a root that
  is no longer a real directory counts as above every bound. Every compile-time
  `CARGO_MANIFEST_DIR` in the workspace is now a fallback behind the run-time `AF_WORKSPACE_ROOT`,
  because a warm gate reuses test binaries compiled in the previous gate's sandbox, and a test
  refuses a new one; this repository bounds its warm cache at 16 GiB. Every operation below a
  toolchain key goes through the key directory's descriptor, never a path, so a check that swaps
  the key's parent for a link cannot redirect cleanup; the shared bound is measured over the whole
  key before and after every check and an eviction removes every kind below it; and an
  observation's `evicted_bytes` carries its `evicted_reason`, which `af task show` prints. Only the
  two warm kinds are ever locked and only the kernel's own lock files are exempt from the bound
  (they are truncated on acquisition), a policy whose kinds a Cache Snapshot supersedes still holds
  and bounds the key, and a check whose warm directory is suspect once it ended fails even when
  nothing was left to evict. Only the lock inodes the holder opened are exempt from the key's
  bound, the key directory itself must stay private and its held locks in place for a check to be
  accepted (a widened key is emptied before reuse, never repaired), and a check that holds the
  key is monitored and judged even when every declared kind is superseded. A held lock is exempt
  only at its own name with its own inode and must stay a single empty name; the key is made
  writable through its held descriptor before an eviction, entries are removed by their exact
  bytes, and an eviction that leaves anything behind is an error; every declared observation of a
  failed check carries the eviction and its cause. A lock's inode is judged a plain, singly linked
  file of this user at its name before acquisition writes through it, a key that a check renamed
  and recreated is displaced and suspect, eviction empties or drops the held locks and reports
  success only when nothing but sound empty locks remains, and suspicion outranks the byte count
  in the recorded cause. ADR-0131 states where these rules stop: `trusted_local` is not
  isolation, and the cache is honest as evidence, not a defence against a check acting on the host. A link or a
  directory at a lock's name is removed before the name is opened, and a waiter judges the inode
  again after acquiring the lock, before truncating it. A held lock a check grows counts toward
  the running bound, removal addresses every entry by its exact bytes, and a directory's
  `source_digest` no longer varies with the lookup's outcome.

## [0.9.1] - 2026-10-01

### Authority compatibility

Current-shape committed .af/ authority needs no edit for these fixes; updating the installed default does not change project pins. To adopt 0.9.1 deliberately, run af onboard --refresh-lock --af 0.9.1 online and review/commit the authority. Native Rust snapshots require an explicit captured request and operator mapping; absent mappings preserve cold setup. Pre-GA (0.x) Task/Campaign state has no cross-release compatibility guarantee and is not migrated; retain its original release to finish or inspect it.

### Changes

- Explain Task verification budget rejections (#145)
- fix: clarify review timeout scope and externalize read-only gate reports (#146)
- Keep Claude Task Workers running on Claude Code 2.1.285 and across its updates (#147)
- feat(tasks): pinned private Rust snapshots for native checks (#151)

- Infeasible Task verification reserves now report required and available tokens, Attempts and
  wall milliseconds, identify exceeded resources, and list each protected node's contribution,
  including Provider admission. These are declared allowances, not estimated runtime; admission
  rules and configured budgets are unchanged.

- Keep nextest CI JUnit reports outside read-only source trees during `scripts/verify.sh`,
  alongside the existing external Cargo target. Unique per-run report directories preserve
  failed-run evidence; temporary store-only tool configuration is removed afterward. Direct
  `make test` retains its existing report location and all test/profile settings are unchanged.
- Run `make check` and `scripts/verify.sh` as an unprivileged user, including in containers.
  The new sealed-source regression requires mode bits to deny writes; root or
  `CAP_DAC_OVERRIDE` bypasses that seal and fails the explicit precondition rather than
  silently skipping the regression.

- Correct the `af review run --timeout-secs` help: without `--file` (including the default
  routed run and explicit `--campaign`/`--pipeline`) it bounds each
  reviewer Attempt (default 1800 seconds, pinned in the Campaign manifest as
  `reviewer_timeout_seconds`), not the whole run; with `--file` it caps the whole Task at the
  lower of the flag and the file's `limits.wall_ms`. Timeout behaviour is unchanged.

- Claude Task Workers no longer fail with `Native billing usage is incomplete` on Claude Code
  2.1.285, whose final result counts requests in `modelUsage` that its top-level `usage` leaves
  out. A breakdown that no top-level counter exceeds is charged as the complete bill; a top-level
  counter above the breakdown is still refused
  ([ADR-0125](docs/adr/0125-charge-a-claude-model-breakdown-that-covers-the-top-level-summary.md)).
- An Attempt refused for incomplete billing now names the cause in its diagnostic, for example
  `Native billing usage is incomplete: Claude top-level and per-model usage cannot be reconciled`.
- A Claude Code or Codex update in the middle of a Task no longer refuses the Task's next model
  Worker with `Captured Task Provider identity is no longer current`. The Task keeps running the
  client executable it captured when it started, and moves to the installed client if the update
  removed that file; the account is still proven before every invocation
  ([ADR-0126](docs/adr/0126-keep-the-captured-native-executable-when-its-launcher-moves.md)).

- Native code checks can opt into pinned Rust from an explicit operator mapping; AF verifies and privately copies only that toolchain before dispatch, avoiding repeated rustup downloads in fresh check homes. Host Cargo/Rustup homes are not passed through, inherited Cargo subcommands remain available, and check writes are never promoted. Missing mappings preserve cold setup; invalid mappings fail closed. Trusted-local isolation is unchanged ([ADR-0127](docs/adr/0127-snapshot-pinned-rust-before-native-task-checks.md)).

## [0.9.0] - 2026-09-30

### Authority compatibility

Committed `.af/` policy from v0.9.0-rc.9 keeps working as is. To move a project pin to v0.9.0, run `af onboard --refresh-lock --af 0.9.0`. This is a public alpha: compatibility guarantees begin at 1.0; persisted pre-GA state is unsupported across upgrades.

### Changes

- Publish the tested v0.9.0-rc.9 CLI as the public alpha, available through the standard installer
  and `af self update`. Includes Task execution, sandboxed Review and the terminal browser.
- Mark the README as public alpha and document security support for the latest alpha.
- Add advisory Task preflight for lineage, budgets, and evidence (#142)

- Add an offline advisory orchestration helper, `scripts/task-preflight.py`, for original-source
  identity, declared admission/Worker allowances and explicit acceptance-evidence mappings.
  It reads native inspection exports without changing Task authority, budgets or acceptance;
  ambiguous replay, live reservations and semantic completeness remain explicitly unverified.

## [0.9.0-rc.9] - 2026-09-29

### Authority compatibility

Committed `.af/` policy keeps working as is.

### Changes

- release: v0.9.0-rc.8 (#126)
- af self update: keep unverified --version out of the latest cache; make a failed update unmistakable (#128)
- brand: the afactory design system for the README, the website and the TUI (#127)
- Bump jsonschema from 0.56.0 to 0.57.0 (#125)
- TUI: fast Workers open; list old-release Stores once; uix measures latency (#130)
- tui: the bare ? help opens with the pixel worker; CHANGELOG entry for the brand (#129)
- Halve the gate: parallel test processes, linked-once test binaries, a compile cache that hits (#131)
- Change notes per pull request under changelog.d/; CHANGELOG.md never conflicts (#140)
- providers: a 15-second Codex probe budget (#133)
- TUI: Pipelines read in batches; spawn-count guard; index flags do not hide drift (#138)

- The design system: `brand/` keeps the designer's wordmark (AFACTORY with three workers) and
  derives every asset from it with `brand/gen.py`; `brand/tokens.css` and `tokens.json` carry
  the palette, type, spacing and radii; `brand/README.md` is the brand book. The README opens
  with the banner; the tagline is "agent pipelines made fast". The browser paints its status
  line ink on the brand's blue and its errors pink where `COLORTERM` says the terminal takes
  truecolor, and as before otherwise (#127).
- The browser's bare `?` help opens with the pixel worker, the version and the tagline above
  the key reference.

- `make check` runs its tests one process per test across every test binary at once
  (`cargo nextest run --profile ci`), with `TEST_THREADS` set from the machine's cores; the
  five tests that assert against a fixed real-time budget run alone and first, listed in
  `.config/nextest.toml`. The longest tests are split one case per test, the eight largest
  crates link their integration tests as one binary under `tests/it/`, doctests are off (there
  are none), and debug information is line tables only. CI compiles through `sccache` keyed
  by rustc inputs, caches only the crate registry, links with `lld` on Linux, and lints in a
  job beside the tests
  ([ADR-0124](docs/adr/0124-run-tests-in-parallel-processes-and-link-them-once.md)). Measured
  on a 14-core developer machine: the test step 661 s to 289 s, a cold test build 58 s to
  46 s with 122 to 48 executables and 5.7 GB to 3.5 GB, the doctest step 22 s to none.

- The browser's Workers pane opens in a tenth of a second instead of most of one: it reads every
  committed declaration, prompt and lock with one `git cat-file --batch` and checks their drift
  with one `git diff --literal-pathspecs --name-only`, side by side, where it spawned about four
  git processes per Worker on every open (0.79s to 0.06-0.13s for 14 Workers on a loaded
  machine). A file name with `*` or `[` is now compared literally.
- The Tasks pane lists the Task Stores an earlier (pre-GA) af release wrote as one bar entry,
  `! N old Stores`, and one note saying where they are and that af does not read pre-GA state
  (ADR-0113), instead of an error row each. A Store refused for any other reason still shows its
  own error. `review_core::event::ANOTHER_RELEASE` is the phrase both sides share.

- The Codex status and subscription probes allow 15 seconds instead of 5. On a loaded machine
  (a full build, an IDE indexing) starting `codex` alone took long enough that `af task run`
  refused a healthy provider with `Codex subscription probe timed out after 5 seconds`, and
  provider tests failed in Task gates. Claude's structural probe already allows 30 seconds.

- The browser's Pipelines pane reads every committed pipeline, the catalog and their drift
  with the same two batched git calls as the Workers pane, instead of two git processes per
  pipeline. Tests assert both panes spawn as many git processes for 30 entries as for 2, so
  a per-item read cannot come back unnoticed.
- A declaration marked `skip-worktree` or `assume-unchanged` in the index is still marked `*`
  when its working-tree copy differs from `HEAD`: `git diff` does not compare such files, so the
  panes hash those, and only those, against the committed blob.

## [0.9.0-rc.8] - 2026-09-28

### Authority compatibility

Committed `.af/` policy keeps working as is; a source-writing Worker that declares `execute-checks` gets a shell, and only then (ADR-0120).

### Changes

- release: v0.9.0-rc.7 (#118)
- Give a source-writing Worker that declares execute-checks a shell (ADR-0120) (#119)
- The af browser's Tasks pane (docs/design/tui.md §5.5) (#120)
- TUI: the Workers pane (M4) (#123)
- TUI: hand af commands off from the : line (M5) (#124)
- Give a source-writing Worker that declares `execute-checks` a shell (ADR-0120): `worker_access`
  maps `write-source` plus `execute-checks` to `WorkerAccess::WriteSourceWithShell`, the Claude
  adapter grants `Read,Glob,Grep,Edit,Write,Bash` and Codex runs `workspace-write`, and the shell's
  process group dies with the Attempt. Candidate capture skips the scratch a shell leaves: paths
  under a top-level name the source did not hold, and new top-level dotfiles
  (`SealedSandbox::capture_snapshot_where`). A writer without `execute-checks` is unchanged. The
  kernel's `kernel/implementer` declares it and formats, lints and tests before replying.
- The browser's Tasks pane (ADR-0121, `docs/design/tui.md` package M3) lists the scope's Tasks
  under `running/`, `awaiting approval/`, `done/` and `failed/`, newest first, as
  `task-id  outcome  progress%`. The user scope adds one level per repository's Task state. A
  Store the binary cannot read is an error row naming its directory. One Task's pane shows TASK,
  PLAN and SNAP lines, one PROGRESS row per stage of the plan's graph order (`[ok]`, `[..]` with
  its attempt, `[!!]`, `[  ]`, with recorded wall and charged tokens), the TOKENS and TIME totals,
  and one HISTORY row per event. Every value comes from `af task list --json` and the
  `af task explain --json` document (`af task show --json` with the plan and its graph), or from an
  artifact they name. A running opened Task is read again about once a second off the key loop.
  `Enter` on a HISTORY row shows its artifact, `p` opens the Task's pipeline, `y` yanks the Task
  id and `R` reads again. The `af task show` builder is split from its printer, and
  `default_task_state` is the one spelling of the default `--state`. No `--json` document
  changes.
- The browser's Workers pane (ADR-0122, `docs/design/tui.md` package M4) lists the reviewer
  Workers and Task Worker packages `HEAD` commits under `.af/workers/`, `.af/task-packages/`,
  `.af/packages/` and `.af/vendor/`, grouped by source when more than one has Workers. The
  folder is read when it is first opened. One Worker's pane shows IDENTITY (its declaration and
  its `af.lock` or catalog pin, `unpinned` or `pinned elsewhere`), then STATE, then PROMPT
  (`reviewer.md` or `instructions.md` as committed, with `gf` opening the working-tree file).
  STATE counts the Attempts in this scope's Task Stores whose invocation ran a slot that the
  recorded plan binds to the Worker at the digest the committed pin records (other digests of
  the name on one `other` line), a Review shard's through the node that owns it, and a
  reviewer and a Task package of one name apart: reserved, settled ok, settled failed and
  released, plus the tokens charged and the wall time. Every open reads again. A drifted file is marked `*`, and a failed read of
  `HEAD` is shown above the last good entries. No `--json` document changes.
- The browser's `:` line runs `af` commands (ADR-0123, `docs/design/tui.md` package M5). A line
  that clap parses and the browser does not own (`:q`, `:cd`, `:scope`, `:e` and `:help` keep
  their behaviour) runs as a child of the running `af` executable, with exactly the parsed words
  as its arguments. It runs in the scope's root, inherits the environment, and carries
  `AF_DISPATCHED_FROM`, so it is never dispatched to another release. The browser releases the
  terminal, and the child's process group owns the foreground, so `<C-c>` stops the child, not
  the browser. The released screen then shows `af LINE: exit N` (or the signal) and waits for
  Enter. After it, the browser reads again the scope, the settings and the Tasks, Workers and
  Providers panes (without the charged probe), keeping what is opened. A line without a
  subcommand, which would open a second browser, is refused. `<Tab>` completes subcommands at
  every level from the clap definition, and a Task ID argument from the Tasks pane. In the Tasks
  pane, `r` prefills `task run ID --confirm-plan PLAN`, and `D` on a verified Task prefills
  `task deliver ID --branch af/ID --worktree ../ID --confirm` and a space, with the
  confirmation left to type. `D` on an unverified Task says why. No `--json` document changes.

## [0.9.0-rc.7] - 2026-09-24

### Authority compatibility

Committed `.af/` policy keeps working as is; `execute-checks` on a review Worker is opt-in.

### Changes
 Stage progress counts only records of the Task's current plan, so a refresh or a Review continuation that reuses node names starts from nothing; a dynamic Scatter's completion settles its parent; an invocation without a reserved Attempt is not shown running; a finished Task's elapsed and wall time end at its `finished` transition; a Task the Store lists but the pane cannot inspect refuses the Store with the cause instead of being grouped by a partial summary. Opening a Task that can no longer be inspected refuses its Store the same way; a HISTORY row opens the artifact its change is about by change kind (a revocation, not the decision; a report, not an event id); an existing state directory that is not a Task Store, or one the process may not read, is refused, never listed as empty (`task_execution::store_present`), a dangling `events.sqlite` link included. A load or `R` inspects every listed Task again rather than reusing a finished Task's cached summary. A state directory that exists but cannot be listed is refused; an opened Task awaiting approval is read live too, since the CLI may start it; `p` opens the package the committed catalog pins under the Task's Pipeline name at a fresh read of `HEAD`, never another file declaring that name; a Store refused on a live read closes its opened Task. A load or `R` re-resolves every recorded artifact as well; PROGRESS counts a failed stage only once no Attempt is left for it; `p` is refused when `HEAD` cannot be read; a refreshed Task's time ends at its last finish; the bar groups a Task by the phase its inspection read; opening another Task drops the previous one's live read; the user scope reads a symlinked Task state directory and refuses a link to nothing. A bar row's outcome and charge come from the same inspection read as its group, and a Store refused on a live read also closes an artifact opened from the Task's HISTORY. A reservation released before dispatch is not counted as an Attempt; a stage's charge takes an Attempt's highest recorded charge, a usage observation included even when it arrives after the settlement; a failed report of an unfinished Task leaves a stage with Attempts left open; a record newer than the last run report decides its stage, so a retry after a failed report shows; opening a Task rebuilds its bar row from the same read as its detail; cached summaries and artifact lookups are kept per Store, so one Store's reads never stand in for another's; a run report not superseded by newer records decides its stage over an older success (a later suppression shows skipped); `crates/af/tests/tui.rs` also drives the Tasks pane at 80x24 on a pseudo-terminal; `p` works again once `HEAD` reads again after a failure; the TASK line cuts a long id before it hides the goal; a stage's latest settlement decides its mark (a newer failure after a success is failed); a node suppressed for a missing upstream stays open while the Task can still recover; a HISTORY artifact the Store cannot give back refuses the Store.
- release: v0.9.0-rc.6 (#107)
- Name the CLI crate after its binary: crates/af (#108)
- Bump base64 from 0.22.1 to 0.23.1 (#103)
- Bump the cargo-minor-and-patch group across 1 directory with 3 updates (#101)
- test: stop reporting a reaped descendant as live after cancellation (#109)
- Take the toml, toml_edit and jsonschema updates, parsing TOML documents as tables (#111)
- Bump toml from 0.8.23 to 1.1.6+spec-1.1.0 (#102)
- Bump toml_edit from 0.22.27 to 0.25.15+spec-1.1.0 (#105)
- Bump jsonschema from 0.26.2 to 0.56.0 (#104)
- Make Provider onboarding safe for agents (#112)
- Remove pre-GA legacy, migration and compatibility code (#114)
- Keep .af/ to declarations: layout table, undeclared-path report, Task input bindings (ADR-0112..0114) (#113)
- Pin Opus 5.5 and GPT-6 Sol defaults (#116)
- Fix source install adoption by af self (#115)
- Bare af opens a vim-native TUI; review Workers may run a shell (execute-checks) (#117)
- Bare `af` at a terminal opens a read-first, vim-shaped browser (ADR-0119,
  `docs/design/tui.md` package M1). On a pipe, bare `af` still prints help to stderr and exits
  2. The scope comes from `config::load`: the user scope outside a repository, the project scope
  inside one, or `af --repo DIR`. A 28-column folding bar holds `providers/`, `workers/`,
  `pipelines/` and `tasks/`. Beside it, the Settings pane shows the layer table and the effective
  values with their origins, as `af config paths` and `af config show --origin` print them, and
  `e` opens a layer with `af config edit`. The Providers pane shows the `af provider status`
  columns with a bar per quota window, and `R` runs the bounded usage probe off the key loop. The
  Pipelines pane shows, verbatim, the `af task explain --tree` text of a token-free plan compiled
  as `af task plan` compiles one, into a scratch Store; the status line shows the highlighted
  slot's Worker binding. The Workers and Tasks panes arrive in later packages. `:` lines are
  parsed by the CLI's own clap definition. The terminal runs through `rustix::termios`, an
  existing dependency, instead of the planned `crossterm`. No `--json` document, schema or
  fixture changes. Discovery under `.af/task-packages/` walks every real directory, however deep, and keeps walking below a package. Escape sequences split across terminal reads decode whole (`tui::keymap::Decoder`), and a lone `ESC` is Escape only after a read brought nothing. Outside a repository the user scope never opens a directory layer, so a broken `.af/af.toml` above a plain directory cannot block it. `af --repo DIR` with no subcommand is exempt from pin dispatch like bare `af`. The bar lists the pipelines `HEAD` commits and reads every declaration from `HEAD`, the authority a preview compiles, marking a working-tree file that differs; a package the kernel refuses to plan shows its declared contract; a review Pipeline shows its committed declaration. `af config edit user` names the user layer without reading the ladder, so a directory layer that does not parse cannot block it. `e` on a highlighted layer opens that exact file, so two present directory layers cannot swap; a settings refresh or an editor hand-off whose reload fails says the settings are stale and why, instead of calling them refreshed; the Worker-binding status follows a modified package's notice row; a malformed escape stream is bounded and dropped through its terminator. Git runs for the pipelines pane with a cleared environment and an allowlist, so an inherited `GIT_DIR` cannot redirect it; a failed read of `HEAD` is an error above the last good entries, not an empty catalog; entries are bound to one resolved commit and read again before a preview when `HEAD` moved; a package is previewed only when the committed catalog pins its name at that file; an editor hand-off refreshes the opened pane; the status line yields the breadcrumb before cutting a binding or an error at 80 columns. A preview compiles the exact commit its entries were read from (`plan_tree_preview_at`), and a result for a commit the pane left is dropped; drift is judged by `git diff` (bytes, mode, existence); a failed `HEAD` read is shown on an opened entry too and nothing is compiled from stale entries; a scope change drops the previous repository's entries; the terminal's panic hook restores the screen only from the thread that owns it. `R` acts on the bar's selected node while the bar has focus; an editor hand-off from the bar refreshes that node's pane too and the bar follows; opening an entry the new `HEAD` no longer commits falls back to its folder; a failed reload after an editor is reported as stale settings; an unborn `HEAD` is told apart from a branch naming a missing commit. The kernel's review-light reviewers get two Attempts each, so a malformed reply is retried instead of ending the round. A toplevel is an ancestor whose `.git` git would open, not any path so named; a cancelled provider probe is forgotten and the last complete inventory stays; a modified pipeline's bar label carries `*`; a reload that finds the place became another scope enters it and drops the old entries.
- Let a review Worker that declares `execute-checks` build and drive the candidate (ADR-0118). A
  model Worker whose `roles` contain `review` and whose effects are
  `["read-source", "execute-checks"]` now runs in the same `Mode::EphemeralWrite` clone that
  AF-owned preparation uses, with its readable Review inputs, instead of a read-only tree. The
  Claude adapter adds `Bash` to its adapter-owned `--tools` and `--allowedTools` and keeps
  `--safe-mode --restricted --permission-mode dontAsk --strict-mcp-config`. Codex runs
  `-s workspace-write` rooted at the sandbox. The model process's whole group is killed when it
  exits, so a shell child cannot outlive the Attempt; wall-clock and token bounds are unchanged.
  Nothing is sealed back. At `finish` every Snapshot entry must be byte-identical; anything the
  reviewer added — build output, its harness, the dotfiles tools write into `HOME`, which is the
  sandbox root — is discarded with the clone and is not a source edit.
  Any source edit fails the Attempt with a diagnostic naming the changed paths, and a read-only
  Worker's refusal names its paths the same way. The adapter's `writable: bool` became the
  kernel-derived `review_runner::task::WorkerAccess`, computed from the captured effects by
  `review_pipeline::task::source::worker_access`, so no package or `.af/` policy can name a tool.
  Existing Workers derive exactly the access they had; fixtures and `--json` documents are
  unchanged. A command Worker declaring the same effect runs under the process-group-killing
  exit policy too, so a background child it starts cannot outlive its Attempt
  (`crates/review-runner/tests/task_command_process_group.rs`).
- Kill a supervised child's process group before reaping the child, not after: a reaped pid is
  free for reuse, so the late `SIGKILL` could land on an unrelated process that had just been
  spawned into its own group under the recycled id — on a loaded machine, a fresh `git rev-parse`
  dying with an empty stderr. `review-process` now observes the exit without reaping
  (`waitid(WNOWAIT)` on Linux, kqueue `NOTE_EXIT` on macOS and the BSDs), routes every group
  kill through the unreaped leader, and reaps last on every path — deadline, cancellation and
  held-pipe cleanup included; the provider probes poll the same way, ending a probe's group
  before reaping it. A failed git command with no stderr now reports its exit status, so a
  signalled git reads as signalled. The two provider lock files are opened with a
  bounded retry on the spurious `ENOENT` APFS returns to a concurrent `openat(O_CREAT)`.
- Declare the `.af/` layout once and keep Task files out of git (ADR-0115): every canonical entry
  under `.af/` — its kind, its writer of record, whether git versions it and what it holds — is
  now one table in `review_config::layout`, and `layout::classify` answers whether any
  repository-relative path is declared or undeclared. `af help config` renders that table and one
  paragraph saying what never belongs under `.af/` and where it lives instead, and the old claim that `af init`
  gitignores `af.local.toml` is gone — there is no `af init`, and no command writes a `.gitignore`. A test walks every production source under `crates/*/src` and
  fails on any `.af/` path the table does not declare, which added `document-policy.toml`,
  `packages/`, `artifact-reuse/`, `cache/` and the in-memory-only `task-compat/` to the declared
  set. `af task plan`, `af task start --file`, `af review plan --file` and `af review run --file`
  now warn on stderr, after capture, when the Task file resolves inside the repository and `git
  check-ignore` does not ignore it, naming the file and `$XDG_STATE_HOME/af/tasks/`. The warning
  is advisory: exit codes, stdout and every `--json` document are unchanged, and a Task file that
  is gitignored or outside the repository draws no warning.
- Report undeclared `.af/` paths and let a project refuse to deliver them (ADR-0116):
  `review_config::layout::classify_manifest` groups every `.af/` path of a captured Snapshot
  manifest into declared and undeclared, with a path count and a byte total for each. It is a
  pure function of the manifest and the L1 table — it reads recorded entries, never a working
  tree or a sandbox, and judges a path by its decoded bytes while keeping the manifest spelling —
  so the same Snapshot answers the same on every machine. `af task plan` and `af task start` now
  print one advisory line naming the count, the byte total and up to ten paths, and carry the
  whole group in a typed `undeclared_af_paths` field of the `--json` document; `af task deliver`
  records it in `af/task-delivery@1` beside `ignored_paths` and prints it in the delivery
  summary. A new project policy `[delivery] undeclared_af_paths = "warn" | "refuse"` in
  `.af/af.toml` defaults to `warn`; under `refuse`, delivery fails before the prepared record and
  before any Git mutation, naming every undeclared path and leaving no branch, no worktree and no
  record. The policy is captured into the Task's project policy identity at plan time and read
  back from that record at delivery, so editing `.af/af.toml` afterwards cannot change an
  admitted plan, while the default is not written down and moves no existing policy identity.
  Nothing is removed: Snapshot identity, the delivered tree and `ignored_paths` are exactly what
  they were, receipts written before the field deserialize with an empty group, and a repository
  whose authority tree is fully declared reports nothing at all.
- Decide how a Task file binds a root input port to a recorded Task's output (ADR-0117, proposed;
  documentation only, no behaviour changes yet). A Task file gains one optional `inputs` table
  mapping `source`, `history` or `sources` to `{ "task": "<task_id>", "port": "<output port>" }`
  or to an exact `{ "artifact": "sha256:…" }`, so chaining implementation to repair to review no
  longer means exporting a candidate tree and reviewer results to files that then ride along in
  every later Snapshot. References resolve once, at plan time, from the `--state` Store into exact
  artifact IDs in the compiled plan, so resume, retry and replay never read the referencing Task
  file again; type and cardinality are checked against the port before any Worker or Provider
  admission, and the compiler's existing root-input check remains the one that cannot be
  bypassed. The referenced Task must be recorded and finished with the named port in its result,
  but need not be verified — the reference carries provenance only, and no acceptance,
  verification, plan approval, delivery or budget authority crosses. A bound `source` that is
  already a root capture is carried verbatim and delivers as usual; a derived one — what a
  `snapshot` output is — is republished as a root Snapshot over the identical Manifest with a new
  `af.task-source-origin/2` origin that names the referenced Task, result and port and carries no
  `source_revision`, so `af task deliver` refuses it for the one honest reason: a derived tree has
  no commit for the target's `HEAD` to equal. ADR-0031's exact comparison is unchanged. The Task-file shape,
  the display in `af task explain` and `af task show`, and the implementing package's crates,
  types and tests are written up in `docs/task-execution/task-inputs.md`, linked from
  `docs/README.md`.
- Bind a Task input port to a recorded Task's output (ADR-0117, accepted; the behaviour the
  entry above decided). A Task file's optional `inputs` table now resolves at plan time, from
  the `--state` Store only, into ordinary root ports: the referenced Task must be recorded and
  `Finished`, its result must read and validate, the named port must be in `result.outputs` and
  every artifact it names must verify in the CAS, and the recorded type and cardinality must
  equal the destination port's exactly — a `many` output never binds a `one` port however many
  artifacts it holds. `requirements`, `base`, `continuation` and any other name are refused by
  name. Every refusal is an ordinary Task-file input error naming the Task and the port, exit 1
  with `af/error@1` under `--json`, raised while the revision is still being built and therefore
  before any Worker dispatch or Provider admission. A bound `source` that is already a root
  capture is carried verbatim; a derived one is republished as a root Snapshot over the
  identical Manifest with a new `af.task-source-origin/2` origin and a new `af/SourceTree@1`
  envelope, and a parentless generation-2 Snapshot referenced again is carried verbatim, so
  nothing is re-rooted twice. `history` and `sources` carry the referenced artifact ID verbatim;
  a bound `history` suppresses the `empty_review_history` root default and a bound `sources`
  makes `document_sources` unnecessary. One `af/TaskInputBindings@1` artifact records the
  binding from `TaskRevisionV1.provenance.input_artifact_ids`, written only when the Task file
  carried an `inputs` table; `af/TaskRevision@1` is unchanged and a Task without bindings keeps
  its exact revision, plan and `--json` documents. `af task explain` annotates the `IN` line and
  `--tree` adds `BOUND` rows, `af task show` prints one `bound <port> <- …` line and advances
  its `--json` document gains an `input_bindings` field only when a binding exists, which the
  self-optimizer's AF history adapter admits without changing how accounting is read. `af task
  deliver` reads either origin generation and refuses a re-rooted source before the prepared
  record and any Git mutation, naming the referenced Task and port or the artifact ID: a derived
  tree has no commit for the target's `HEAD` to equal. ADR-0031's exact comparison is unchanged,
  and no acceptance, verification, plan approval, delivery or budget authority crosses a Task
  boundary. One limit is recorded rather than worked around: a bound `history` feeds a Pipeline
  that *reads* the ledger, not one whose Review would continue the predecessor's Round, because
  restoring a Round recomputes it and that recomputation requires every reviewer result to
  retain the current Task's `af/Requirements@1` — a rule this change does not relax.
  `docs/task-execution/task-inputs.md` says so and the fixture is built that way. Seven review
  findings were then repaired in place, each with the regression its reviewer asked for: a bound
  `source` is admitted only when the referenced envelope's payload, its `subject_snapshot_id` and
  the recorded port name one Snapshot whose origin belongs to the tree it describes; a refreshed
  revision keeps the `af/TaskInputBindings@1` record while replacing only its requirements
  artifact, and the Store's refresh validator derives the same expected list; selection preserves
  that record alone, so a Task with other non-port provenance and no `inputs` table keeps
  byte-identical revision, plan and inspection documents; the Claude usage probe observes its
  leader's exit without reaping and ends the process group before waiting, so a same-group
  descendant cannot outlive it; every untrusted label a refusal echoes — destination port,
  Task ID, output port, artifact spelling and the bindable-port reason — goes through the
  preview's display sanitizer; the `af/task-inspection@11` schema gains the optional `input_bindings` property. A command Worker declaring the same effect runs under the process-group-killing exit policy too, so a background child it starts cannot outlive its Attempt (`crates/review-runner/tests/task_command_process_group.rs`).

## [0.9.0-rc.6] - 2026-09-21

### Upgrading from 0.x

- GA reads only what GA writes
  ([ADR-0113](docs/adr/0113-ga-reads-only-what-ga-writes.md)). Review Campaigns and Tasks that a
  0.x release wrote are not supported (they may be refused or misread): that covers everything
  under `$XDG_STATE_HOME/af/review/` and `$XDG_STATE_HOME/af/task/`, and any directory passed with
  `--state` or `--state-root`. Before upgrading, finish or abandon in-flight Campaigns and Tasks
  with the release that started them, then delete that state. Committed `.af/` files are read only
  in the shapes this release accepts. A key or shorthand that only an earlier release wrote is
  refused: delete the refused key by hand, because `af onboard --refresh-lock` cannot repair a
  file it cannot parse.
- Removed `af review tui`. It read Worker pins only from the lock's legacy `[reviewers]` table,
  so it failed on every lock this release writes. The subcommand is now a usage error, and the
  release no longer ships its `af-review-tui.1` man page.
- Removed `af onboard --migrate` and the `.review/` to `.af/` conversion; the retired `.review/`
  layout is not read at all. The flag is now a usage error. `af onboard` on a repository that
  still carries `.review/` scaffolds `.af/` as for any other repository, a `.review/…` pipeline
  path gets the generic "must live under `.af/pipelines/`" error, and Campaigns whose manifests
  pinned `.review/` paths (af 0.7 and earlier) can no longer be resumed, reported or compiled into
  a Task.
- `.af/af.lock` no longer has a `[reviewers]` table or the 0.7.1 top-level `af_version` key, and
  `.af/af.toml` no longer has `[worker.*]` tables: this release refuses a file that still carries
  them, so delete those lines by hand; `af onboard` no longer writes `[reviewers]` or
  `[worker.*]`. Worker pins live under `[workers]` and the release pin under `[af]`, as before.
  A Worker package's `reviewer.toml` must now declare `subjects`; an omitted list no longer means
  whole-tree only.
- `af review run|plan|render`, the `af review` shorthand and `af provider doctor` no longer accept
  `--authority REV` or `--light`; both are usage errors now. Write `--policy-rev REV`, adding
  `--base REV` for a diff pipeline (a whole-tree pipeline still refuses `--base`), and drop
  `--light`, which only restated the default. The `af/review-plan@1` document no longer carries
  `selectors.compatibility_authority`, and the text plan drops its `compat` line.
- `af provider setup` no longer starts an official Provider CLI login on its own — add
  `--login`, and run it at an interactive terminal — and `af provider status` no longer probes
  subscription and quota windows unless asked with `--usage`. Both print the command or flag to
  use, so an existing habit fails loudly rather than silently. This is the machine-local
  `af provider` surface, not repository authority: the machine-local registry stays version 1,
  and its transaction, lock, publication and recovery protocol is unchanged.
- Default Campaign state resolves only the opaque `c-<id>` directory under
  `$XDG_STATE_HOME/af/review/campaigns/`; a directory there named by the label (the layout af 0.4
  and earlier wrote) is no longer a fallback. `af review campaigns|gc --state-root` still list an
  explicit `--state` directory named by its label. A Campaign you placed under that root yourself
  with `--state …/campaigns/<label>` must keep being addressed with `--state`: omitting it starts a
  new `c-<id>` Campaign, and `af review campaigns|gc` then refuse the root because one label holds
  state under both names.
- Removed `af help trust` and its `af-trust.7` man page, which described an `af trust` command that
  never shipped.
- `af self` and `install.sh` no longer install, activate or dispatch to releases older than 0.8.0,
  the first release with a signed `SHA256SUMS`: `af self install 0.7.x` (or a 0.8.0 release
  candidate) is refused, and a project whose lock pins one runs the current `af` instead, with a
  warning. A binary that embeds the release key, and `install.sh` with `minisign` on PATH, now
  refuse any release whose `SHA256SUMS` is unsigned, instead of accepting a pre-0.8.0 release on its
  checksums alone.
- The pre-rename `~/.config/afactory/` directory is no longer read, and nothing warns about it:
  move `providers.toml` and `caches.toml` from there to `~/.config/af/` (or
  `$XDG_CONFIG_HOME/af/`), or `af` finds no provider registry and no cache policy. Setting
  `AFACTORY_CACHE_POLICY_FILE` is no longer an error; it is ignored, so use
  `AF_CACHE_POLICY_FILE`.
- `af task list`, `af task show` and `af task deliver` read only the common `events.sqlite` Task
  store. Implementation Tasks that af 0.8.x and earlier kept in `tasks.sqlite` no longer appear,
  `af task show` no longer emits `af/task-inspection@1`, and delivery no longer knows the
  `refs/afactory/deliveries/<task>` ownership ref. In `af/task-inspection` and `af/task-list`
  output, every delivery preparation and receipt now carries `result_id` and every receipt carries
  `ignored_paths`, as this release always wrote them; the published schema requires both, and a
  Task whose stored receipt lacks one is refused.
- `af task start` now requires `--file`: `--kind implement --goal …` and `--pipeline` are usage
  errors. The fixed implementation v1 format they read, `.af/pipelines/implement.toml` with its
  `.af/workers/` implementer and evaluator packages, is no longer read, and `.af/af.toml` no longer
  accepts `defaults.task_pipeline`: every `af` command refuses a project file that still sets it,
  so delete that line by hand. The `implement` pipeline, its two Worker packages and their pins in
  `.af/af.lock` are then unused and can go too. Run implementation Tasks from a Task file against
  a Task catalog instead; `af catalog init --profile software --destination <new-dir>` creates a
  new starter directory with a runnable catalog and Task files. The `make pilot-check` target is
  gone; `make check` runs the same delivery and recovery tests.
- A Task catalog Worker's `worker.toml` can no longer declare
  `runner.kind = "legacy_task_command"` with its `protocol` and `legacy_budget_tokens` keys: the
  catalog refuses such a package. That runner spoke the fixed implementation v1 Markdown and
  verdict protocol. Declare a `command` or `model` runner instead, which reads
  `af.worker-request/1` and replies with `af.worker-reply/1`. Worker context is always
  `af/TaskContext@1`; `af/TaskContext@2` is neither written nor read.
- Task Review has one generation. A Task catalog whose `[review]` table omits `generation` now
  captures `af.review-task-policy/2`, the same policy as `generation = 2`, instead of generation
  one; any other value is still refused. Reviewer packages must use the generation-two ports: an
  `assignment` input of type `af/TaskReviewAssignment@1`, a `subject` of type
  `af/TaskReviewSubject@2`, and a `review.kernel/ReviewerResult@2` result that lists
  `dispositions` (one per assigned prior Finding) instead of `disputes`. The Review pipeline wires
  each reviewer's `assignment` from `review-bind`. A package with the old
  `af/TaskReviewSubject@1` or `review.kernel/ReviewerResult@1` ports, or without an assignment, is
  refused at planning, so update it together with its pin. The `af/TaskReviewSubject@1` contract
  and its `task-review-subject-v1.json` schema are gone.
- Task execution records have one encoding per record kind: the combined `prepared` record is
  gone, and an Attempt's context is bound through separate `reserved` and `context_bound`
  records. Settlements and usage observations carry decimal-string charges.
  `af/TaskTransition` drops `revision_recorded`, which no release wrote, and
  requires `revocation_id` on `approval_revoked`, which this release always writes.
- `af self optimize` history sources: the `af` adapter reads only the `af/task-inspection`
  receipts that `af task show --json` prints and refuses any other line, including the
  `af.task-event/1` event export that no af command produced. The `af`, `codex` and `claude`
  adapters no longer read normalized records (receipted as `legacy-normalized-v1`): `af` refuses
  such a line, and `codex` and `claude` take no observation from it. Label such a source
  `adapter = "external"` and give it a new `source_id`, because a retained source cannot change
  adapter. Report-only and `--experiment` requests now derive their `optimize-…` Task ID
  the same way light requests do, so re-running one whose capture an earlier release took starts a
  new Task.
- `af/TaskRuntimeEvidence@1` cache observations no longer carry `layer` and `result`, which were
  always `dependency_preparation` and `prepared`, and a runtime span's `kind` is `check` or
  `dependency_preparation` only. `task-runtime-evidence-v1.json`, and the `af/task-inspection`
  schemas that embed it, are narrowed to match, so runtime evidence an earlier release recorded no
  longer decodes.
- Self-optimizer contracts are narrowed in place. `af/OptimizationEconomics@1` drops
  `cache_results`, a per-kind `hit`/`miss`/`unknown` map that duplicated `cache_economics`: read
  `cache_economics.<kind>.hits`, `misses` and `unknown_results` instead.
  `af/OptimizationResult@1` and `af/OptimizationReport@1` drop the constant
  `live_demonstrations: "pending"` field, and optimize Task requirements no longer carry it; the
  Markdown report replaces its "Milestone gates" section with one plain sentence saying live paid
  demonstrations and adoption observations are still pending. A result `conclusion` is
  `validated`, `rejected` or `recommendation_only` (never `inconclusive` or `no_change`), and an
  `af/OptimizationVerification@1` `profile` is always `candidate`. The `af/ExperimentalSlot@1` and
  `af/ExperimentTrialResult@1` contracts and their `experimental-slot-v1.json` and
  `experiment-trial-result-v1.json` schemas are gone. An experiment arm Worker that declares an
  `execution_configuration` input is now refused like any other unavailable input instead of
  having it silently dropped. Optimizer artifacts an earlier release stored may no longer decode,
  and native observations that older captures stored under source-dependent IDs are no longer
  merged, so replaying such a history can count them twice.
- Published schemas are narrowed. `campaign-manifest-v1.json` now requires
  `check_timeout_seconds` and `git_timeout_seconds`, which every current Campaign manifest already
  carries; a manifest from a release that predates them no longer loads. The schemas also drop
  values that no release ever wrote. A dirty `SourceSnapshot@1` capture `boundary` is always
  `revalidated` (`filesystem_snapshot` is gone), and a Gate Execution Binding's
  `provided_isolation` in `run-report-v6.json` and `run-event-v1.json` is `none` or `container`
  (`process` is gone). In `task-contracts-v1.json`, and every schema that
  embeds its Task phase or result, a Task phase is never `resolving`, `planning` or `verifying`, a
  Task result's `execution` is `completed`, `incomplete` or `exhausted` (never `blocked` or
  `cancelled`), and the unreferenced `reviewConclusion` definition is gone.
- A Codex Task Worker's reply is read only from the `-o` last-message file that `codex exec`
  writes (codex-cli 0.147.0 always writes it). The Task adapter no longer falls back to the last
  `agent_message` event on stdout, so with a codex CLI that does not write that file the Attempt
  fails with "Codex Worker returned no final message"; the usage it reported is still charged.
- Source Manifests have one path spelling. The `path_encoding` field (`legacy_v1` or
  `percent_v2`) is gone: every path is spelled the way capture already spelled new trees, so a
  path that starts or ends with whitespace, or holds a space together with a `%` or non-UTF-8
  bytes, is percent-escaped (a leading space, as in `" notes.md"`, is percent-escaped to
  `%20notes.md`, and `a%b c` becomes `a%25b%20c`). That now
  includes a file a reviewer or Worker creates during a run, which a sandbox seal, warm workspace
  scan or Task delivery spelled literally when the baseline was an ordinary tree. A Snapshot's
  content digest hashes the stored spelling, so ordinary trees keep their digests, but a tree
  with such a path gets a different Snapshot digest than an earlier release gave it. ADR-0024 is
  superseded by ADR-0113 and removed.
- `af` builds and runs on Linux and macOS only. A source build for any other host, including
  Windows and the BSDs, now stops with a compile error. It no longer compiles fallbacks that
  skipped read-only sandboxes, process-group kills, symlinks or executable bits. The release
  targets and `install.sh` are unchanged.
- `af review report` no longer has a `spend` section: the JSON of `af/review-report@4` drops the
  `spend` array (the schema no longer lists it), text output drops its `Spend:` block, and
  Markdown drops its `## Spend` table
  and `### Attempts` list. They described only Attempts of the pre-Task executor, so for a Round
  a Task hosts they were empty or held a zero-token placeholder row; `task_accounting` reports
  those Rounds' Attempts, usage, wall-clock and caps. `RunReport@1` and `RunReport@2` events
  are neither written nor read, so a Campaign whose log holds one can no longer be run, reported
  or listed. The unused `run-report-v2.json` and `review-report-v2.json` schemas are gone.
- The retired shell review harness is gone from the repository: `compat/legacy-harness/`, the
  `fixtures/synthetic/` corpus generated from it, and the `fixtures/legacy/` private-corpus
  tests. `make fixtures` and `make review-kernel-test-corpus` no longer exist, and `make check`
  no longer regenerates the corpus. A Campaign event log that holds an artifact-less
  `FindingReported@1` (the `"imported": true` shape that only the unused `ledger.jsonl` importer
  wrote) no longer replays, and `af review ledger`, `af review show` and `af review report` no
  longer print an "unavailable: legacy import" placeholder.
- `af review run` and `af provider doctor` run every Campaign on the common Task runtime; the
  pre-Task executor they fell back to is gone. A Campaign whose log holds events only that
  executor wrote (`RunReport@3` to `@5`, Cold Closeout or Session Snapshot events, from Rounds run
  by af 0.9.0-rc.0 or earlier) is refused with "Campaign predates the common Task runtime
  (af < 0.9); start a new Campaign"; one that holds its reviewer Attempt, Provider Operation or
  broker events no longer replays at all (see below). A Campaign whose first Task capture failed
  now retries capture on the common runtime, including after `--restart-round`,
  `af review policy-time advance`, `af review evidence add` or `af review demand waive`, where it
  used to run on the pre-Task executor. `--resume-provider` is gone and is now a usage error
  (exit 2); it only continued that executor's fenced Provider Operations, and the common path
  already refused it. The `af/review-outcome@1` and `af/provider-doctor@1` documents, which only
  that executor printed, are no longer produced: Providers are admitted by the Review Task's own
  probe Attempts, and doctor prints `af/provider-doctor@2`. `af review run` no longer requires
  `HOME` up front. `af review report`, `ledger` and `campaigns` count only Task Attempts toward a
  Campaign's wall-clock, so a pre-Task Campaign's report no longer shows one. ADR-0016 is
  superseded by ADR-0113 and removed. A `--restart-round` before the Task exists now keeps
  Round 1's original prior Finding Set even when the candidate changed, so the Task captured on
  the new epoch resumes; each later run used to fail with "restarted Review changed its original
  prior sets or adjacent epoch".
- The pre-Task executor's Campaign event types are gone from the event vocabulary and from
  `run-event-v1.json`: `AttemptAdmitted@1`, `AttemptDispatched@1`, `AttemptFailed@1`,
  `AttemptFenced@1`, `AttemptFeedback@1`, `AttemptInput@1`, `AttemptReleased@1`,
  `ReviewerExecutionBound@1`, `BrokerOperationCompleted@1` and `ProviderOperationTransition@1`,
  with the `provider-operation-transition-v1.json` schema and the `review.kernel/RefusalHistory@1`
  artifact type. No current command wrote them. A Campaign log that holds one fails to replay with
  "unknown review-kernel event type: <type>; this log was written by another af release; start a
  new Campaign or Task", so `af review run`, `report`, `ledger` and `show` fail on it, and
  `af review campaigns` lists it as a problem. Every event log that holds an event type this
  release does not know fails with the same message. `af review report` no longer carries the
  optional `recorded_not_gathered` field, or prints its "Recorded, not gathered" section, which
  only such events filled; the field is gone from `review-report-v4.json`.
  `af review run` still lists recorded, not gathered results from the Round's Task Attempts. The
  `af review ledger` notice for an absent latest-Round Ledger drops its
  "(N admitted result(s) remain recorded, not gathered)" clause, which always counted 0.
  ADR-0022 and ADR-0023 are superseded by ADR-0113 and removed.
- `RunReport@3`, `@4` and `@5`, the run conclusions only the pre-Task executor wrote, are gone
  from the event vocabulary and `run-event-v1.json`, with the `run-report-v3.json`, `-v4.json`
  and `-v5.json` schemas. `RunReport@6` is the only run conclusion. `run-report-v6.json` now
  defines its outcome, verdict, binding and cache shapes itself, and
  `task-review-gate-facts-v1.json` takes its Cache failure shape from it. A Campaign log that
  holds a retired report, `RunReport@1` to `@5`, no longer replays. When that report is the
  first record replay cannot read, `af review run` and `af provider doctor` refuse the Campaign
  with "Campaign predates the common Task runtime (af < 0.9); start a new Campaign"; when a
  retired Attempt, Provider Operation or broker event comes first, they print the unknown event
  type message above. `af review report`, `ledger` and `show` fail on it, `af review campaigns`
  lists it as a problem, and a new event for the Round such a report concluded is refused with
  the unknown event type message. The Round rows of `af review report` drop `reported_tokens`,
  which only those reports' plain numeric spend filled. Every row now carries
  `task_chargeable_tokens_at_report` and `task_accounting`, which `review-report-v4.json`
  requires.
- The `gate_blocked` suppression reason is gone; only the pre-Task executor's scheduler wrote it.
  A Review Gate is a Task condition, so a node behind a Gate that did not pass reads
  `branch_not_selected` in `af/TaskRunReport@2` and in `af/review-outcome@3` node
  outcomes, or `upstream_missing` once its predecessors were suppressed, and `RunReport@6`
  records both as `upstream_missing`, as before. `task-run-report-v2.json`, `run-report-v6.json`
  and `review-outcome-v3.json` no longer list `gate_blocked`, and the
  review-outcome `ledger_production` no longer lists `not_produced_gate_blocked`. A stored report
  that carries `gate_blocked` no longer decodes.
- A Campaign manifest records only the canonical `report-derived@1` Finding identity policy, and
  `campaign-manifest-v1.json` no longer lists `legacy-path-title@1`. A Campaign whose manifest
  pins that path/title policy (opened before path-independent Finding identity, ADR-0006) can no
  longer be run or continued: its manifest is refused for an unknown finding identity policy, and
  its Ledger reports the manifest as unavailable authority. The Ledger reads a Report only as an
  enveloped `FindingReport@1` whose locations are canonical repository paths. An un-enveloped
  Report, the flat pre-`FindingReport@1` shape, or a Report with a noncanonical location such as
  `./src/a.rs` now projects as an unreadable-authority placeholder that blocks convergence; a
  noncanonical location used to leave the claim readable with unknown Scope. A Ledger node's
  `FindingSet@1` output must be an envelope: the untyped `{round, sources, findings}` summary is
  refused.
- `af review report --json` Finding objects no longer carry `news_round`, a Round counter no
  decision read; convergence counts news by `scoped_news_round`, as before. A Finding view's ID
  hashes the view, so view IDs differ from the ones an earlier release computed for the same
  Finding. A `FindingReport@1` relation only `corroborates` a `finding`: the `disputes` kind and
  the `report` target, which no release wrote, are gone from `finding-report-v1.json`, and a
  Report that uses them no longer decodes.
- Review pipeline format 1 is gone. A pipeline that declares `version = 1`, the format without
  `[subject]` whose untyped `findings`, `prior_findings` and `change_set` ports were typed by their
  names, is refused as an unsupported version; formats 2 through 5 still load (see the typed-port
  entry below). Declare
  `version = 2` with `[subject]` and typed `FindingSet@1` or `ChangeSet@1` ports instead. A
  Campaign whose manifest pinned a format 1 pipeline can no longer be resumed or continued.
- Campaign review has one reviewer contract, `review.kernel/ReviewerResult@2`. A pipeline is
  refused when it loads if a reviewer declares `review.kernel/ReviewerResult@1` or an untyped
  result output such as `outputs = ["result"]`, or if its Generation emits
  `review.kernel/PriorFindings@1`. Every reviewer and every Scatter must declare one optional,
  singular `review.kernel/FindingSet@1` input with snapshot affinity `any`, wired from
  Generation's `FindingSet@1` output. A reviewer without it used to run as `ReviewerResult@1`,
  and a Scatter without it fell back to `@1` silently; both are now refused at plan time.
  Reviewers answer `dispositions`, one per assigned prior Finding (`corroborate`,
  `not_reproduced` or `dispute`, keyed by `finding_id`), instead of `disputes` keyed by
  `claim_id` with `confirm` or `refute`. `af onboard` and the software starter already write this
  wiring; update a hand-written pipeline and its pin in `.af/af.lock`. The
  `reviewer-result-v1.json` schema and its conformance corpus are gone, and
  `reviewer-result-v2.json` now defines the flat report shape itself. `finding-set-v1.json` accepts
  only `review.kernel/finding-reducer@2` and `task-review-result-metadata-v1.json` only
  `ReviewerResult@2`, so a stored `ReviewerResult@1` result or a Finding Set reduced by
  `finding-reducer@1` no longer loads; a Ledger that reduces no reviewer result now records
  `finding-reducer@2` as well, so that Finding Set's ID differs from an earlier release's. A
  command reviewer's stdin document now always carries
  `"result_contract": "review.kernel/ReviewerResult@2"`.
- `review.kernel/ReviewerResult@2` no longer has the reviewer's `verdict` and `summary`, which no
  decision, report or display read. A result is exactly `reports`, `benchmark_demands` and
  `dispositions`, and the model output contract no longer asks for the other two. A model reviewer
  that still sends them is unaffected, because its answer is normalized to the contract. A command
  or Task reviewer Worker whose reply still carries them is refused as
  `unexpected_or_missing_fields`: drop both keys from its output and from the package's
  `outputs/result.schema.json`, then re-pin the package digest. That includes the `bugs` and
  `correctness` Workers of a starter an earlier `af catalog init --profile software` wrote; run
  it again into a new directory to get the current ones.
- Every review pipeline port is a typed table, in every pipeline format. The string shorthand
  (`outputs = ["decision"]`, `inputs = ["reports"]`) is refused when the pipeline is parsed, and so
  is a node without `outputs`, which used to get an implicit `out` port. A Gate, Gather or Ledger
  output typed `review.kernel/Opaque@1`, the type the shorthand stood for, is no longer retyped by
  its node kind: it is refused as an unsupported Review output before the Round's Gate runs, and
  the Store no longer skips payload validation for Opaque@1 artifacts. Spell each port out as
  `{ name = "…", type = "…", cardinality = "one", optional = false, snapshot_affinity = "any" }`:
  a Gate outputs `review.kernel/GateDecision@1`, a Gather `review.kernel/ReportSet@1`, and a
  Ledger exactly one `review.kernel/FindingSet@1`, optionally beside a `review.kernel/DemandSet@1`;
  a Ledger without that one Finding Set output is refused when the pipeline loads, where a lone
  untyped Ledger output used to receive the Finding Set by position. `af onboard` and every
  shipped pipeline already write typed ports. A Campaign whose manifest pinned a pipeline with the
  shorthand can no longer be resumed or continued.
- Brokered credentials are gone; GA has no broker. A format 4 or 5 reviewer's `execution` accepts
  only `credential_mode = "credential_free"` or `"trusted_unsafe"` and `auto_apply`:
  `credential_mode = "brokered"` and an `operations` list are refused when the pipeline is parsed,
  in `af review plan` too. They used to pass `plan` and fail only at `af review run`, because no
  adapter could serve them. No shipped pipeline declares either. Provider admission always
  compiles to `af/TaskProviderAdmission@1` with the `af/TaskProviderContext@1` readiness context.
  The captured `af/LegacyReviewTaskPolicy@4` drops `settings.provider_probes`, which was always
  empty, and holds the review settings directly under `settings`, so a new Review Task's policy
  digest differs from an earlier release's. The `task-provider-admission-v2.json`,
  `task-provider-context-v2.json` and `task-provider-probe-policy-v1.json` schemas are deleted, and
  `compiled-task-v1.json` loses the `provider_admission_brokered` operator.
- The Broker's Task records went with the Broker; no release wrote them, because none ever
  installed a Broker. `TaskBrokerTransition@1` is no longer an event type, so a Task log holding
  one fails to replay. `af task show --json` and `af task explain --json` no longer emit
  `af/task-inspection@4`, a `broker_records` section or a `broker_transition` history entry, and an
  `af self optimize` history source with the `af` adapter refuses an `af/task-inspection@4`
  receipt. The `task-broker-binding-v1.json`, `task-broker-operation-v1.json`,
  `task-broker-transition-v1.json`, `broker-operation-receipt-v2.json` and
  `task-inspection-v4.json` schemas are deleted, and `run-event-v1.json` and
  `task-inspection-v11.json` drop their Broker entries.
- Task inspection has one version. Every command that prints a Task as JSON (`af task show`,
  `explain`, `plan`, `start`, `run`, `approve` and the others, and `af self optimize`) now emits
  `af/task-inspection@11`, instead of a version from `@3` to `@11` chosen by the sections the Task
  happened to have. Each section beyond the core (`owned_child_sets`, `review_handoffs`,
  `review_integrations`, `attempt_walls` with `runtime_observations`, `experiments` and
  `adoption_observations`) appears only when the Task recorded it, and `history` holds
  `TaskTransition@5` payloads. `task-inspection-v11.json` is now self-contained and
  describes all of it; it no longer requires `experiments` and `adoption_observations`, and a
  Failed settlement's `diagnostic` must be a JSON object, as this release always writes it. The
  `task-inspection-v3.json` and `-v5.json` to `-v10.json` schemas are deleted, and
  `task-list-entry-v2.json` and `task-plan-inspection-v1.json` now refer to
  `urn:af:schema:task-inspection:11`. The `af` history adapter of `af self optimize` accepts only
  `af/task-inspection@11` receipts, so a receipt an earlier release exported is refused. A script,
  skill or hub check that matches another `af/task-inspection@N`, or validates against a deleted
  schema, must switch to `@11`.
- Task usage and Review provenance have one encoding each. Every usage artifact is
  `af/TaskTokenUsage@3` and every Task Review provenance artifact is
  `af/TaskReviewAttemptProvenance@2`, whatever the width of their counters; narrow values no
  longer select `af/TaskTokenUsage@1` or `@2` or `af/TaskReviewAttemptProvenance@1`. The
  payload bytes are unchanged, but the artifact types and content IDs of new usage and
  provenance artifacts differ from those an earlier release wrote. A Task Review selection whose
  provenance or usage artifact carries a retired type, or no artifact envelope at all, is
  refused, and the `task-token-usage-v1.json`, `task-token-usage-v2.json` and
  `task-review-attempt-provenance-v1.json` schemas are deleted: every other schema now refers to
  `urn:af:schema:task-token-usage:3` for its decimal counters.
- Task records have one version per name. `TaskTransition@1` to `@5` collapse to
  `TaskTransition@5`, `af/TaskExecutionRecord@1`, `@3`, `@4` and `@5` to
  `af/TaskExecutionRecord@5`, `af/TaskRunReport@1` and `@2` to `af/TaskRunReport@2`, and
  `af/TaskReviewHandoff@1` and `@2` to `af/TaskReviewHandoff@2`. Every change kind, record kind
  and field is kept: the new versions are supersets of the old ones, so a transition now carries
  `review_continued`, `review_integration_selected`, `review_integration_finished`,
  `recording_resumed` or `adoption_observation_recorded` under the same number as `opened` or
  `finished`; a settlement or usage observation carries its decimal-text `charged_tokens`, and
  the owned-child and experiment kinds travel in the same record type. An Integration phase
  report is `af/TaskRunReport@2` with a `phase_id`; a Round report is the same type without one.
  The `task-transition-v1.json` to `-v4.json`, `task-execution-record-v1.json`, `-v3.json` and
  `-v4.json`, `task-run-report-v1.json` and `task-review-handoff-v1.json` schemas are deleted,
  and `run-event-v1.json` and `task-inspection-v11.json` name only the surviving versions. This
  is a wire break: a `.af/state` Task log or CAS record an earlier release wrote no longer
  decodes, and `af task list` and `af self optimize` fail for the whole store while one remains,
  rather than skipping it. Finish or delete in-flight Tasks before upgrading. The
  `execution_records[].artifact_type` and `review_handoffs[].artifact_type` values in
  `af task show --json` each collapse to one string.
- A Task catalog and its captured run authority have one schema each: `af.task-catalog/2` and
  `af.task-run-authority/2`. `provider_admission` is now optional in a catalog; omitting it means
  the fixed 4,096-token, 45-second admission allowance that `af.task-catalog/1` had, and
  declaring it keeps the explicit bounded cost. A committed `.af/task-catalog.toml` that still
  says `schema = "af.task-catalog/1"` is refused with `Task catalog requires schema
  af.task-catalog/2`; change that one line by hand (nothing else about the file changes).
  `af catalog init` now writes `af.task-catalog/2` for every profile. The captured run
  authority always records the resulting admission cost and re-checks it against the captured
  catalog bytes, and `task-catalog-v1.json` is deleted.
- The Attempt wall sidecar in `events.sqlite` keeps one exact usage column, `usage_v3_json`,
  beside `usage_observation_v1_json`; both are created with the `attempt_wall` table. The numeric
  token columns, `usage_v1_json`, `usage_v2_json` and the `ALTER TABLE` migration that ran on
  every open are gone, as is the Campaign-keyed narrowing view only the removed pre-Task executor
  wrote. Wall rows an earlier release recorded in the retired columns are no longer read, so
  finish in-flight Tasks before upgrading: recovery raises an abandoned Attempt's charge from its
  recorded wall usage, and without it the Attempt settles at its reservation floor.
- `af review run --json` always emits `af/review-outcome@3`, instead of `@2` when every selected
  Attempt's usage fitted u64, and `review-outcome-v2.json` is deleted.
- `af review report --json` always emits `af/review-report@4`, instead of `@3` for narrow usage or
  `@1` for a Campaign with no Task. `task_accounting` is always present; it is empty exactly for a
  Campaign whose first Task capture failed, which has no Rounds either, and
  `review-report-v4.json` describes that case. `review-report-v3.json` is deleted. A script, skill
  or hub check that matches `af/review-outcome@2`, `af/review-report@1` or `af/review-report@3`,
  or validates against a deleted schema, must switch to the single version.
- The reviewer output contract a model Worker is prompted with now names its report array
  `reports`, the key the stored `ReviewerResult@2` artifact and every Worker package schema
  already used, instead of `findings`. An answer that still says `findings` is read as `reports`,
  so hand-written command reviewers keep working, but the prompt bytes changed: reviewer Attempt
  context IDs differ from those an earlier release computed for the same node.
- This file starts at the first GA release: the `0.7.0`–`0.9.0-rc.6` sections are out of it and
  stay on the GitHub release pages and in git history. `README.md` and `SECURITY.md` now state
  the GA policy — compatibility obligations start at 1.0, and only the latest `1.x` minor line
  receives security fixes.

### Changes

- Make Provider onboarding safe for automated callers (ADR-0112): refuse to start an official CLI
  login unless the operator opted in with `--login` *and* the process owns an interactive terminal
  on stdin, stdout and stderr, returning the `human_action_required` result with the exact
  private-terminal command instead; warn about OAuth URLs and authorization codes before handing
  over the terminal; keep registering an already-authenticated context with no login at all; add
  stable versioned `af/provider-status@1` and `af/provider-setup@1` documents under `--json` that
  distinguish registration, authentication, usable-or-untested and usage without exposing account
  email, organization identity, credentials, OAuth material or raw Provider output; make
  `af provider status` a fast registry and authentication check with subscription and quota probes
  behind `--usage`, where an unavailable probe exits 7 and leaves an authenticated Provider
  authenticated; document exit codes 3 (human action required), 4 (Provider CLI missing), 5
  (registry conflict), 6 (authentication failed) and 7 (usage unavailable) with results on stdout
  and diagnostics on stderr; and keep `af provider doctor` the charged end-to-end usability check.
- Remove the `task_planning` integration-test timing race: a Task deadline is absolute
  wall-clock from creation, and the generated-plan tests hold one Task open across a long chain
  of `af` invocations, Git commits, catalog operations, signing and Python Workers. Under the
  four-thread full test gate those subprocesses consumed the fixtures' 60s budget, so valid
  generated-plan and imported-catalog resumes were correctly but unhelpfully refused with `Task
  deadline protects still-required verification`. The long-lived Tasks in
  `crates/af/tests/task_planning.rs`, `task_catalog.rs` and the native-model cases in `task_file.rs`
  now use a documented ten-minute wall budget, added to the total and taken from nothing:
  per-Attempt walls, Attempt counts, token budgets and the verification reserve keep their fixture
  values, and no production code changed. New tests pin both halves — only the total fixture wall
  moves, and `TaskBudget::prepare` keeps its exact deadline boundary and refusal wording at both a
  small and a large budget
  ([ADR-0114](docs/adr/0114-budget-cli-task-fixtures-for-loaded-machines.md)).
- The internal delivery records are out of `docs/`: the `P00`–`P14` package checklist, the product
  backlog, the release-timing and validation-cost measurement records, the self-optimizer plan and
  review record, and the pre-implementation design notes the shipped architecture was ported from
  (`docs/design/{overview,entities,state-machines,config,store,research,task-execution,task-execution-examples}.md`).
  Git history keeps them. The engineering values are now [`docs/values.md`](docs/values.md), and
  `docs/design/` keeps only the two designs still in flight, warm layers and the self-optimizer.
  `CONTRIBUTING.md` now documents the opt-in `make check TEST_RUNNER=nextest` runner.
