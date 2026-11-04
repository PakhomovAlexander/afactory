# Nightly fixes and local check report verification

Local verification on 3 November 2026, macOS, pinned Rust 1.88.0. This continues the candidate
for issues [#575](https://github.com/PakhomovAlexander/afactory/issues/575) and
[#576](https://github.com/PakhomovAlexander/afactory/issues/576). These are focused local test
runs, not a claim that the Task's subsequent sealed-candidate gate or independent evaluation
has already run.

## Original fixes

The supplementary groups in the build and publish units and the exact RC4/RC5 telemetry
contract declarations were already present in the continued candidate and remain unchanged.
The complete focused suites ran, including their temporary-directory setup, fake binary
execution and fixture copying:

```sh
TMPDIR=/private/tmp python3 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_nightly_measure.Units","tests.test_telemetry_bench_contracts"],verbosity=1)'
```

Result: **18 tests, OK**, exit 0, 5.350 seconds. This includes
`test_build_and_publish_read_nightly_secrets_and_build_keeps_docker_access`, both version
selection/refusal tests and
`test_rc4_and_rc5_keep_all_committed_fixtures_without_contract_specific_pins`.
These tests remain in the perf laboratory selection; the fast gate does not claim to run them.

All seven current source blob identities and four fixture catalog identities in
[the RC4/RC5 evidence](telemetry.md#performance) were rechecked with `git hash-object` and match
the recorded values. This sandbox has no Git repository or historical release objects, so this
run does not claim to repeat the earlier comparison against the two release revisions.

## Report repair

The native gate ran nextest separately for each class and wrote multiple `tests` records to
`AF_CHECK_REPORT`. The installed Task runtime refused the second record. The producer now
collects those same records privately and applies the existing fold before publishing one
`af.check-report/1` tests record. Step records still stream directly, including build completion.
The receipt schema and parser, class selection, isolation reruns and their bounds are unchanged.

`test_verify_folds_class_tests_into_one_record_and_keeps_streaming_steps` exercises the real
native wrapper and producers with inexpensive class payloads. It checks success, a failing
second class, an early stop and malformed private evidence, including exact exit statuses.
Against copies of the three producer scripts from before this repair, it fails with **3 != 1**
tests records on success and **2 != 1** on failure. With the repair it passes.

`test_gate_test_fold_keeps_bounds_counts_outcomes_and_refuses_malformed_runs` covers empty and
single runs, all rerun states, complete and capped failure counts, duplicate names, output
bounds, malformed fields, duplicate fields, count overflow and preservation of earlier steps.

The normal local Task runtime test
`task_gate::the_shipping_gate_producers_write_one_tests_record_the_task_runtime_admits`
uses the shipping producers for three class runs. It asserts a single raw tests record, an
admitted `af/TaskCheckReport@1`, preserved step and flaky-test evidence, a passed gate and
satisfied Task acceptance after evaluation. It uses the existing private HOME/XDG helper.

Executed checks:

- `cargo fmt --all`: passed.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`: passed.
- `python3 -I -B scripts/test-test-classes.py`: **55 tests, OK**, 135.999 seconds; the
  additional standalone-report assertion also passed in its focused rerun.
- `python3 -I -B scripts/test-nextest-gate.py`: passed, including real sealed-source Rust
  fixture gates, isolation reruns and failed-gate propagation.
- `python3 -I -B scripts/test-test-time-report.py`: passed.
- `cargo nextest run --workspace --locked --offline --profile ci -E 'binary_id(af::it) & test(/^task_gate::/)'`:
  **12 tests run: 12 passed**, 26.523 seconds; the new Task test took 3.313 seconds.
- A second selection also included `review-core`'s `task::check_report::tests` module:
  **24 tests run: 24 passed**, 25.362 seconds.
- `python3 -I -B scripts/test-classes.py audit`: passed, with existing classifications.
- `python3 -I -B scripts/performance/cli.py harness-digest --check`: passed; the protected
  harness stays `sha256:2b182f6cb059e752ddc312974b9a899f65e2b01d47c1e5a4cc05e3d155a2b651`.
- `python3 -I -B scripts/changelog-notes.py --check`: passed.
- Markdownlint: **not run**. Offline npx reported `ENOTCACHED` for
  `markdownlint-cli2@0.22.1`; the Task's configured markdownlint gate supplies its own warm cache.

The repair adds two Python tests to the existing functional script suite and one local Task
test to the existing e2e module, with no new test binary, sleep or timeout assertion. A focused
run of the two Python tests took 3.746 seconds; together with the new Task test that is about
7.1 seconds of measured test execution on this host, not a gate wall-time guarantee. Production
overhead is one final Python invocation per reported native gate and bounded folding of its
small run file; startup, Store replay and source capture are unchanged. No remote gate,
deployment or live unit change was used.

## Step-field continuation

The next refusal, `a step needs a name, its command and at most a bounded printable fix`,
is the strict parser's combined step-field validation. Inspection found two shipping producer
violations: `run_class` used script filenames such as `test-release-resolve.py` as labels,
where `is_name` permits only ASCII alphanumerics, underscores and hyphens (first character
alphanumeric, at most 128 bytes); it also put the complete generated Rust filter in one
command argument, which can exceed the parser's 1,024-character printable-text bound.
The previous outer run's raw report is unavailable here, so this diagnosis identifies
reproducible violations without claiming which exact line that run refused first.

The class runner now uses filename stems for script labels and writes the complete selection
to `<label>-filterset.txt` beside the inventory. The reported command uses
`nextest-gate.py --filterset-file PATH`; that script expands the file to the original `-E`
argument before nextest executes or constructs isolation reruns. Test selection, inventory
comparison, report folding and the strict parser are unchanged. No schema shape changes.

`SealedEvidence.test_selected_build_receipts_cover_all_variants_before_any_suite` now uses
100 additional inexpensive fixture test identities, making a generated filter longer than
1,024 characters. It executes the real native wrapper, class runner and step writer with
stand-in suite payloads, checks every emitted step's label and bounded printable command
fields, and retains its successful/failed build and hosted/native assertions.
`scripts/test-nextest-gate.py` checks exact file expansion, isolation selection removal and
refusal of missing filter files; its existing real tiny Rust fixture gates still pass.
The Task runtime regression named above now feeds the actual `run_class` labels and commands
(including a script filename, a long generated selection and a build fix) through the normal
check-report parser. Its suite payloads and inventory acquisition are test doubles; its
assertions require report admission and satisfied fixture Task acceptance.

Continuation checks on 3 November 2026:

- `cargo fmt --all`: passed.
- `python3 -I -B scripts/test-test-classes.py`: **55 tests, OK**, 124.126 seconds.
- `python3 -I -B scripts/test-nextest-gate.py`: passed, including its real fixture gates.
- The nightly Units and telemetry contract command above: **18 tests, OK**, 7.663 seconds.
- Rust Clippy and filtered workspace nextest: **not completed**. Offline resolution lacks
  `num-bigint`; normal Clippy also fails creating the configured Cargo home registry cache
  with `Operation not permitted`. The expanded Rust regression has not run in this Attempt.
- Markdownlint: **not run**; offline npx reports `ENOTCACHED` for version 0.22.1.

These checks are pre-seal regression evidence only. The independent evaluator must separately
inspect the outer Task's actual `af/TaskCheckReport@1` linked through its `receipt_id` to the
check receipt for the exact sealed Snapshot, and require the kernel check's `report` to be
`admitted`. A passed process receipt alone cannot prove admission. Neither the previous
outer report nor this Attempt's future sealed-run report is available in this sandbox;
accepted implementation, required local gates and independent goal evaluation remain the
outer pipeline's obligations. The fixture Task's admission is evidence for the repaired
producer/parser path, never a substitute for the outer Task's report.
