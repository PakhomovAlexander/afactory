# Nightly fixes and local check report verification

The candidate for issues [#575](https://github.com/PakhomovAlexander/afactory/issues/575) and
[#576](https://github.com/PakhomovAlexander/afactory/issues/576) retains the permanent unit and
native report repairs. The current telemetry continuation replaces historical fixture overlays with one native Task
benchmark contract. These are source and regression checks; sealed Task
acceptance remains the outer runtime's responsibility.

## Secret access and telemetry contracts

The build and publish units explicitly add supplementary group `af-nightly`, retaining primary
group `af-perf-readers`. The build also adds `docker`. The signing key and publish tokens stay
`root:af-nightly` 0440, and the units retain their hardening and resource bounds.
`test_nightly_measure.Units` checks the actual committed units, including
`test_build_and_publish_read_nightly_secrets_and_build_keeps_docker_access`.

Current telemetry uses `native-task-review@1`, with exact source-schema admission, committed
package digest checks and the measured binary's own zero-Attempt planner. The
[contract description](telemetry.md#performance) supersedes the historical release tables.
`test_telemetry_bench_contracts.FixtureContracts` covers current identity, malformed contracts,
package tampering, zero-Attempt admission and original early-exit diagnostics. The Store fixture
now has its own native Task catalog. Release version alone supplies no capability proof.
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
