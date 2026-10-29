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
guide, ADR-0240, domain design and change note document these repairs.

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

## Previous exact-main integration (2026-10-27)

This bounded integration preserves accepted candidate
`a81be5d90cbdebc6a0311515c7cfdfe2257c61dd` (tree
`8d70f9f6d6c145a7bcb28724875a52236b5ae572`) and integrates every upstream change from
`8c0ca71a1df2b73ed7c1d0ba9686a6ec1ddf47b1` to exact main
`2dce351a4b332f0e3cbbaf78aa586de2770637bb`. The latter was the whole-feature review base
for that continuation. Earlier sections retain historical verification and review bases.

Before reading the declared public bundle, `sha256sum` and `wc -c` confirmed SHA-256
`3750e9c8afc6e733acc417288b82f3024821c7a48a11a3570b2711b6ad44bb3d` and 66870 bytes.
A disposable object database received only source Git objects reachable from the two declared
commits. `git bundle verify` confirmed the exact upstream ref and old-base prerequisite;
`git merge-base` returned that prerequisite. All 2750 accepted source files matched their
Git blob identities and executable modes before integration. No external source worktree was mutated.

`git merge-tree --write-tree a81be5d90cbdebc6a0311515c7cfdfe2257c61dd 2dce351a4b332f0e3cbbaf78aa586de2770637bb`
reported exactly the four declared content conflicts. Retain upstream ADRs 0235–0238 and move
only the executor ADR, title and references to its then-unused number. The schema fixture index has no shipped
whole-index generator: its independently added expectation records were mechanically combined
from the two exact parents, requiring equality for every shared record and uniqueness by path.
All **399 entries** and every incoming telemetry refusal fixture remain; unchanged parity tests
check complete fixture coverage, readers, writers, closed schemas and obsolete-version refusal.
The existing schema generator regenerated only executor descriptions for the ADR renumbering.

All **45 upstream changed paths outside the four conflicts and four automatic text merges**
match exact main byte-for-byte and in executable mode, with the obsolete telemetry v1 schema
absent. The four automatic merges (contracts, telemetry controller, cleanup tests and design)
match Git's result exactly, apart from the executor ADR reference in the design. Upstream
release proof, scoped builds, uniform bootstrap and telemetry result v2 coexist with executor
configuration/state/result v2. No executable integration repair was necessary. The schema and
protected inventory generators/bootstrap match both parents exactly; schema parity tests match
the accepted feature exactly. The four accepted repairs remain covered, and the accepted minor
musl reproduction-command omission remains a follow-up.

Focused commands, Python 3.14.4:

```sh
python3 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_executor","tests.test_localcalibration","tests.test_contracts","tests.test_hostlock","tests.test_builds.ContainerBuild","tests.test_builds.FakeBuild.test_the_shipping_recipe_builds_through_exactly_its_pinned_cargo_zigbuild","tests.test_diagnostics.Command","tests.test_telemetry_measurements","tests.test_telemetry_lab","tests.test_telemetry_lock_handoff","tests.test_telemetry_build_bounds","tests.test_telemetry_cleanup","tests.test_telemetry_terminal"],verbosity=1)'
python3 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_kernelguard.Bootstrap","tests.test_kernelguard.Staged","tests.test_kernelguard.Receipts.test_the_committed_fixtures_are_their_recipes"],verbosity=1)'
python3 -I -B scripts/test-test-classes.py Selection Release ReleaseProof -v
python3 -I -B scripts/test-test-classes.py Catalog SealedEvidence Metrics Gates -v
python3 -I -B scripts/test-release-resolve.py -v
python3 -I -B /tmp/af-integration-refusal.py
```

**PASS, all exit 0:** respectively **163 tests / 170.696 s**, **10 / 1.428 s**,
**12 / 0.088 s**, **22 / 25.029 s**, **22 / 4.564 s**, **1 / 1.840 s**: 230 focused tests.
The temporary refusal harness reused the accepted end-to-end local telemetry orchestration
fixture with its real journal, native controller, verification, history and machine lease.
Only its compute stand-in changed: candidate overhead supplied `drain.pairs[0].off_ms = -1`.
It required executor completion with measurement status `partial`, separate
`af.perf-executor-result/2` and `af.perf-telemetry-result/2`, exactly one retained
`refused_document` with that field and reason `negative`, successful original/offline/history
verification, and subsequent common-lock acquisition. The original unchanged success fixture
also passed in the 163-test command. No Actions transport or authentic compute was used.
An initial class-check command named nonexistent class `Policy`; its 22 real tests passed,
and the corrected selections above passed. A temporary harness indentation typo was corrected
before its passing run; neither issue changed product source or assertions.

Generator and metadata commands, each **PASS, exit 0**:

```sh
python3 -B scripts/performance/schemas.py --write
python3 -B scripts/performance/schemas.py
python3 -I -B /tmp/af-integration-inventory.py
python3 -B scripts/test-classes.py audit
python3 -B scripts/changelog-notes.py --check
```

The inventory helper handled the sandbox's reserved empty `.git` by enumerating actual source
with the unchanged bootstrap classification and copying the protected closure to a temporary
root. It invoked unchanged `cli.py harness-digest --root COPY --write` and `--check`, copied
back only generated staged policy/inventory, and required exact digest/path/size/hash equality
between the actual sandbox closure, generated copy and literal bootstrap. The merged inventory
has **632 protected files**,
`sha256:0798e4a4794ac2904802054f5b4e296a9976f83473f9a3f11f45659c63500c1e`.
No generator, `.af/` authority, deadline, budget, original, denominator or statistical rule was
changed. Temporary merge objects/helpers stayed outside the source; no bundle was copied in.

The full unchanged native kernel/Markdown gates and independent Terra/Astra judging remain
with the enclosing pipeline; this Worker ran no `make check`, full test-performance or Rust
suite. Coordinator-owned installation, real VM core/telemetry runs, cross-user lock proof,
report export and backend activation remain unclaimed. Earlier hosted deadline failures and
cleanup fences remain evidence. No external writes, deployment or publication occurred.

Final source checks with `python3 -I -B /tmp/af-integration-final.py` passed, exit 0:
Git whitespace checks against both parents, exact preservation of accepted executor behavior
and regression tests, absence of obsolete v1 shapes, and complete executor ADR renumbering.
The source contains no temporary integration helper, merge object database or input bundle.

## Latest exact-main integration and fixture proof (2026-10-27)

This continuation starts from sealed source `ebeb6fffb6d1e89d584a7ec5b84a8bad2fc5ac0e`
and integrates **all** changes from previous main
`2dce351a4b332f0e3cbbaf78aa586de2770637bb` to exact main
`bdf17c2b239dba966be5c2fd272dab443e3772e0`, the current whole-feature review base.
The preceding integration's focused results did not establish independent acceptance or a full
native pass: native validation stopped at the upstream copied-fixture permission failure.
This continuation uses PR405's already merged repair unchanged.

Before use, these commands confirmed the declared input's **24,170 bytes**, SHA-256
`fa987cd1f921194a5b01392906e2971624ade09528897354d160aa6e4554abee`, exact main ref
`refs/heads/upstream-main-vm-final-20261027`, and previous-main prerequisite:

```sh
stat -c '%s %n' /var/lib/af-task-performance/ga/task-inputs/upstream-main-bdf17c2b239d.bundle
sha256sum /var/lib/af-task-performance/ga/task-inputs/upstream-main-bdf17c2b239d.bundle
git --git-dir=/tmp/af-main-integration-TMRIgJ/objects.git bundle verify /var/lib/af-task-performance/ga/task-inputs/upstream-main-bdf17c2b239d.bundle
git --git-dir=/tmp/af-main-integration-TMRIgJ/objects.git bundle list-heads /var/lib/af-task-performance/ga/task-inputs/upstream-main-bdf17c2b239d.bundle
git --git-dir=/tmp/af-main-integration-TMRIgJ/objects.git merge-base ebeb6fffb6d1e89d584a7ec5b84a8bad2fc5ac0e bdf17c2b239dba966be5c2fd272dab443e3772e0
```

The disposable Git database read only the exact declared public source objects; no external
worktree was changed. Before integration, all **2,773** source files matched the sealed commit's
blob identities and executable modes. `merge-tree --write-tree` on the two exact tips found
only the ADR-index content conflict. All **19 nonconflicting incoming paths**, counting both
sides of schema renames, match main exactly in bytes and executable mode; deleted v1 schemas
are absent. The combined index is exactly main's index followed by the executor entry.
Upstream ADR-0239 and every earlier decision are preserved; only the executor ADR moves to
0240. Module docstrings and generated descriptions follow that renumbering.

The incoming fast defaults, `release_pr` provenance, selection/binding/receipt v2 and obsolete
version refusal remain upstream's exact implementation and assertions. Ordinary main, local
full validation and unbound releases remain full. The fixture repair grants owner-write only
to regular, nonsymlink files in its disposable fixture before overwriting them. It does not
change source permissions, production sealing or fixture cleanup ownership.

Focused commands, **non-root uid 994**, Python **3.14.4**; all exited **0**:

```sh
python3 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_executor","tests.test_localcalibration","tests.test_contracts","tests.test_hostlock","tests.test_builds.ContainerBuild","tests.test_builds.FakeBuild.test_the_shipping_recipe_builds_through_exactly_its_pinned_cargo_zigbuild","tests.test_diagnostics.Command","tests.test_telemetry_measurements","tests.test_telemetry_lab","tests.test_telemetry_lock_handoff","tests.test_telemetry_build_bounds","tests.test_telemetry_cleanup","tests.test_telemetry_terminal"],verbosity=1)'
python3 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_kernelguard.Bootstrap","tests.test_kernelguard.Staged","tests.test_kernelguard.Receipts.test_the_committed_fixtures_are_their_recipes"],verbosity=1)'
python3 -I -B scripts/test-release-resolve.py -v
python3 -I -B scripts/test-test-classes.py SealedEvidence.test_selected_build_receipts_cover_all_variants_before_any_suite -v
python3 -I -B scripts/test-test-classes.py -v
python3 -I -B /tmp/af-main-integration-TMRIgJ/sealed-check.py
```

**PASS:** executor/schema/telemetry **163 tests / 149.643 s**; protected inventory
**10 / 1.406 s**; release resolution **25 / 5.613 s**; ordinary fixture method
**1 / 13.257 s**; complete ordinary class suite **45 / 20.588 s**. The owned read-only
source proof reran that same unchanged method (**1 / 12.833 s**) and complete class suite
(**45 / 20.725 s**): **290 test executions** in total, with no skipped assertions or scenarios.

The temporary seal helper copied public source into its own temporary directory, required
non-root ownership, removed every write bit from copied files and directories while preserving
execution bits, and proved an attempted source overwrite raises `PermissionError`. It ran the
two commands above against that copy with isolated Python and bytecode disabled. After each
run it compared SHA-256 bytes and full permission modes for all **3,380** copied file/directory
entries, and separately checked the sandbox source: unchanged. Only after comparison did it
restore permissions on its own copy for removal. The copy was removed; original source modes
were never changed. These tests run the native wrapper and Make/class receipt graph with the
existing bounded Cargo/suite stand-ins, not the full native gate.

Metadata and faithful-integration commands, each **PASS, exit 0**:

```sh
python3 -B scripts/performance/schemas.py --write
python3 -B scripts/performance/schemas.py
python3 -I -B /tmp/af-main-integration-TMRIgJ/inventory.py
python3 -B scripts/test-classes.py audit
python3 -B scripts/changelog-notes.py --check
python3 -I -B /tmp/af-main-integration-TMRIgJ/final.py
```

The unchanged schema generator regenerated eight executor schema descriptions. The inventory
helper copied the public protected closure to an owned temporary root without the sandbox's
reserved empty `.git`, ran unchanged `cli.py harness-digest --root COPY --write` and `--check`,
then copied back only generated staged policy/inventory. Exact digest/path/size/hash parity
holds between the actual sandbox closure, generated copy and literal bootstrap: **632 files**,
`sha256:014ac3f955a72a5cbec2dc086d72eede4b550c7ee25fef1410e3dfcd53aaa44a`.
Both generators and the bootstrap match the source and main exactly. The schema fixture index
retains all **399** entries unchanged. `.af/` authority was not edited.

The final audit checks exact incoming preservation, combined index, executor ADR renumbering,
whitespace against both tips, and the complete permitted change set. Executor implementation
and regression tests are unchanged apart from ADR text in three module headers/contracts.
All four accepted fixes remain: diagnostic cleanup uncertainty keeps exclusion; frozen Zig
configuration reaches both builders; full queues drain; expired calibration originals never
launch and retain their identities and denominator. Telemetry result v2 and executor
configuration/state/result v2, common lock, exact sources/containers, budgets, deadlines,
statistical minima and original records remain intact. The accepted missing `--zigbuild-bin`
in diagnostic reproduction commands is still a follow-up; actual execution retains the path.

No `make check`, `scripts/test-performance.py`, full Rust suite or full Markdown gate ran in
this Worker. The enclosing unchanged native kernel/Markdown gates and independent judging
must still accept this exact candidate. Installation, publication, real VM core/telemetry runs,
live cross-user common-lock proof, backend activation and report export remain coordinator-owned
and unclaimed. No original was retried, cleanup fence cleared or measurement limit relaxed.
Temporary merge objects, helpers, ignored test bytecode and owned copies were removed; no input
bundle or helper was added to the sealed source. The declared external input was left untouched.

## Integration with main `7932210` and the fast Task gate (2026-10-28)

This integration applies the accepted feature `d8fc7e37361cda5704cb9a107b28e62b9fe8d32b` to
exact main `7932210329697f5aa374e9a4b432899fc6170d9a` (PR419: fast Task gate, manual kernel
measurement, targeted Worker tests, class build receipts, ADR-0251 and ADR-0252). Main is the
first parent and d8fc the second. The declared input hashes were checked before use: the
**487,538-byte** patch `git diff bdf17c2b239d..d8fc7e37361c`
(`db4fafcad008969d863a249ae784d8b5e7e596d49077208dc68266cdfffd58eb`) and the old-base and
feature versions of the three conflicting files.

A disposable Git copy of the sandbox, outside the source, held the old-base blobs of the three
files, then ran `git apply --3way --index` with the patch. Exactly three paths conflicted:
`docs/adr/README.md`, `fixtures/performance/kernel-measure/code-policy-measures.toml` and
`fixtures/performance/kernel-measure/inventory.json`. The other **71** patched paths had main's
bytes equal to the patch preimage, and their results equal the d8fc postimage blob IDs. The
46 new files are mode 100644 and nothing is deleted.

- The ADR index lists 0239, then 0240 (executor), then main's 0251 and 0252, in numeric order.
- The staged policy and inventory were regenerated with the unchanged
  `python3 -I scripts/performance/cli.py harness-digest --write`, then `--check`. The result is
  `sha256:09a58c96ec450e5411f7480739a0a2faacee692405a3764265c1166b1d882f91` for **632 files**,
  the same in the sandbox and in the Git copy. A three-way comparison of the inventory entries
  shows 54 from the feature, 1 from main (`scripts/performance/tests/test_kernelguard.py`) and
  no mismatch. The policy differs from main's only in its digest comment and its four digest
  arguments. Its measures, repetitions, wall and manual/full authority are main's.
- `.af/code-policy.toml`, `.af/`, `.config/`, `.github/`, the Makefile and Cargo files are
  exactly main. No guarded measure is installed in `.af/`.
- Every file under `scripts/performance/` is in the protected inventory. Of those, main changed
  only `tests/test_kernelguard.py` since the feature base. The merged laboratory is therefore d8fc's
  laboratory plus that test.

Focused commands on macOS (Darwin, Python 3.15), from the sandbox root:

```sh
python3 -I scripts/performance/cli.py harness-digest --check
python3 -I -B scripts/performance/schemas.py
python3 -I scripts/test-classes.py audit
python3 -I -B scripts/changelog-notes.py --check
python3 -I scripts/test-gate-owner.py
cargo nextest run --workspace --locked --profile ci -E 'binary_id(af::it) & test(/^perf_kernel_measure::/)'
cargo nextest run --workspace --locked --profile ci --run-ignored only -E 'test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)'
```

Each of these exited 0. `test-gate-owner.py` ran 14 tests, 10 of
them skipped as Linux-only. The `perf_kernel_measure` filter ran **8 passed**, including main's
build-free `guarded_kernel_experiments_refuse_every_tampered_harness` (85.9 s), and
`guarded_consumer_keeps_the_complete_instrument_and_crates_without_repository_growth` over the
merged inventory. The manual measurement, which ADR-0252 requires before a pull request changing
`scripts/performance/`, **1 passed** in 1,083.8 s, with its judged comparison and
tampered-candidate refusals after a measured baseline. On this host `PATH` resolved Homebrew
cargo 1.94 for those two nextest runs. The pinned 1.88.0 rerun at the end is recorded below.

The performance-lab modules ran through a scratch `unittest` loader that adds
`scripts/performance` to `sys.path`, the same way `scripts/test-performance.py` does. Results:

- `test_contracts`, `test_suite`, `test_builds`, `test_diagnostics`, `test_runner`,
  `test_kernelguard`, `test_localcalibration`, `test_telemetry_build_smoke`,
  `test_telemetry_cleanup` and `test_telemetry_terminal`: **294 tests OK**, 1 skipped.
- `test_hostlock`, `test_telemetry_measurements`, `test_telemetry_lab`,
  `test_telemetry_lock_handoff` and `test_telemetry_build_bounds`: **60 tests OK**.
- `test_executor`: **27 of 31 OK**. These four fail on this macOS host:
  - `Journal.test_local_diagnostic_uses_native_child_planner_report_and_admission`
  - `Journal.test_shipping_nightly_release_reach_both_builders_with_frozen_pinned_tools`, in
    both its `nightly` and `release` profile subtests
  - `Journal.test_diagnostic_build_removal_uncertainty_keeps_borrowed_executor_fence`

  Each stops in `diagnostics.check_mode` with `host_mismatch`, "environment
  linux_x86_64_…_claw is the contained Linux profile; this host is macos". The fixtures leave
  `host_os` unset, so they assume a Linux host. The test and laboratory files are byte-identical
  to d8fc, so the failures are not merge-introduced. This note records them as a follow-up and
  does not repair them. They passed on the accepted Linux run above, and the perf class is not
  part of the fast Task gate (ADR-0251).

`cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` passed on
the pinned 1.88.0 toolchain, with rustup's `~/.cargo/bin` first on `PATH`. Homebrew's clippy
0.1.94 instead reports `manual_is_multiple_of` in unchanged main crates. markdownlint-cli2 0.22.1
passed on every changed Markdown file. After the note was written, the build-free
`perf_kernel_measure` filter above and `harness-digest --check` ran again on the pinned
toolchain: **8 passed**, same digest.
No `make check`, `scripts/test-performance.py`, full Rust suite or remote gate ran here. The
enclosing native kernel/Markdown gates, the required reviewers and the evaluator must still
accept this exact seal. Publication, merge, installation on Claw, original core/telemetry
benchmarks and live common-lock exclusion remain coordinator-owned and are not claimed. The
missing `--zigbuild-bin` in generated diagnostic reproduction commands remains a follow-up.
The scratch Git copy, patch copy and loader stayed under the temporary directory, outside the
source.

## Scratch enforcement sync batching (2026-10-28)

Recorded in the AF implementation sandbox on macOS (Darwin, Python 3.15.0). This section records
source checks only. It is not a VM verification or measurement.

**The blocker.** The Claw telemetry original `exp_15f8e705d9ae31b774e8f15fd7ca67c0ec3284b52671dc16`
refused before any candidate build. Its error was "enforcement not verified: the fill did not
finish within its time bound (1458569216 bytes written)". `telemetrybuild.run` checks its 8-GiB
build scratch with a 120 s deadline. `capability.fill` keeps 30 s of that for cleanup, which
leaves 90 s to write. The fill synced its file after every 1-MiB write, so it paid one durable
flush per MiB. That original stays terminal and uncertain, and its artifacts stay as they are.
This change does not authorize replaying it.

**The change** (`scripts/performance/perflab/capability.py`). The fill still writes one reused
1-MiB buffer at a time and counts every byte a write accepted, partial writes included. It now
syncs after every `FILL_SYNC_BYTES` (64 MiB) written. After the filesystem refuses a write with
ENOSPC or EDQUOT, it syncs once more before it reads `statvfs`. A refused write alone does not
show that the data before it was committed. That last sync's own error is the fill's stop: a
delayed ENOSPC or EDQUOT stays a capacity refusal at `fsync`, and EIO or any other errno refuses
as an error. A sync answered after the fill's bound is the deadline's, and the refused write's
errno is cleared. Unchanged:

- `fill_refusal`: progress, the byte limit, under two chunks free, and a clean removal.
- `af.perf-capabilities/2` and its reader.
- `FILL_SECONDS_MAX` (600 s), `FILL_CLEANUP_SECONDS` (30 s) and the 120 s telemetry cap.
- The 8-GiB build ceiling, every run, sample and container limit, and the machine lock and
  fence.
- The abandoned worker's close-only rule.

The guarded harness inventory was re-pinned with the unchanged generator.

**Focused commands**, from the sandbox root:

```sh
python3 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_supervise","tests.test_contracts","tests.test_kernelguard","tests.test_runner.Preflight","tests.test_runner.HostCancellation","tests.test_continuous.HostedScratch","tests.test_telemetry_build_bounds","tests.test_telemetry_cleanup"],verbosity=1)'
python3 -I scripts/performance/cli.py harness-digest --write
python3 -I scripts/performance/cli.py harness-digest --check
python3 -I -B scripts/performance/schemas.py
python3 -I scripts/test-classes.py audit
python3 -I -B scripts/changelog-notes.py --check
cargo nextest run --workspace --locked --profile ci -E 'binary_id(af::it) & test(/^perf_kernel_measure::/)'
```

**Results.**

- **Capability tests.** The 17 `test_supervise.Capabilities` tests pass. On the base
  `capability.py`, 9 of them fail: every new test and the two updated ones.
- **The regression.**
  `test_one_sync_per_mib_misses_an_8_gib_check_that_batched_syncs_verify` models the Claw
  scratch without storing anything. A sync costs 60.7 ms plus its unsynced bytes at 250 MB/s,
  and the filesystem holds 160 MiB less than its 8 GiB.
  - The old per-MiB sync stops at the fill's time bound. Run unpatched on the base, it wrote
    1,454,374,912 bytes, within 0.3% of the observed figure.
  - The new fill reaches the virtual capacity by ENOSPC. It verifies the bound after
    41 virtual seconds, inside the 90 s it has.
- **Adversarial tests.** Further tests cover:
  - a final-sync ENOSPC or EDQUOT, which verifies, and an EIO, which refuses;
  - delayed refusals at a periodic sync;
  - a partial last write;
  - the byte limit, with one periodic sync before it;
  - a late periodic sync and a late final sync;
  - a final sync blocked at the deadline, after which the worker only closes its descriptor.
- **Inventory and schema checks.** `harness-digest --check`, `schemas.py`, `test-classes.py
  audit` and `changelog-notes.py --check` exited 0. The new digest is
  `sha256:45c9f615e81fc95661e362bb39ee0028c44d283876b97970053e385410b13a85`.
- **Rust checks.** No Rust source changed. `cargo fmt --all --check` and `cargo clippy
  --workspace --all-targets -- -D warnings` passed on the pinned 1.88.0. Homebrew's cargo
  1.94 was first on `PATH`, and its clippy fails in the unchanged `review-process` crate.
- **Kernel guard.** The `perf_kernel_measure` filter: **8 passed**, including
  `guarded_kernel_experiments_refuse_every_tampered_harness` (95.3 s) over the re-pinned
  inventory.
- **One failure that is not this change.**
  `ContainedSample.test_a_late_confirmation_in_real_time_is_unconfirmed` failed here. It
  failed alone and twice on a scratch copy of the base. It tests `supervise.remove_container`
  with a 0.2 s real-time allowance at a load of about 14, and this change does not touch it.
- **A flake.** `HostCancellation.test_a_seed_writer_that_never_confirms_its_end_keeps_the_seed_store`
  failed once in the combined run, as `run_deadline`, and passed alone.

**Not claimed.** The model's rates are illustrative. If Claw's real flush bandwidth is far
below 100 MB/s, no sync schedule fills 8 GiB in 90 s. Still owned by the coordinator after
acceptance:

- an unchanged-budget 8-GiB fill on Linux;
- a new telemetry original on Claw, clearly distinguished from the first.

No VM, SSH, benchmark, `make check` or whole suite ran here.

**Integration with a newer main.** The accepted change was later applied to a newer main.
`capability.py` and `test_supervise.py` match the accepted bytes exactly. The two kernel-measure
fixtures were the only conflicts. They were regenerated with that main's unchanged
`harness-digest --write`, and `--check` exited 0. The digest above belongs to the accepted
source only. The integrated tree's digest is
`sha256:9363f32dc9397b388178adc1b423a9bc676c2a9a77db366c52644bbcdda54f9b`. The 17
`test_supervise.Capabilities` tests, `test_kernelguard`, `test_contracts`, `schemas.py`,
`test-classes.py audit` and `changelog-notes.py --check` passed there.

## Two Mac-gate test helper repairs (2026-10-28)

The integrated candidate's native gate failed in three tests outside the scratch change. This
section repairs only those test helpers, in two files. No product code, limit, assertion or
test selection changed.

- **Read-only copied fixtures.** `onboard::afactory_authority_plans_a_three_field_task_with_its_defaults`
  copies `.af/` with `cp -R`, which keeps the gate's read-only modes. `migrate_check_receipt`
  in `crates/af/tests/it/support/task_cli.rs` then failed to rewrite the copy, at line 114.
  It now sets the owner-write bit on each copied file it rewrites, catalogs included.
- **A `next:` line cut inside a word.** Under a long temporary directory, `af task list
  --attention` cuts the `--state` path into pieces. Each piece ends in `\` and the next starts
  at column 0. `attention_group` in `crates/af/tests/it/task_outcomes.rs` joined every line
  with a space and dropped those column-0 lines, so it failed at line 279. It now reads the
  lines as a shell does: after a `\`, an indented line adds a space and a column-0 line
  continues the same word.

**Reference.** The repair is public PR 440's diff
`fe40b34de4176e244032044fd0578e7ab3a49041..57860818612655a90b9e997de6371529c8d9e2ff`
(3390 bytes, SHA-256
`7b279b35054dc05c60b5775c7376dd9730419830d869304118c0e3144d9c313f`). It applied cleanly and
unchanged. Its join rule matches `human::fit_words`: a space and `\` between arguments, then
an indented line; a bare `\` after a cut piece, then the next piece at column 0.

**Focused results.** Each run used a copy of the tree with every file read-only, passed as
`AF_WORKSPACE_ROOT`. Each also used a long `TMPDIR`.

- **Without the repair**, at `TMPDIR` lengths 104 and 144, all three tests failed at the
  gate's lines: `task_cli.rs:114` and `task_outcomes.rs:279`.
- **With the repair**, the complete selection, the `task_outcomes::` module and the onboard
  test (17 tests), passed at length 104 and with `TMPDIR` unset. It was not complete at 144
  or 224: one of its tests failed there, as the next bullet records.
- **With the repair**, the three targeted regressions, the three tests that failed without
  it under the read-only copy and long `TMPDIR` above, passed at lengths 104, 144 and 224.
- **Not repaired.** At lengths 144 and 224, the separate existing test
  `task_outcomes::a_next_line_fits_the_terminal_and_runs_as_printed` failed because the printed
  command is wider than the test's 250-column terminal. It passes at 104 and did not fail in
  the gate. Its assertions were kept unchanged; it is out of this repair's scope.

## Host CPU observer and the durable release comparison (2026-10-30)

This change adds the trusted host CPU observer (`perflab/hostobserver.py`), the partitioned
serial core comparison and the native comparison lifecycle (`perflab/comparison.py`). Its
focused checks ran on a macOS development host, with injected computation:

- **Host observer** (`tests/test_hostobserver.py`, 9 tests). Real processes: a gated shell, a
  stand-in perf that speaks perf's acknowledged control protocol and ends with its target, and
  the observer's bounded runner. A fixture process table stands in for `/proc`. The tests show
  perf's acknowledgement before the command's first instruction, a region probe attached
  disabled, a recycled PID refused with the gate kept closed, a target outside the owned
  container or account refused, the capability probe decoding samples and refusing a perf that
  cannot record, and trusted tools routed to the host while perf and the host shell are never
  bound into the candidate container.
- **Comparison** (`tests/test_comparison.py`, 4 tests). The real journal, Git retention,
  claims, offline verification, aggregation, rendering and export, with synthetic builds and
  shards. Checked: the serial schedule, retained refusals that reserve nothing, launch and
  resume without a replay, read-only status while the journal is locked, build reuse by the
  second partition, the aggregate over the whole family's denominator, lanes selected by the
  aggregate verdict, a portable export verified after copying (also through `cli.py executor
  comparison-verify`), and a lost controller with its failed reuse leaving the family
  incomplete. The synthetic stand-in writes no seed copies, so its real aggregate is
  `incomplete`; the `complete` report is shown with that aggregate injected as whole.
- **Unchanged contracts.** The contract parity, executor, diagnostics, report-diagnostics,
  supervision, reproduction and whole-source tests pass. Three executor tests (four cases)
  need a Linux host for the contained mode (`host_mismatch` on macOS). With `host_os` patched
  to Linux, all three pass. One of them needed a repair: it asserted that no tool is probed at
  all, while the executor's existing pre-build tool check probes once before any build. It now
  asserts that every tool probe precedes the first build.

Not established here: Docker, a real perf, Claw, the v0.13.0 against v0.14.0-rc.1 comparison or
its export. The outer operator runs that from the installed candidate control.

## Claw validation repairs: admission, CLI contracts, partitions, export, observer (2026-10-30)

This change repairs the concrete defects the operator's Claw runs of the previous candidate
found. Its focused checks ran on a macOS development host with injected computation; nothing
here ran Docker, a real perf, Claw or GitHub Actions, and no figure below is a measurement.

- **One storage and time admission** (`tests/test_executor.py`,
  `test_preflight_submission_and_execution_share_time_and_storage_admission`). Preflight,
  submission and the claim use `executor.native_reserve` and `executor.time_bounds`: with
  `run_seconds = 3600` the `manual` telemetry profile is refused at preflight naming 24,870
  seconds and each step, nothing reserved; a 64 GiB `max_bytes` below the native reserve is
  refused with the bytes held, the reserve and the bound, and 100 GiB is ready. An executing
  telemetry original with too little time left ends `failed`, `not_executed`, before either
  build, with no fence kept.
- **Per-release CLI contracts and whole output** (`tests/test_runner.py`, `CliOracle`
  `test_each_release_declares_its_own_report_schema_and_whole_output`). 0.13.0 selects
  `af_cli_v2` (`af/review-report@4`), 0.14.0-rc.1 and later `af_cli_v3` (`@5`); each refuses the
  other's schema, malformed JSON, a truncated output, a missing Finding, a duplicated Finding
  and empty output. A 234,442-byte 100-Finding report is retained whole within the unchanged
  262,144-byte total (245,760 bytes of stdout, a 16 KiB stderr reserve).
- **Partition execution authority** (`tests/test_runner.py`, `PartitionExecution`). Through the
  real runner deadline path: a later partition whose reused build's run expired a run total
  ago completes in its own sealed window, the caller's deadline still tightens it, its build
  bytes stay unchanged, and its shard in a bundle without the authority is a `shard_identity`
  failure. A widened or re-totalled window, a forged build digest, a changed build run, a
  window before the build ended, an undeclared shard and an authority changed after sealing
  each start nothing (`partition_authority`). `tests/test_comparison.py` shows the aggregate
  carrying the second partition's authority.
- **Portable export closure** (`tests/test_comparison.py`). The telemetry original is now a
  real `telemetrylab.run` result: its page links `raw/` and `logs/`, and the export carries
  them with its manifest; each core partition carries its run bundle and its report's
  evidence; the aggregate carries its run. The copied bundle verifies (also through `cli.py
  executor comparison-verify`), re-evaluating every core run to its bundled verdict and the
  telemetry result under the installed authority. A self-consistently rehashed copy missing a
  linked raw document, one with a changed raw document, and one without a partition's run
  bundle are each refused.
- **Host observer lifetime and callback failures** (`tests/test_hostobserver.py`). Through the
  real `toolrun.start` request admission with 1,404 seconds of diagnostic window left, the
  whole window is refused (`at most 120 s`) and the per-command lifetime (10 s + 40 s of
  allowances) is admitted and records. `Profile.tool_run` with a failing observer callback: a
  confirmed cancellation carries its settled Result and keeps nothing; an unconfirmed one and an
  opaque supervisor failure are `cleanup_uncertain` with the observer's reason first and the
  scratch kept; a cleanup failure after a failed repetition keeps the primary cause.
- **Telemetry documentation.** `docs/telemetry.md` names the
  `harness_last_write_to_command_exit` boundary for the invocation-cost drain, marks the
  CAS-write drain figures historical and separates a negative physical duration (not judged)
  from a negative signed difference (judged); the rendered telemetry page says the same.

The whole laboratory suite (`python3 -I scripts/test-performance.py`, 1,162 tests) ran once on
this host at load: the three executor tests (four cases) that need a Linux host for the
contained mode failed with `host_mismatch` as before; the contract parity test failed until the
new contract's golden examples were added; and two tests outside the changed paths
(`test_telemetry_workflow` admission, whose subprocess read the rendered page this change was
editing while the suite ran, and the `test_whole_source_repairs` 2.5-second deadline) failed
under load and passed alone. `make perf-kernel-measure`, which this protected-harness change
requires, is recorded in the Task's report with its result.

Not established here: Docker, a real perf, Claw, the v0.13.0 against v0.14.0-rc.1 comparison or
its export. The outer operator runs that from the installed candidate control.
