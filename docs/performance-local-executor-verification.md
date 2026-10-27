# Independent executor: focused implementation verification

Recorded 2026-10-27 in the AF implementation sandbox, using Python 3.14.4. The unchanged
Rust pin is 1.88.0. This note records implementation checks; it is not VM measurement evidence.

## Final focused command

```sh
python3 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_executor","tests.test_localcalibration","tests.test_contracts","tests.test_hostlock","tests.test_builds.ContainerBuild","tests.test_planning","tests.test_telemetry_lab","tests.test_telemetry_lock_handoff","tests.test_telemetry_build_bounds","tests.test_telemetry_cleanup","tests.test_telemetry_terminal","tests.test_runner.Preflight","tests.test_runner.HostCancellation","tests.test_diagnostics.Command","tests.test_suite","tests.test_continuous.HostedScratch","tests.test_calibration.ReadyCohort","tests.test_calibration.OriginalDispatch","tests.test_calibration.Declarations","tests.test_calibration.Arithmetic"],verbosity=1)'
```

**PASS: 223 tests, 144.400 seconds, exit 0.** New tests are under the existing performance
unittest discovery contract. The resolver, catalog, native full gate and hosted workflows
were not changed. Performance coverage is required for this change; `no-perf` would exclude
the behavior being changed.

The checks exercise actual independent processes with different HOME/TMPDIR values, real
flocks and cleanup barriers across core, telemetry, diagnostics, scratch preparation and
service entry points. They cover killed controllers with live owned children, cancellation,
deadlines, child failure, subsequent acquisition, retained uncertainty, unsafe lock paths and
inode anchors. Actual Git objects, cross-account ownership checks, journal claims/recovery,
native raw-sample admission, comparison/rendering, archives/history and local whole-cohort
arithmetic run with bounded synthetic compute and transport seams. Local execution is tested
without Actions identity or transport. A failing verdict and an unavailable diagnostic remain
reportable completed measurements. Hosted dispatch ambiguity never replays an original.

Expanded checks initially exposed workflow fixtures sharing a failed machine fence and using
the sandbox's non-system-owned `/tmp`, plus a publication-only failure losing the hosted safe
cleanup path. Fixtures now use separate simulated machines for independent fault scenarios;
no uncertain fence is removed for progress. Production still refuses the unsafe directory.
Safe cleanup after publication failure requires the original durable stopped/nonpending
ownership receipt and absence of work, within the original deadline. Existing assertions,
statistical minima, quotas and deadlines remain enforced.

## Metadata and protected inventory

These commands each passed with exit 0:

```sh
python3 -B scripts/performance/schemas.py
python3 -B scripts/test-classes.py audit
python3 -B scripts/changelog-notes.py --check
```

Protected inventory was regenerated only through the existing unchanged generator. This
sandbox has a reserved empty `.git` directory rather than usable Git metadata, so a temporary
public copy contained the inventory's existing build closure plus the complete performance
source/fixture trees. The sandbox helper command was:

```sh
python3 -I -B /tmp/regenerate-local-executor-inventory.py
```

It invoked the unchanged `cli.py harness-digest --root COPY --write` and `--check` (both exit
0), copied only the generated staged policy/inventory back, then required the unchanged
bootstrap's digest and every path/size/hash on the actual sandbox closure to match. Result:
615 protected files, `sha256:22f5e652e5a38dccd42dcae9c10eb8ce36550bfb3efb1f0896a92ebde8f35a89`.
The generator, bootstrap and `.af/` authority were not edited.

Historical hosted declarations remain byte-identical:

| Declaration | SHA-256 |
| --- | --- |
| hosted v6 | `62955096a58517dad3544425ead9b18ebe3b85de94b8518d5ce85deb761d5ab7` |
| hosted v7 | `4f975367d9f27855a1318d3b18a0b0845efa26dd259fa85fe555ce45ae536308` |

`systemd-analyze verify` was attempted on temporary copies of the installed unit filenames;
it could not complete in this sandbox because its SO_PASSRIGHTS/SO_PASSCRED socket setup was
denied. Unit verification on the deployment host remains required.

## Remaining external evidence

No Docker/Claw measurement, host/service/GitHub modification, operational rollout, empirical
calibration qualification or speedup is claimed. No real local cohort or commit identity was
invented. The full `scripts/test-performance.py`, `make check` and full Rust suite were not run:
the enclosing unchanged native/Markdown gate and independent Terra/Astra reviews own them.
The coordinator owns authentic local core and telemetry runs, live VM contention/cleanup proof,
artifact/hash checks, report export and final backend selection after acceptance. Follow-ups
#378, #382 and #380 remain separate; local source retention is necessary for the new backend.
