# Independent executor: focused implementation verification

Recorded 2026-10-27 in the AF implementation sandbox, using Python 3.14.4. The unchanged
Rust pin is 1.88.0. This note records implementation checks; it is not VM measurement evidence.
The repair sections below record the latest focused checks and protected inventory.

## Original feature focused command

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

## Narrow CI repair (2026-10-27)

This repairs the supplied public candidate `ec8a7da9f39feeb464ec1cf54e0c70435bc1f62f`
from PR398. The enclosing Task's final unchanged kernel/Markdown gates, Terra bug review,
Astra correctness review and Astra evaluation must judge the **whole feature** against main
`8c0ca71a1df2b73ed7c1d0ba9686a6ec1ddf47b1`, including these repairs. The supplied earlier
Rust/hosted results do not constitute acceptance of this final candidate.

The isolated runner test now forwards `register` and `lease` to the production lock function
while selecting its private test lock. Every existing assertion and checked lease remain.
The smoke child loads the actual CLI through `runpy`, preserving its source-only admission
before library imports and deferring argument dispatch until observation is installed. A
test-only guard observer calls the unchanged guard and records real lock/fence identities
outside candidate mounts before launch. Acquisition, the production lock path and checked
capability remain unchanged; no environment authority or inherited lock descriptor is used.

After synchronously reaping that child and passing the intended attack assertions, the harness
explicitly reacquires the same physical common flock. It verifies the held/named lock inode,
installed anchor when present, owner/type/mode/link count, and the exact empty private fence's
inode, owner and change time. Under that flock it proves no telemetry-build containers remain,
checks the original scratch image/mount identities and size, unmounts/removes only those owned
paths and confirms their absence before settling its fence. Changed, foreign, nonempty or
uncertain state refuses settlement. Failed assertions/timeouts retain the fixture directory
for inspection instead of recursively deleting a possibly mounted filesystem. Production
failure fences and executor behavior are unchanged; there is no recovery/reset product API.

Hostile-build checks require the typed build failure, and resource attacks additionally require
the expected compile failure and an existing compile log before reading it. Existing file,
aggregate-size, sentinel, credential, network, binary and admission assertions remain enforced.

Latest focused command (Python 3.14.4):

```sh
python3 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_runner.Shards.test_the_run_command_refuses_an_ambiguous_mode_and_reports_unavailability","tests.test_telemetry_build_smoke","tests.test_hostlock","tests.test_executor","tests.test_telemetry_lock_handoff","tests.test_telemetry_build_bounds","tests.test_runner.Preflight","tests.test_runner.HostCancellation"],verbosity=1)'
```

**PASS: 73 tests, 37.054 seconds, exit 0.** This includes the exact failing runner test,
18 smoke-maintenance regressions and directly affected existing lock/executor/build tests.
Real independent CLI children and flocks exercise successful/rejected/successful sequencing
on one unchanged lock inode, exclusion during cleanup, refusal while the original lease is
held, changed lock/fence/scratch identities, foreign ownership, symlinks, nonempty fences,
missing reaping proof, live Docker-list evidence, failed unmount and falsely reported cleanup.
Builds, Docker listing and mount operations in these unit tests use explicit stand-ins.
An actual child bootstrap against an unsafe library fixture proves source-only refusal occurs
before any library code executes.

The earlier focused set passed 69 tests in 34.273 seconds before three additional regressions.
It then passed 72 tests in 35.178 seconds before the source-admission regression. An intermediate
exact runner/smoke run passed 18 tests in 5.228 seconds. Development
runs first exposed a typo in the new child helper and a missing-child diagnostic that accessed
`returncode` too early; both were corrected before the passing runs above.

Protected inventory was regenerated through the unchanged generator:

```sh
python3 -I -B /tmp/af-ci-repair-inventory.py
```

**PASS, exit 0:** the sandbox helper copied the public protected closure to a temporary root,
invoked unchanged `cli.py harness-digest --root COPY --write` and `--check` (both exit 0),
copied back only generated staged policy/inventory, and checked exact path/size/hash parity
against the actual sandbox closure using the unchanged bootstrap rules. This handles the
sandbox's reserved empty `.git`; it does not edit `.af/`, the generator or bootstrap.
Latest inventory: **616 protected files**,
`sha256:d284d0c8f8fdec4a0c85925854ec6a7e5c940dac21629b7dc68d89b776a050aa`.

No full gate, full Python/Rust suite, actual Docker smoke, Claw execution, deployment, service
or GitHub change was performed in this repair Worker. Unit results do not establish those
external proofs, independent acceptance, calibration qualification or speedup. The coordinator
owns fresh real Docker CI and deployment/Claw runs after the enclosing Task accepts the complete
feature. Hosted backends, historical cohorts, contracts, limits and statistical minima remain.

## Narrow native validation repair (2026-10-27)

This patch starts from `e27cd335342f406b11d6f8af701b539af51f6eee` and preserves both earlier
CI fixes and their recorded 73-test result above. It removes only the three surplus Markdown
blank lines reported at original `CONTEXT.md` lines 252/272 and
`docs/design/continuous-performance.md` line 1027, preserving all prose.

The fixture-only `state(pid)` reader now treats exactly `ProcessLookupError` alongside
`FileNotFoundError` as absence when a process disappears during its procfs read. The new
deterministic regression injects both exceptions into `Path.read_bytes`, requires absence,
and requires the identical `PermissionError` and unrelated `OSError(EIO)` instances to
propagate. Production `gate-owner.py`, all existing assertions, cleanup/ownership logic,
timeouts, quotas, retry policy, Makefile and gate selection are unchanged. This repairs an
observation race; it does not establish a cleanup repair.

Focused commands with Python 3.14.4:

```sh
python3 -I -B scripts/test-gate-owner.py StateObservation -v
python3 -I -B scripts/test-gate-owner.py -v
```

**PASS:** regression, 1 test in 0.002 seconds, exit 0; complete module, all 13 existing tests
plus the regression, **14 tests in 15.041 seconds, exit 0**, with no skips.

Pinned Markdown check on all three changed Markdown files:

```sh
npx --cache /tmp/af-validation-markdown-npm --offline --yes markdownlint-cli2@0.22.1 CONTEXT.md docs/design/continuous-performance.md docs/performance-local-executor-verification.md
```

**PASS:** markdownlint-cli2 0.22.1 / markdownlint 0.40.0, 3 files, 0 errors, exit 0.
The sandbox's default cache initially returned `ENOTCACHED`; a fetch attempt returned
`EAI_AGAIN`. The passing offline command uses a temporary writable copy of the public native
npm dependency cache. No formatter rule or exclusion changed. All four changed paths return
false from the unchanged `kernelguard.RULES['protected']` predicate, so no protected inventory
regeneration is needed; the generator and recorded inventory remain unchanged.

The preceding native Task was **not accepted** (`gate_failed/check_failed`): source Snapshot
`4016f950d0633359226016d7450feeb3e20cf45c52d3d462723c7d92e4a7150b`, check receipt
`a854482424e7f9fb5d5dad6b860ecfe5186e0c254b13feccb5bc088181e708dc`. No reviewer ran because
checks failed. The enclosing pipeline must run the unchanged full kernel/Markdown gates,
independent Terra bug review, Astra correctness review and Astra evaluation through
`codex-personal` on the **complete main-to-final feature**, using explicit main base
`8c0ca71a1df2b73ed7c1d0ba9686a6ec1ddf47b1`. These remain acceptance requirements, not passes
claimed by this Worker. No full suite ran here. The coordinator owns actual Docker CI, final
publication and authentic VM execution after native acceptance; deployment, calibration
qualification and speedup remain unclaimed. Follow-ups #378/#382/#380 remain separate.

## Four native finding repairs (2026-10-27)

This continuation starts from reviewed candidate `28cc14e32215fe6773633e4880da5f17d4c72cd3`
(PR398), with findings from `performance-lab-local-executor-verify-20261027`. Coverage maps to
all recorded IDs:

- `correctness-1` and the evaluator's first issue: real diagnostic orchestration stops on
  unconfirmed build-container removal, marks owning/borrowed cleanup uncertain and retains
  the fence plus child plan and named recovery detail. Tests drive the actual build-image
  removal/confirmation protocol through a stand-in Docker client, standalone `run_profile`,
  and executor `cycle` → `execute_local` → borrowed `run_profile`. No later tool/capture or
  scratch cleanup launches; subsequent acquisition refuses. Confirmed-clean build failures
  still produce admitted omission reports and allow safe subsequent acquisition.
- `correctness-2` and the evaluator's second issue: configuration v2 freezes a validated
  image-local `zigbuild_bin`, included in environment identity and passed to both Builder
  sites. Nightly/release plans reach the actual parent and diagnostic `Builder.check_host`,
  `ContainerToolchain` and `ContainerZigbuild` readers with the configured path and unchanged
  Rust/Zig/cargo-zigbuild pins. Image responses and compilation are synthetic; the installed
  image admission and authentic builds are not proven. Missing/invalid shipping settings
  refuse through the actual service command before journal reservation or Builder creation.
- `bugs-1` / duplicated `correctness-3`: the actual service command drains a manual original
  from `max_queue=1`, discovers the new main tip, executes it on the next invocation and never
  claims either twice. Busy/full admission waits without swallowing authority refusals.
- `bugs-2`: frozen member windows are checked after machine contention/capacity scanning and
  again after source preparation, before a builder can launch. Tests cross midnight at both
  boundaries, preserve terminal expiration across controllers/idempotent resubmission and
  prove subsequent queue progress. Native readiness retains the missed power original as
  `invalid` / `off_schedule`, with request provenance, no qualification and denominator ten.
  Existing in-window cohort execution, seal and statistical assertions still pass.

Changed persisted shapes take configuration/state/result v2; obsolete v1 schemas/readers are
removed and old receipts refuse. Request/cohort/readiness shapes and hosted declarations stay
unchanged. New shipping/expiry golden fixtures pass schema/reader/writer parity. The operator
guide, ADR-0235, domain design and change note document these repairs.

Focused command, Python 3.14.4:

```sh
python3 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_executor","tests.test_localcalibration","tests.test_contracts","tests.test_hostlock","tests.test_builds.ContainerBuild","tests.test_builds.FakeBuild.test_the_shipping_recipe_builds_through_exactly_its_pinned_cargo_zigbuild","tests.test_diagnostics.Command"],verbosity=1)'
```

**PASS: 62 tests, 64.763 seconds, exit 0.** An earlier version of this set passed 61 tests
in 52.027 seconds before the explicit missed-power readiness regression was added.
Initial targeted development exposed an unsupported release-case selection and an incorrectly
quoted expected error in the new tests; both fixtures were corrected without changing a
product contract, limit or assertion requirement.

After adding directory fsync for recovery evidence and the service configuration-refusal
regression, the directly affected checks were run again:

```sh
python3 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_executor.AuthorityAndShapes.test_service_refuses_missing_or_invalid_shipping_configuration_before_reservation","tests.test_executor.Journal.test_diagnostic_build_removal_uncertainty_keeps_borrowed_executor_fence","tests.test_diagnostics.Command","tests.test_contracts"],verbosity=1)'
python3 -B scripts/performance/schemas.py
python3 -B scripts/changelog-notes.py --check
python3 -I -B /tmp/af-four-findings-inventory.py
```

**PASS:** 17 tests in 2.029 seconds, exit 0; schema parity, change-note validation and inventory
helper each exit 0. Schemas were generated with `python3 -B scripts/performance/schemas.py --write`.
The inventory helper handles the reserved empty `.git` by copying only the public protected
closure/staged data to a temporary root, invoking the unchanged `cli.py harness-digest --root
COPY --write` and `--check` (both exit 0), copying back only generated staged policy/inventory,
and requiring exact path/size/hash parity against the actual sandbox closure. Latest inventory:
**621 files**, `sha256:beacbf7d9140e27f6c4d88b962f87122a6796eddd8b492d0a931bb2a5e3c661f`.
The generator, bootstrap, `.af/`, native gates, hosted workflows and service units are unchanged.

These focused checks establish orchestration and protocol behavior using explicit synthetic
compute/image/mount seams, real Git, journals, locks and retained evidence. They establish no
authentic Docker/Claw execution, deployment, calibration qualification, speedup or independent
acceptance. No `make check`, full `scripts/test-performance.py` or Rust suite ran here. The
unchanged enclosing native/Markdown gates and independent judging own final validation; the
coordinator owns accepted installation and authentic core/telemetry/lock evidence afterward.
The earlier hosted core absolute deadline and all sample/statistical/resource minima remain
unchanged. Separate #378/#382/#380 work remains outside this repair.
