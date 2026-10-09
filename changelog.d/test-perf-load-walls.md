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
