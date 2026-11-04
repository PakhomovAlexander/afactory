# Nightly fixes and local check report verification

The candidate for issues [#575](https://github.com/PakhomovAlexander/afactory/issues/575) and
[#576](https://github.com/PakhomovAlexander/afactory/issues/576) retains the permanent unit and
native report repairs. The RC6 continuation adds an explicit telemetry fixture contract and
restores coverage across RC4, RC5 and RC6. These are source and regression checks; sealed Task
acceptance remains the outer runtime's responsibility.

## Secret access and telemetry contracts

The build and publish units explicitly add supplementary group `af-nightly`, retaining primary
group `af-perf-readers`. The build also adds `docker`. The signing key and publish tokens stay
`root:af-nightly` 0440, and the units retain their hardening and resource bounds.
`test_nightly_measure.Units` checks the actual committed units, including
`test_build_and_publish_read_nightly_secrets_and_build_keeps_docker_access`.

The [source evidence](telemetry.md#performance) records the receipt writer, definition, schema,
package digest paths and fixture pins supporting `check-receipt@2` for RC4, RC5 and RC6.
`test_telemetry_bench_contracts.FixtureContracts` checks explicit selection, requested-contract
mismatch and unnamed-version refusal. Its
`test_rc4_rc5_and_rc6_keep_all_committed_fixtures_without_contract_specific_pins` checks every
fixture for all three versions, independently validates the committed package pins and requires
byte-identical copies with no receipt renaming. The Store fixture has no Task catalog.
`test_workspace_release_has_an_explicit_valid_telemetry_contract` reads `Cargo.toml`, preventing
a workspace version bump from silently shipping without a declared telemetry workload.
These tests stay in the perf laboratory selection; the fast gate does not claim to run them.

## Native report producers and admission

The native gate collects nextest's per-class records in a private spool and publishes one
complete `af.check-report/1` tests record. Step records still stream directly, including build
completion. The receipt schema, strict parser, class selection and isolation rerun bounds stay
unchanged.

The class runner uses filename stems for script labels and retains the complete Rust selection
in `<label>-filterset.txt` beside the inventory. The reported command uses
`nextest-gate.py --filterset-file PATH`, which expands to the original `-E` argument before
execution or isolation selection replacement. This preserves the full selection while keeping
labels and command arguments within the parser's printable field bounds.

- `test_verify_folds_class_tests_into_one_record_and_keeps_streaming_steps` exercises the
  native wrapper and producers with inexpensive class payloads: passing and failing classes,
  early stops, malformed private evidence and exact exit statuses.
- `test_gate_test_fold_keeps_bounds_counts_outcomes_and_refuses_malformed_runs` checks counts,
  rerun states, caps, duplicate names and fields, malformed input and earlier-step preservation.
- `test_selected_build_receipts_cover_all_variants_before_any_suite` checks real class-runner
  labels and bounded printable fields, including a generated filter longer than 1,024 characters.
- `scripts/test-nextest-gate.py` checks exact filter expansion, isolation selection replacement,
  missing-file refusal and real tiny Rust fixture gates.
- `task_gate::the_shipping_gate_producers_write_one_tests_record_the_task_runtime_admits`
  passes the shipping producers' output through the normal local Task runtime. It asserts one
  raw tests record, report admission, preserved steps and flaky evidence, and satisfied fixture
  Task acceptance after evaluation. It exercises real class labels and commands with stand-in
  suite payloads and inventory acquisition, using the existing private HOME/XDG helper.

The protected-harness policy and inventory are generated with
`python3 -I scripts/performance/cli.py harness-digest --write`; `--check` verifies them.

## Evidence boundary

Independent judges assess observable code regressions and supplied check receipts. They are
not required to fetch an outer Task report unavailable in their inputs or to require future
final acceptance before evaluation. After the sealed gate, the coordinator/operator checks
that its actual `af/TaskCheckReport@1` is linked by `receipt_id` to the check receipt for the
exact Snapshot and that the kernel check's `report` is `admitted`. A passed process alone does
not prove admission. Final implementation acceptance belongs to the runtime; the nested fixture
regression proves the producer/parser path and does not replace that outer postcondition.
