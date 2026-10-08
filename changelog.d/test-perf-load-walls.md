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
  `capture::tests::held_stdout_capture_and_compatibility_wrapper_keep_the_same_failure` (1 s);
  `cancellation::cancellation_retains_prefixes_and_stops_running_stdin_and_each_held_drain`
  (5 s readiness, 10 s wall).
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
  `task_runtime::control::captured_command_cancellation_retains_both_streams_and_never_retries`
  (3 s readiness),
  `task_runtime::domain_observes_started_attempt_and_persists_through_the_runtime_store` (2 s
  Store wait), `review_domain::integration::tests::`
  `controlled_check_sequence_retains_interrupted_raw_result_and_stops_before_the_next_check`
  (20 s Check wall, 3 s readiness).
- Unchanged, subject is a timeout or deadline (every wall kept, including fixture-preparation
  walls inside them): codex and claude `task_worker::`
  `timeout_and_cas_failure_preserve_reported_overrun_without_admitting_the_message`; claude
  `task_model_usage::synthetic_native_multi_model_usage_survives_refusal_timeout_and_cas_outage`;
  `model_supervision::a_hung_reviewer_is_killed_at_the_deadline`,
  `a_killed_reviewer_keeps_what_it_wrote_so_far`,
  `a_model_parent_exit_cannot_leave_the_stdin_writer_unbounded`; `review-source-task`
  `native_source_cancels_inflight_process_and_bounds_output` and
  `replacement_sources_have_equivalent_requirements_and_exact_field_provenance`, which also
  assert `TimedOut`; `review-check` `deadline` tests;
  `review-source-git` `git_deadline`; `review-process` `concurrent_pipes` and `drain` unit tests;
  `review-sandbox` container probe, wedged-runtime, reap and timed-out-container tests (with
  their shared `write_runtime` helper); `af` `providers::installation` probe-deadline test and
  `identity_rechecks_use_remaining_attempt_deadline_and_prespawn_cancellation`;
  `review-pipeline` `legacy_check_order_and_one_writable_clone_survive_the_shared_sequence`,
  `an_absolute_attempt_deadline_refuses_setup_and_bounds_the_running_check` and
  `unconfirmed_container_cleanup_preserves_the_writable_sandbox_and_stops_checks`, whose
  container is ended by its 100 ms Check wall.
- Unchanged, no process starts under the wall: `model_supervision::`
  `a_missing_provider_is_unavailable_not_silent`,
  `an_untrusted_option_is_refused_before_the_model_starts`;
  `cancellation_before_spawn_cannot_execute_the_command`;
  `capture::tests::spawn_failure_does_not_invent_output`;
  `a_removed_executable_with_no_installed_client_refuses_by_name_before_any_probe`;
  `an_installed_but_broken_runtime_is_unusable_not_usable`; and `task_model_transport`, whose
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
