# Release harness preflight execution record

Execution source: the implementing Worker in this Task sandbox, 2026-11-07.
The requested original base is `b51e3e36afa57b8863b2d8f7f2ca030e65f98f47`; this
sandbox has no Git metadata, so the Worker did not independently resolve that commit.
The before observations below were taken on the supplied source before regeneration.
All recorded commands ran from the repository root, with `TMPDIR` set to the absolute
checkout path of `target/tmp`. Empty streams are explicitly identified.

The Task requirements report that a prior native Task passed its required gate and
correctness review, but its evaluator could not execute focused tests in its read-only
sandbox. That prior native outcome remains inconclusive, not verified. This record is
Worker execution evidence, not independent attestation or a replacement for the mandatory
enclosing native gate, correctness review and evaluation.

## Base canonical check (before regeneration)

Started (UTC): `2026-11-07T18:33:54.549077+00:00`.

```sh
python3 scripts/performance/cli.py harness-digest --check
```

Exit status: `1`.

stdout: empty.

stderr:

```text
stale: fixtures/performance/kernel-measure/code-policy-measures.toml is not what the protected harness sha256:b5019e34bfa66a30980e7e77bffeb1d59838f5847e6ff379248659f003785da2 renders
stale: fixtures/performance/kernel-measure/inventory.json is not what the protected harness sha256:b5019e34bfa66a30980e7e77bffeb1d59838f5847e6ff379248659f003785da2 renders
run python3 scripts/performance/cli.py harness-digest --write and follow fixtures/performance/kernel-measure/README.md to re-pin
```

## Canonical regeneration

Started (UTC): `2026-11-07T18:33:55.423924+00:00`.

```sh
python3 scripts/performance/cli.py harness-digest --write
```

Exit status: `0`.

stdout:

```text
sha256:b5019e34bfa66a30980e7e77bffeb1d59838f5847e6ff379248659f003785da2
```

stderr:

```text
re-pinned fixtures/performance/kernel-measure/code-policy-measures.toml and fixtures/performance/kernel-measure/inventory.json to sha256:b5019e34bfa66a30980e7e77bffeb1d59838f5847e6ff379248659f003785da2; review the change, then re-install it into .af/ by hand
```

## Candidate canonical check (after regeneration)

Started (UTC): `2026-11-07T18:33:56.237926+00:00`.

```sh
python3 scripts/performance/cli.py harness-digest --check
```

Exit status: `0`.

stdout:

```text
sha256:b5019e34bfa66a30980e7e77bffeb1d59838f5847e6ff379248659f003785da2
```

stderr: empty.

## Focused Bootstrap and Staged tests (one execution)

Started (UTC): `2026-11-07T18:33:57.046083+00:00`.

```sh
python3 -I -B -c 'import sys, unittest; sys.path.insert(0, "scripts/performance"); unittest.main(module=None, argv=["unittest", "-v", "tests.test_kernelguard.Bootstrap", "tests.test_kernelguard.Staged"])'
```

Exit status: `0`.

stdout: empty.

stderr:

```text
test_case_variant_build_inputs_are_protected (tests.test_kernelguard.Bootstrap) ... ok
test_links_special_files_and_missing_inputs_are_refused (tests.test_kernelguard.Bootstrap) ... ok
test_the_bootstrap_imports_nothing_from_the_measured_tree_before_its_check (tests.test_kernelguard.Bootstrap) ... ok
test_the_bootstrap_refuses_before_importing_the_candidate_driver (tests.test_kernelguard.Bootstrap) ... ok
test_the_digest_covers_exactly_the_protected_closure (tests.test_kernelguard.Bootstrap) ... ok
test_a_pin_made_in_a_worker_sandbox_ignores_its_home_and_cargo_state (tests.test_kernelguard.Staged)
A Worker sandbox's HOME may lie inside the tree, or be the tree, and Cargo writes its ... ok
test_an_installed_guarded_measure_must_be_the_staged_one (tests.test_kernelguard.Staged) ... ok
test_the_cli_checks_and_re_pins (tests.test_kernelguard.Staged) ... ok
test_the_staged_policy_is_rendered_and_checked_deterministically (tests.test_kernelguard.Staged) ... ok

----------------------------------------------------------------------
Ran 9 tests in 3.480s

OK
```

## Bounded inventory and canonical rendering comparison

The Worker saved the original inventory before `--write`, then loaded that copy and the
generated inventory with Python stdlib JSON. Assertions compared unique path maps, counts,
path sets, every size and the complete set of changed content hashes. Both generated files
were also compared byte-for-byte to `kernelguard.staged_documents` on the candidate.
The comparison exited 0, with empty stderr and this stdout:

```json
{
  "before_digest": "sha256:d6ee4e5fad3c5299df0760811764c30697f3e15e55514accb01372d59d1bb82c",
  "after_digest": "sha256:b5019e34bfa66a30980e7e77bffeb1d59838f5847e6ff379248659f003785da2",
  "before_count": 790,
  "after_count": 790,
  "added_paths": [],
  "removed_paths": [],
  "changed_sizes": [],
  "changed_hashes": [
    {
      "path": "crates/review-pipeline/examples/perf-probe/authority.rs",
      "size": 24426,
      "before": "sha256:9d1e922e5248e1eeabd491a4702c3c618225aeeabd367ecf9964f279792469a3",
      "after": "sha256:ca9a67b4c7d65fc6d2a326a68d2f571bc3ef5427da06f5354f1be7685a06dfbd"
    }
  ],
  "canonical_bytes_equal": [
    "fixtures/performance/kernel-measure/code-policy-measures.toml",
    "fixtures/performance/kernel-measure/inventory.json"
  ]
}
```

No protected source bytes were edited. The 790 paths and all sizes are retained; only the
inventory hash of the existing 24,426-byte authority adapter changes. The canonical generator
and literal bootstrap, metrics, thresholds, repetitions, installed `.af/` authority and
historical receipts remain unchanged. No benchmark or product measurement ran, and there
is no performance claim. Actual release preparation remains the separate `make release`
path after source verification and integration.

## Validation scope

Only the existing five Bootstrap and four Staged tests were selected; no tests were added,
and their single execution is recorded above. The unrelated Adapter suite and a duplicate
full fast gate were not run. This metadata-only Attempt uses the focused Python tests and
exact canonical check authorized by the request; Rust format, clippy and nextest are left
to the mandatory enclosing gate. No Cargo build, registry extraction or dependency change
was needed. Added recurring gate work: zero tests and zero executable code paths.
