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
requires, was not recorded here with its result; the next section records its run on the
following candidate.

Not established here: Docker, a real perf, Claw, the v0.13.0 against v0.14.0-rc.1 comparison or
its export. The outer operator runs that from the installed candidate control.

## Image, storage, CLI log and musl debug-information admission (2026-10-30)

This change repairs four gaps the operator's Claw runs of candidate `fe1d1e59` found, after
integrating main `955e24bf` (the ADR index keeps both ADR-0264 and ADR-0265). Its focused
checks ran on a macOS development host (Python 3.14.7) with injected computation; nothing
here ran Docker, a real perf, Claw or GitHub Actions.

- **Image and image-local tool admission** (`tests/test_executor.py`, `ImageAdmission`). A
  stand-in docker client answers `image inspect` for the images it holds and runs each probe
  container's entrypoint on this host. Preflight and submission refuse, reserving nothing and
  starting only bounded `af-perf-probe-` containers, an absent pinned build image, an absent
  sample image, a `zigbuild_bin` without cargo-zigbuild and a Zig 0.15.1 where the musl recipe
  pins 0.15.2. With both images and the pinned tools present, preflight is `ready` and lists
  `build_image`, `sample_image` and `image_toolchain` (rustc 1.88.0, cargo-zigbuild 0.23.4,
  Zig 0.15.2); the probes' exclusion settles clean. When the build image is then removed, the
  claim's own admission ends the original `failed`, measurement `unavailable`, before any
  build, with no fence kept. A probe container whose removal is unconfirmed keeps the fence.
  `tests/test_comparison.py` shows a missing image refusing the whole comparison before any
  member is submitted.
- **Physical storage** (`tests/test_executor.py`,
  `test_storage_admission_needs_the_policy_bound_and_the_filesystems_free_space`). The root
  filesystem's available bytes are injected at `Journal.free_bytes`: within a 100 GiB
  `max_bytes`, one byte less than the native reserve is refused at preflight, at submission and
  at the claim (which leaves the original queued and builds nothing); exactly the reserve is
  ready. A 1 PiB filesystem never admits a reserve over `max_bytes`. Every other executor,
  comparison and local calibration fixture injects ample space, so no test depends on this
  host's disk.
- **CLI diagnostic log total** (`tests/test_diagnostics.py`,
  `CliCases.test_a_cli_command_keeps_the_cores_whole_sample_log_total`). Through a real
  diagnostic child run, every CLI command's supervisor request is 245,760 bytes of stdout in a
  262,144-byte log (it was 311,296), and every other tool keeps its 64 KiB stderr allowance.
  Through the stand-in supervisor with those caps, 234,442 and 245,760 bytes are retained whole
  and 245,761 bytes fail the run as over its output cap. A total without stderr room or above
  the 1 MiB hard cap is refused.
- **Bounded musl CPU-profile debug information** (`tests/test_builds.py`,
  `test_the_musl_shipping_cpu_profile_builds_bounded_line_tables_with_frame_pointers`). The
  real Builder, with a stand-in musl toolchain and cargo-zigbuild, builds the
  `linux_musl_cpu_profile` child plan through `zigbuild`; every step records
  `-Cdebuginfo=line-tables-only` and `-Cforce-frame-pointers=yes` with
  `CARGO_PROFILE_RELEASE_DEBUG=true`, and the recipe digest and settings bind them. The
  shipping verdict build sets neither. `tests/test_diagnostics.py` `Registry` checks the
  derivation for every recipe. No real musl AF was built here, so its size under this setting
  is not measured; on GNU the same setting gave a 285,890,672-byte AF.

Focused command, from the repository root:

```sh
cd scripts/performance && /opt/homebrew/bin/python3.14 -I -B -c 'import sys,unittest; sys.path.insert(0,"."); unittest.main(module=None,argv=["unittest","tests.test_executor","tests.test_comparison","tests.test_localcalibration","tests.test_builds","tests.test_diagnostics","tests.test_hostobserver","tests.test_telemetry_diagnostics","tests.test_runner.CliOracle","tests.test_contracts","tests.test_supervise"])'
```

**300 tests, 776.630 seconds, exit 1**, run while a Rust build shared the host. Four cases of
three executor tests failed with `host_mismatch`, as in the sections above: they need a Linux
host for the contained mode. With `diagnostics.host_os` patched to Linux they pass (4 tests,
6.738 s). `test_hostobserver` `test_bounded_runner_ends_its_whole_group_at_its_deadline` and
`test_supervise` `test_a_late_confirmation_in_real_time_is_unconfirmed`, real-time assertions
on paths this change does not touch, failed under that load and passed alone (2 tests,
0.742 s). Every other test passed.

Protected harness:

```sh
make perf-kernel-measure
```

**PASS, exit 0:** `perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample`,
1 passed, 776.847 s (778.285 s with its build), after
`python3 -I scripts/performance/cli.py harness-digest --write` re-pinned
`fixtures/performance/kernel-measure/` to the protected harness
`sha256:56432bb59d0a45c708fa0a2be3aa6700c06e2ba34448d169a7d7ccad7738c65a` (667 files; only
the changed protected Python files differ) and `--check` passed. A run before that re-pin
failed in 3.392 s as it should: the staged policy was stale for the changed tree. The `cargo`
on this host's PATH is Homebrew's 1.94.0, not the pinned 1.88.0. `scripts/test-classes.py audit`, `scripts/changelog-notes.py --check` and
`scripts/performance/schemas.py` pass, and `cargo clippy --workspace --all-targets -- -D
warnings` passes with the pinned 1.88.0 (Homebrew's 1.94 reports `manual_is_multiple_of` in
unchanged Rust code).

Not established here: Docker, a real perf, Claw, a musl CPU capture and its flamegraphs, the
v0.13.0 against v0.14.0-rc.1 comparison or its export. The host observer's timeout and cleanup
repairs have not yet reached an actual AF capture on musl. The outer operator reruns the musl
CPU acceptance on Claw from this candidate.

## Final native integration: image-policy refusals and every recording's build IDs

This candidate combines the sealed native repairs (image and image-local tool admission,
physical storage, the CLI diagnostic log total, musl line-tables-only debug information, the
repeated CLI CPU capture, its decoding and its exec-to-exit timing) and settles two defects
those repairs left. Its focused checks ran on a macOS development host (Python 3.14.7) with
injected computation; nothing here ran Docker, a real perf, Claw or GitHub Actions.

- **An image-policy refusal keeps no fence** (`tests/test_executor.py`, `ImageAdmission`).
  `BuildImage.refuse_implicit_volumes` refused a declared writable volume with a plain
  `ContractError`, which escaped `image_admission` and `Journal.exclusive`, so a preflight
  that had run only `docker image inspect` left the machine fence and disabled every later
  experiment. The images' identity and volume checks now refuse as an admission refusal: no
  probe container has started then, so the exclusion settles clean. A volume declaration that
  is not JSON refuses the same way. With the stand-in docker client declaring
  `{"/unexpected-volume":{}}`, `test_an_image_volume_refusal_runs_nothing_and_keeps_no_fence`
  shows preflight `refused`, submission refused with nothing reserved, only `image inspect`
  invoked, and no fence;
  `test_a_claimed_image_volume_refusal_fails_before_any_build_without_a_fence` shows the claim
  ending the original `failed` before any build, with no fence.
  `test_an_unconfirmed_probe_container_keeps_the_fence` still keeps the fence. Both new tests
  fail on the earlier catch, which left the fence.
- **Build IDs from every recording** (`tests/test_report_diagnostics.py`, `RepeatedCaptures`).
  `Profile.analyse` decoded every repetition but listed build IDs only from the first, so a
  short first execution with no AF sample reported `binary_build_id_present: false` although
  later recordings hit AF. Each recording is now listed by its own `perf buildid-list
  --with-hits`, in order, under the tool's own time and output bounds and the diagnostic
  deadline; a listing that fails or is malformed refuses the capture, naming its recording,
  where a failure used to be read as no build IDs. Through the actual Profile writer and report
  admission, a first recording with no hits, libc-only middle recordings and a last recording
  hitting AF give `binary_build_id_present: true`
  (`test_a_later_recordings_hit_of_the_executable_is_found_when_the_first_has_none`). With no
  recording hitting AF it is `false` even though the executable carries that ID
  (`test_no_recordings_hit_of_the_executable_is_never_inferred_from_the_executable`). A failed
  or malformed listing of recording 7 omits the capture as `tool_failed` or
  `malformed_build_ids`, every recording and observer record kept and the report admitted
  (`test_a_failed_or_malformed_build_id_listing_refuses_the_capture_by_its_recording`). The
  first two fail on the first-recording-only listing.

The protected harness was re-pinned from this final tree with
`python3 -I scripts/performance/cli.py harness-digest --write` to
`sha256:fca6c728a31bc1b0976e5558dcfd27b3a2470672af0be53bf8f424f93e7b0cae` (677 files), and
`--check` passes; before that it refused both staged files. The manual harness pass recorded
above used Homebrew's Rust 1.94.0, so it is not proof on the pinned 1.88.0 toolchain. Not
established here: `make perf-kernel-measure` with the pinned Rust 1.88.0 on Claw, a full
release comparison, telemetry, an actual trace or CPU capture of this candidate, or a portable
verified report. The outer operator runs those.

## Whole-controller cancellation and the telemetry build image ID (2026-10-30)

Two integration failures of an actual Claw full comparison (`cmp_1dd3108b…`), whose failed
evidence stays as recorded and is not replayed. The focused checks below ran on a macOS
development host (Python 3.14) with injected computation. Nothing here ran Docker, Claw or
GitHub Actions, and no signal was sent to any process.

- **A controller's signal no longer ends only one shard** (`perflab/runner.py`,
  `perflab/executor.py`). SIGTERM to `comparison-run` during shard 2 was consumed by the
  shard's own handler; `execute_local` went on to shard 3 and `comparison.run` to the queued
  telemetry original. Under a controller's lease, `run_shard` now seals the shard's
  `interrupted` record and then raises `runner.Cancelled`. The executor removes the original's
  scratch (`settle_cancellation`), and the cycle publishes an `interrupted` result with the
  partial evidence and settles the fence only when every owned cleanup was confirmed, then
  re-raises; otherwise the claim is `uncertain` and the fence stays.
  `test_runner.ControllerCancellation` calls the handler the real shard installed: under a
  lease it raises with the sealed record, and alone the shard returns its `interrupted`
  record as before. `test_executor` covers the confirmed outcome, an `interrupted` original
  that is never replayed
  (`test_a_cancelled_shard_settles_confirmed_cleanup_interrupted_and_never_replays`), and an
  unconfirmed cleanup that stays `uncertain` with the fence kept
  (`test_a_cancelled_shard_with_unconfirmed_cleanup_is_uncertain_and_keeps_the_fence`).
  `test_comparison.test_a_controller_cancelled_in_a_shard_runs_no_later_shard_or_original`
  runs a nightly partition of shards 0 and 1 through `comparison.run`. A cancellation in
  shard 0 leaves shard 1, the second partition and the telemetry original unstarted and
  queued, and the next cycle claims only the second partition.
- **A telemetry build in a local image ID** (`perflab/contracts.py`). Build admission takes
  `sha256:<64 hex>` (ADR-0170), but the build record's image allowed only `name@sha256:…`, so
  a comparison's telemetry original compiled both sides and was then refused at
  `validate_telemetry_build`. The record now uses `images.REFERENCE`, the same canonical
  authority as admission, as `af.perf-telemetry-build/2` inside `af.perf-telemetry-result/3`.
  The previous versions are refused. A tag, a tag before a digest, an uppercase name, a short
  or uppercase ID, and a bare hexadecimal ID are refused too.
  `test_local_telemetry_publishes_and_verifies_an_actual_local_image_id` publishes the actual
  ID through the executor and verifies the native result carrying it.
  `test_telemetry_build_records_refuse_previous_versions_and_mutable_references` refuses
  those references through `telemetry-build-record` and a `/1` record through `read_build`.
  The golden fixtures add the local ID and each refusal.

The protected harness was re-pinned from this final tree with
`python3 -I scripts/performance/cli.py harness-digest --write` to
`sha256:348cf76d702a1bcd1f483303207b65aec29256a300278afcd8b5f85660441805` (682 files), and
`--check` passes. Not established here: the actual Claw full comparison and its cancellation,
a telemetry build in the real local image, or the final head's CI. The outer operator runs
those.

## Final release-comparison validation repairs (2026-10-30)

Recorded in the AF implementation sandbox on macOS with Python 3.14.7, each suite in its own
owned writable temporary directory (`HOME` and `TMPDIR` set to a fresh `mktemp -d`). Nothing
here is a Claw measurement or a Linux run; the Linux-only tests named below were skipped on
this host and run in the outer operator's Linux validation and CI.

- **Bounded host-tool cleanup** (`perflab/hostobserver.py`). GitHub run 40998521481, job
  123877732296: `test_bounded_runner_ends_its_whole_group_at_its_deadline` reported
  `uncertain=True` because `Bounded.wait` reaped the leader and then read `killpg(group, 0)`
  success as a live descendant, although killed descendants a subreaper keeps unreaped still
  hold the group. `Bounded` now observes its leader's exit without reaping it (kqueue
  `NOTE_EXIT` on XNU, `waitid(WNOWAIT)` on Linux), so the group ID stays reserved; it kills and
  scans the group (`group_alive`) until no member executes, within `SETTLE_SECONDS`, and only
  then reaps the leader, signalling nothing after. A member in `Z`/`X` state counts as exited (a
  zombie leader thread beside live threads does not); a `stat` that cannot be read or parsed, a
  leader absent from the table or no longer leading its group, or a scan over its bound leaves
  the end `uncertain`, and so does a member that outlived a leader that exited of its own
  accord. XNU keeps its EPERM-for-zombies reading. Captures still run under the authority
  supervisor. Tests: `test_group_alive_tells_exited_members_from_executing_and_unestablished_ones`
  (fixture table: exited, live, zombie-with-threads, unreadable, unparsable, recycled leader),
  `test_a_group_of_exited_members_settles_and_no_other_group_is_signalled`,
  `test_an_executing_or_unestablished_survivor_leaves_the_end_unconfirmed` (only the reserved
  group is ever signalled; an unreadable member or absent leader signals nothing),
  `test_a_member_that_outlives_its_leader_is_killed_and_unconfirmed` (real processes), the
  unchanged CI test, and, Linux only,
  `test_killed_descendants_an_adopting_subreaper_keeps_unreaped_are_confirmed_ended`: a child
  subreaper in its own interpreter keeps the killed descendants unreaped while the runner
  settles, the condition reproduced on Claw at `be37` (the first assertion then failed in
  0.202 s), and reaps them afterwards.
- **TUI cleanup after a refused exit watcher** (`fixtures/performance/cli/tui-open.py`). The
  same CI job: `test_a_session_whose_exit_cannot_be_watched_is_still_ended_and_cleared` exited
  3 instead of 1 because the driver built `Exit` before reading the browser's start, so an
  EMFILE left `born` unset and an older same-user process with an unreadable environment (pid
  1165 on the runner) was not excluded. The browser's start is now read (`birth`) right after
  the spawn, before the watcher, and is kept through the watcher's refusal. Tests:
  `test_a_refused_watcher_keeps_the_browsers_birth_for_the_listing` (any platform: the listing
  receives the birth read before the refusal), and, Linux only,
  `test_a_refused_watcher_leaves_an_older_unreadable_process_untouched_and_certain` (an older
  undumpable sentinel of this user's, the Claw reproduction's condition: exit 1, browser and
  escaped descendant gone, sentinel still running and released by its own file, never
  signalled) and `test_a_refused_watcher_still_fences_a_newer_unreadable_descendant` (exit 3).
- **Comparison member binding** (`perflab/comparison.py`, `perflab/executor.py`). A member's
  request ID depends only on its kind, key and work, so a narrower `K:telemetry` original was
  accepted by full comparison `K`. Every reused member is now checked against the comparison's
  sources, submission, partition, frozen configuration digest and, for a core partition, its
  frozen plan (`_binding`): `comparison-preflight`/`comparison-launch` refuse a mismatched
  original before any new claim; the comparison's own submissions pass
  `bind_configuration=True`, so `Journal._submit` returns an existing original only when it
  froze that configuration (a plain resubmission keeps the original it froze, as before); the
  aggregate excludes such a partition; `status`, `report` and `export` mark it `refused` with a
  reason, so the comparison is never `complete` over its run list; `advance` derives no lane
  from a journal configuration that is no longer the declared one. Test:
  `test_a_narrower_same_key_original_refuses_the_full_comparison_and_never_completes` (real
  Journal, a completed overhead-only telemetry original, then the full comparison; also the
  same-configuration idempotent resubmission). The unchanged full-comparison test proves the
  valid resumed launch and no replay.
- **Telemetry registry version.** The registry is `af.perf-telemetry-lab/3`
  (`schemas/performance-telemetry-lab-v3.json`, regenerated; v2 removed); the fixtures and docs
  name `/3`, and `invalid-prior-version.toml` is the current registry under `/2`. Test:
  `TelemetryLabVersion` and the parity suite. The Task14 `af.perf-telemetry-build/2` is
  unchanged.
- **Pre-build workload and sample admission.** `select_contract` refuses a release no fixture
  contract names (`check-receipt@2` names 0.14.0-rc.1 to rc.3); `Journal._submit` admits, from
  each side's retained Cargo.lock, the telemetry contract of both sides and, for a plan with
  CLI cases, the authority's CLI registry and an adapter for both sides, before any claim or
  build. `image_admission` runs the sample image's own `/usr/bin/python3` once in a probe
  container (`sample_admission`: this account, no network, read-only root, no capability, no
  new privileges, removal confirmed) and refuses a missing interpreter, shell or driver module,
  or missing containment or cgroup v2. Library cases then kept their compile-time probe
  adapters; the final-slot cancellation and selected-workload section below admits them
  before any claim or build.
  Tests: `WorkloadAdmission` (0.11.9 refused for telemetry and CLI with nothing reserved;
  v0.13.0 against v0.14.0-rc.1 admitted with `check-receipt@1`/`@2` and `af_cli_v2`/`af_cli_v3`),
  `test_an_unusable_sample_runtime_tool_or_containment_refuses_before_any_claim` (ten probe
  answers, the fence settled clean), the ready-admission test's `sample_runtime` record, and
  `test_a_release_no_contract_names_is_refused_never_given_the_current_contract`. The Rust
  `telemetry_bench` judges' recorded documents now name 0.14.0-rc.1.

### Historical TUI first-frame audit (operator evidence, Claw, 2026-10-30)

Supplied by the outer operator; not run by this change. Unprivileged UID 991 containers
(network none, read-only tools and fixtures, every capability dropped, private HOME/XDG) under
the common machine lock ran each side's native `setup.sh` registry fixtures and the current
`tui-open.py` driver against the manifest-verified musl release binaries. The loaded fixture
was written by each side's own perf-probe public Store writer, exactly 10,000 events, in the
native `AF_PERF_FIXTURE_PREP` 512 MiB tmpfs; `setup.sh` copied and registered 20 Stores and
checked the registry count. Each first-frame document passed its schema, exit and numeric
interval checks, and every owned container was removed with cleanup confirmed. One observation
per side and fixture: workload and frame compatibility only, no statistical or performance
claim; the full 40-pair experiment gives the verdict. A first attempt without the native
preparation environment hit its 120-second seed bound and is retained as an operator setup
error, not evidence. Per side and fixture, the procedure uses the laboratory's own entry points
and the `tui_open` cases of `fixtures/performance/cli.toml` (the operator's literal scripts are
the receipt's logs, identified by their digests below):

```sh
# In a container of the audit image as UID 991 (--network none --read-only --cap-drop ALL),
# private HOME/XDG, the side's verified binaries and the fixtures mounted read-only.
# The loaded registry's Task seed, written by the side's own probe in the 512 MiB tmpfs:
AF_PERF_FIXTURE_PREP=/perf/prep sh "$FIXTURES/setup.sh" seed_task_state "$SEED_WORK" \
  "$SIDE/af" "$SIDE/perf-probe" "$TASK_SEED_REQUEST"
sh "$FIXTURES/setup.sh" seed_publish "$SEED_WORK" "$SIDE/af" "$SIDE/perf-probe" "$SEED"
# Each fixture in its own scratch, then the driver, which prints one af.perf-tui-open/1 line:
sh "$FIXTURES/setup.sh" registry_empty "$SCRATCH" "$SIDE/af" "$SIDE/perf-probe" -
sh "$FIXTURES/setup.sh" registry_stores_20x10000 "$SCRATCH" "$SIDE/af" "$SIDE/perf-probe" "$SEED"
(cd "$SCRATCH" && /usr/bin/python3 -I "$FIXTURES/tui-open.py" "$SIDE/af")
```

The receipt, as supplied:

```json
{
  "kind": "operator_verified_historical_tui_contract_audit",
  "producer": "be37b698ee8d66df00f340a50c04c5511b71007b",
  "sources": {
    "authority_sha": "be37b698ee8d66df00f340a50c04c5511b71007b",
    "baseline_sha": "e638219ff4a2a65d5403288cb186c9d2b73ec5f4",
    "candidate_sha": "e47741ac401754be822d92eca29b61f5afbe8418",
    "control_sha": "be37b698ee8d66df00f340a50c04c5511b71007b"
  },
  "image": "sha256:4f5e1e215d31e1e7844332afa5c4585bc63cdad88ae6e0b9237ffdbdeefbc2e1",
  "binary_manifest_entries": [
    {"path": "baseline/af", "sha256": "sha256:040210ebf43a8422ecd92ddbd68fec8e578cc348a3952adb1e6f123f88d8b463", "size": 58050304},
    {"path": "baseline/perf-probe", "sha256": "sha256:b64bb8562ffcf6b5f05262b66367902fd1332eaef7a3051b2913dbd250797041", "size": 23606816},
    {"path": "candidate/af", "sha256": "sha256:42152ad11083d26bb1501c05c3bc45aaf04b230d41883be48df4c40a8ba15f58", "size": 60380208},
    {"path": "candidate/perf-probe", "sha256": "sha256:c7c9a2eecbfcd168c1552d7aea6623888e9cd295703df79ccccf54f97e38ec78", "size": 23862992}
  ],
  "observations": [
    {"side": "baseline", "version": "af 0.13.0", "registry": "empty", "browser_ms": "6.364553", "exit_code": 0, "schema": "af.perf-tui-open/1", "splash_ms": "5.075649"},
    {"side": "baseline", "version": "af 0.13.0", "registry": "stores_20x10000", "browser_ms": "5730.15642", "exit_code": 0, "schema": "af.perf-tui-open/1", "splash_ms": "4.935427"},
    {"side": "candidate", "version": "af 0.14.0-rc.1", "registry": "empty", "browser_ms": "7.555114", "exit_code": 0, "schema": "af.perf-tui-open/1", "splash_ms": "5.974847"},
    {"side": "candidate", "version": "af 0.14.0-rc.1", "registry": "stores_20x10000", "browser_ms": "10.21211", "exit_code": 0, "schema": "af.perf-tui-open/1", "splash_ms": "4.998213"}
  ],
  "elapsed_seconds": 79.445,
  "cleanup_confirmed": true,
  "statistical_performance_claim": false,
  "source_files_sha256": {
    "candidate.log": "8f1dd786d665b4055f5a8b87cc1c38de1bf0c66cbaf34117d584e1afd29b74bf",
    "baseline.log": "455075b19e54f947563950aff6172a1c4dc9a31a36156dc2363ed5c6e66ca82f",
    "historical-tui-audit-02-20261030.json": "18bb8614ce6f7aae7b85960909cfbe8fb6a987863114b23beaf629700c22c970"
  }
}
```

### Image admission records (Task13) and this change's focused runs

The outer operator ran `tests.test_builds.ImageIdentity` and `tests.test_executor.ImageAdmission`
on combined `1cbe414667f2ebe337b80b16d13840ad21cf90aa` with Python 3.14.7 and private
HOME/TMPDIR: 10/10 passed in 17.524 test seconds (18.178 s outer), exit 0, log SHA-256
`bfbdf18a987a72f03d3f58eb74416ad798d59015b3d2c0df9108e78dcb5ee3e3`. This change reran them on
its final source, with its own new sample-admission test, as below.

Each command ran from the repository root with `HOME` and `TMPDIR` set to a fresh owned
directory, Python 3.14.7, on macOS (the Linux-only tests skip here).

```sh
python3.14 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_hostobserver","tests.test_tui_open","tests.test_comparison","tests.test_executor","tests.test_contracts","tests.test_telemetry_lab","tests.test_telemetry_bench_contracts","tests.test_builds","tests.test_calibration","tests.test_localcalibration","tests.test_planning","tests.test_diagnostics","tests.test_telemetry_cleanup","tests.test_telemetry_collect","tests.test_telemetry_exec","tests.test_telemetry_lock_handoff","tests.test_telemetry_measurements","tests.test_telemetry_terminal","tests.test_telemetry_workflow","tests.test_whole_source_repairs"],verbosity=1)'
```

**526 tests, 1003.6 seconds: 1 failure, 4 errors, 21 skipped.** Four are this macOS host's, on
paths this change does not touch: three `tests.test_executor.Journal` profile cases refuse
with `environment linux_x86_64_*_claw is the contained Linux profile; this host is macos`, and
`test_diagnostic_build_removal_uncertainty_keeps_borrowed_executor_fence` reads
`host_mismatch` from the stand-in toolchain's macOS host; all four failed identically before
the workload and sample admission existed. The fifth,
`test_telemetry_lab.Runs.test_documents_unlike_their_planned_run_are_refused_even_when_both_sides_agree`,
was this change's: its foreign stand-in document named af 9.9.9, which no fixture contract
names any more; it now names another declared release (0.14.0-rc.1) and passes below. An
earlier run of the same modules failed four `test_localcalibration.LocalOriginals` cases (their
synthetic suite's process cases have no CLI registry; they run no shard, so they now stub the
CLI admission `WorkloadAdmission` proves) and once
`test_telemetry_collect.Collection.test_rotation_keeps_old_results_retries_and_higher_attempts_eligible`,
which passed when rerun alone and in the run above.

```sh
python3.14 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_hostobserver","tests.test_tui_open","tests.test_telemetry_lab","tests.test_telemetry_bench_contracts","tests.test_contracts","tests.test_comparison","tests.test_builds.ImageIdentity","tests.test_executor.ImageAdmission","tests.test_executor.WorkloadAdmission"],verbosity=1)'
```

**PASS: 109 tests, 122.6 seconds, 5 skipped (Linux only), exit 0.** It includes the Task13
`ImageIdentity` and `ImageAdmission` suites (both malformed-identity cases and the unconfirmed
probe container that keeps the fence) with the new sample-admission test.
`cargo nextest run --workspace --locked --profile ci -E 'binary_id(af::it) &
test(/^telemetry_bench::/)'`: 7 passed. `python3 -I scripts/performance/schemas.py`,
`python3 -I scripts/test-classes.py audit`, `cargo fmt --all -- --check` and `cargo clippy
--workspace --all-targets -- -D warnings` pass. The protected harness was re-pinned after the
last edit with `python3 -I scripts/performance/cli.py harness-digest --write` to
`sha256:52d8516373e80469b112a033166b68f66b63436428faec212d96193a57799b4f` (682 files), and
`--check` passes. Not established here: the Linux-only tests above, the actual Claw full
comparison and the final head's CI, which the outer operator runs.

### Final-slot cancellation and selected-workload admission (2026-10-30)

A narrow repair on the combined candidate; every earlier repair above is unchanged. Nothing
here ran Docker, Claw, GitHub Actions or a measurement, and no signal was sent to any process.

- **A signal in the final slot interrupts the shard** (`perflab/runner.py`, `Shard.run`). The
  shard's handler latches the signal, and the loop checked it only before a slot. A SIGTERM
  during the final sample that then returned normally left no later check: the shard sealed
  `complete`, and under a lease `run_shard` raised `Cancelled` with that complete record inside
  an `interrupted` original. Now, after the handlers are restored and before the seeds are
  cleaned up, a latched signal with no stop already made becomes the `cancelled` stop. The
  shard seals `interrupted` and every completed record is kept: `record_count` equals
  `expected_records`, no slot is failed or unstarted, and the comparator still reads the run as
  `incomplete` (`shard_not_complete`). A stop a slot already made keeps its status, failure and
  retained scratch, such as an uncertain cleanup that keeps the fence. Seed cleanup and lease
  marking run as before.
  `test_runner.ControllerCancellation` invokes the handler the real shard installed from
  inside its final slot, after the stand-in sample returned:
  `test_a_signal_in_the_final_slot_under_a_lease_seals_interrupted_then_cancels`,
  `test_a_standalone_shard_a_signal_ends_in_its_final_slot_returns_interrupted` and
  `test_an_uncertain_final_slot_keeps_its_stop_and_fence_when_a_signal_follows`. Before the
  fix, the first two failed: their sealed shard had no failure. The executor's cancellation
  stand-in now seals what the fixed runner seals: every record kept, status `interrupted`.
  `test_a_cancelled_shard_settles_confirmed_cleanup_interrupted_and_never_replays` asserts that
  the original's kept `shard.json` and its result both say `interrupted`.
- **Library workload admission before any claim or build** (`perflab/executor.py`
  `library_workload`, `perflab/adapters.py` `coverage`). The native adapter registry is
  `af.perf-adapters/2` (`schemas/performance-adapters-v2.json`; v1 removed and refused,
  `invalid-previous-version.toml`, `invalid-missing-min-version.toml`). Each adapter declares
  `min_version`; `native_probe_public_api_v1` declares 0.12.0-rc.3, the release whose real
  builds compiled it (ADR-0168, ADR-0169). For every core original, both sides need an
  adapter that declares their release and serves the sentinel's contract and every selected
  library case's contract, its seed's included. An authority without the registry, an
  undeclared release (0.11.9) or an unserved contract refuses with side, release and reason,
  even for a library-only plan. The build still chooses by compile, but only after this
  admission. Tests:
  `test_unknown_or_absent_library_coverage_refuses_before_any_claim_or_build` and
  `test_declared_library_coverage_is_admitted_for_each_side` (0.13.0/0.14.0-rc.1 and
  0.12.0-rc.3).
- **Per-case CLI admission** (`cli_workload`). Every selected process case and its parameters
  needs a declared, runnable command in the chosen adapter of each side. `af_cli_v1`'s pending
  `tui_open` refuses 0.12.0-rc.3 with its exact reason, and its discovery commands stay
  admitted, as do the 0.13/0.14 browser cases. An authority whose registry omits a command is
  refused as before. Tests: `test_a_pending_cli_command_refuses_before_any_claim_or_build`,
  `test_runnable_cli_commands_of_each_adapter_stay_admitted` and
  `test_a_cli_command_the_authority_does_not_declare_refuses_before_any_claim`. Each refusal
  test also asserts that no original exists and that no build or scratch preparation ran.
  Before the change, the library and pending tests failed.

The focused run started at 09:39:45 MSK from the repository root, with `HOME` and `TMPDIR` set
to a fresh owned directory, under Python 3.14.7 on macOS:

```sh
python3.14 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_runner.ControllerCancellation","tests.test_executor.Journal.test_a_cancelled_shard_settles_confirmed_cleanup_interrupted_and_never_replays","tests.test_executor.Journal.test_a_cancelled_shard_with_unconfirmed_cleanup_is_uncertain_and_keeps_the_fence","tests.test_comparison.Comparison.test_a_controller_cancelled_in_a_shard_runs_no_later_shard_or_original","tests.test_executor.Journal.test_local_telemetry_publishes_and_verifies_an_actual_local_image_id","tests.test_executor.Journal.test_telemetry_build_records_refuse_previous_versions_and_mutable_references","tests.test_executor.WorkloadAdmission","tests.test_contracts.Parity"],verbosity=2)'
```

**PASS: 25 tests, 44.7 seconds, exit 0.** The run covered the 5 runner cancellation tests, both
executor cancellation outcomes, the comparison's whole-controller propagation (no later shard
or original, no replay), the two Task14 image-provenance tests, the 7 `WorkloadAdmission`
tests and the 8 schema parity tests.

Two wider runs used the same environment:

- `tests.test_comparison`, `tests.test_localcalibration`, `tests.test_contracts`,
  `tests.test_builds` and `tests.test_planning`: 94 tests, OK.
- `tests.test_activation`, `tests.test_calibration` and `tests.test_diagnostics`: 161 tests,
  OK.
- `tests.test_runner`: 65 tests, OK.
- `tests.test_executor`: 57 tests, 1 failure and 5 errors.
  - Four of these are this macOS host's, as recorded above: three profile cases with `this
    host is macos`, and `host_mismatch` in the diagnostic-build removal test.
  - Two `AuthorityAndShapes` cases refused a bytecode cache. An earlier inspection command,
    run without `-B`, left it in the tree. After the cache was removed,
    `tests.test_executor.AuthorityAndShapes` ran again: 9 tests, OK.

The protected harness was re-pinned after the last protected edit with `python3 -I -B
scripts/performance/cli.py harness-digest --write`, to
`sha256:e905d33fe5473c9ff117e637f957057bc787676df73d5afd8a776aeb3bd974bb` (684 files), and
`--check` passes.

Not established here, and owned by the outer operator:

- the exact final Claw regression tests;
- the pinned Rust 1.88 manual harness;
- the full 33-case release comparison;
- the portable export;
- the final head's CI.

### Bytecode-free Linux subreaper regression in writable checkouts (2026-10-30)

A narrow CI repair to a test; no product code changed. Every earlier repair above, the
production refusal of bytecode caches, the schema versions and the suite samples and bounds
are unchanged. Nothing here ran Docker, Claw, GitHub Actions or a measurement.

- **The defect.** GitHub run 41012988068 on `e67f8bd` (CI 695, Python 3.15.2) failed the
  classperf job: 1209 tests, 72 failures, 121 errors. The Linux-only
  `test_hostobserver.HostObserver.test_killed_descendants_an_adopting_subreaper_keeps_unreaped_are_confirmed_ended`
  passed, but its child ran `[sys.executable, "-I", "-c", SUBREAPER, ...]` and imported
  `perflab.hostobserver` from the writable checkout. `-I` ignores `PYTHONDONTWRITEBYTECODE`,
  and the child does not inherit the parent's `-B`, so the child wrote
  `scripts/performance/perflab/__pycache__`. Every later test and CLI run then refused that
  source ("measurement library bytecode cache refused"), as the authority must. The outer
  operator reproduced this on Claw in a writable copy of the `e67` source. Earlier Claw checks
  ran on read-only source, and macOS skips the test, so neither saw it.
- **The repair** (`tests/test_hostobserver.py`). The child sets `sys.dont_write_bytecode`
  before any import and runs with `-I -B`. The test no longer imports the checkout: it copies
  `perflab` with `tests.support.writable_copy` into its own temporary directory and runs the
  child there, with every `PYTHON*` variable removed and with private `HOME`, `TMPDIR` and
  working directory. A failure therefore cannot write into the checkout. The test asserts that
  the copy is bytecode-free before and after the run. It also asserts that the child imported
  `hostobserver.py` from the copy and that the real subreaper outcome is unchanged
  (`timed_out` true, `uncertain` false). `tests.test_telemetry_exec.AncestorSubreaper` covers
  the telemetry executor the same way. A scratch check on macOS ran the child against such a
  copy without any suppression: the child stops at `prctl`, after its import, and had written
  25 bytecode files. With the source line alone, or `-B` alone, it wrote none.

The focused run started at 09:59 MSK from the repository root, with `HOME` and `TMPDIR` set to
a fresh owned directory and `PYTHONDONTWRITEBYTECODE` unset, under Python 3.14.7 on macOS:

```sh
python3.14 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_hostobserver","tests.test_kernelguard","tests.test_telemetry_exec.AncestorSubreaper","tests.test_whole_source_repairs"],verbosity=1)'
```

**PASS: 72 tests, 99.0 seconds, 8 skipped, exit 0.** All eight skips are Linux-only: the
repaired subreaper test and the seven `AncestorSubreaper` cases. Afterwards
`python3.14 -I scripts/performance/cli.py --help` exited 0 on the checkout, and the tree held
no `__pycache__` or `.pyc` file. The protected harness was re-pinned after the last protected
edit with `python3 -I -B scripts/performance/cli.py harness-digest --write`, to
`sha256:8733e772805cdfc71296593f950a85fca19d3151f38bef018e1e3f6061eacc75` (684 files), and
`--check` passes.

The repaired Linux test did not run on this host. The outer operator owns:

- the repaired test on Claw in a writable copy, followed by the copied CLI and bytecode checks;
- the final head's CI.

## Offline comparison derivation, unsampled executables and the full recipe (2026-10-30)

Recorded in the AF implementation sandbox on macOS with Python 3.14.7, `HOME` and `TMPDIR` set
to a fresh `mktemp -d` directory. Nothing here is a Claw measurement or a Linux run. The
running native comparison `cmp_b748bbb15eb5c84455ea0e6669570c2089b31726a62a43ed` was not
touched or replayed. Correction (2026-10-30): an earlier version of this record called applying
this verifier to that comparison's export the outer operator's step. It is not final proof:
`telemetrylab.verify` requires its installed authority digest to equal every measured authority
byte, so the final report needs a new comparison measured on the final repaired source, and
that comparison's 4-shard first shard did not finish
([below](#eight-serial-shards-for-the-full-release-comparison-2026-10-30)).

- **Offline verification derives the report** (`perflab/comparison.py`). Before this change,
  `verify_export` checked hashes and the members the report named, and `verify_native` skipped
  every member the report did not call complete. A bundle could therefore relabel a failed
  telemetry original, or queued members with no native result, as complete, rehash its report
  and manifest, and pass. `verify_export` now runs `status` over `Bundled`, a read-only journal
  view of the bundle. The view reads the bundled declaration's members, binds each original's
  state to its sealed result, and takes the frozen configuration from a bundled original under
  the declaration's digest. Each complete original is admitted offline (`admit`):
  configuration, provenance, the native evidence its work requires sealed and bundled, its core
  run evaluated again, its telemetry verified again, its profile children admitted again.
  `aggregate_claims` recomputes the aggregate's partition entries, its run's shards (byte for
  byte those of its included partitions), its build, result, coverage and regressions. A report
  that is not exactly that derivation is refused. The `af.perf-comparison-report/1` contract
  also refuses a complete lane over an original that is not complete, and a complete comparison
  over one that is not verified. Its shape is unchanged.
  - `Comparison.test_a_failed_telemetry_export_verifies_incomplete_and_never_relabelled_complete`:
    a real failed telemetry original with its sealed native run. The export verifies as
    `incomplete` through `cli.py executor comparison-verify`. A complete telemetry lane over
    the failed original is refused by the contract. The original also relabelled complete is
    refused by the derivation. Every lane, original and the comparison relabelled complete with
    reasons cleared, report and manifest rehashed (manifest `status` `complete`), makes the
    public command exit non-zero. The state record relabelled complete is refused against its
    sealed result.
  - `DeclarationOnly.test_queued_members_relabelled_complete_with_no_native_result_are_refused`:
    the schema fixtures `comparison/valid-release.json` and `comparison-report/valid-running.json`
    with three queued members and no native result. Relabelled complete, the contract refuses
    it. Relabelled complete and verified too, it is refused for lack of a frozen plan. With a
    plan the declaration names, it derives as unfinished and is refused.
  - `Comparison.test_full_comparison_partitions_aggregates_diagnoses_reports_and_exports`, extended:
    the honest incomplete bundle with both diagnostic lanes still verifies. Its report relabelled
    complete is refused. Its aggregate relabelled as judged whole is refused against its verdict.
    A dropped trace member is refused, with or without its records. A complete export with a
    real regression (the family judged whole, injected for the journal's report and the offline
    derivation alike, as the existing status assertion injects it) verifies `complete` with
    verdict `fail`. Relabelling that verdict `pass` is refused.
- **Known build ID, no recording hit** (`perflab/diagnostics.py`, `perflab/diagcontracts.py`).
  A capture whose recordings never hit the profiled executable's known build ID was kept
  usable (`binary_build_id_present: false`, no omission) and rendered. It is now omitted as
  `executable_not_sampled`, every recording kept and nothing rendered. `profiles.json` refuses
  such a capture without an omission, so `report.admit_child` and the comparison's profile lane
  refuse a forged usable claim. Its shape is unchanged.
  - `RepeatedCaptures.test_no_recordings_hit_of_a_known_build_id_omits_the_capture_and_renders_nothing`
    replaces `test_no_recordings_hit_of_the_executable_is_never_inferred_from_the_executable`,
    which expected no omission. It also checks that the omission removed and resealed is refused
    at admission.
  - `RepeatedCaptures.test_a_build_id_less_executable_keeps_its_hash_bound_capture_and_renders`:
    an executable with no build ID (musl) keeps its capture bound by its exact hash and
    retained archive, renders its differential flamegraphs and is admitted.
  - `test_a_later_recordings_hit_of_the_executable_is_found_when_the_first_has_none`
    (known build ID, hit) is unchanged and passes.
- **The full release comparison recipe** (`docs/performance-local-executor.md`, "Validate an
  exact candidate producer", steps 3 and 5). The guide now gives the candidate configuration's
  replaced keys: a fresh root, journal and mirror, `tier = "nightly"`,
  `environment = "linux_x86_64_musl_claw"`, `shards = 4`, `cases = []`, and the `manual`
  telemetry profile with its three experiments and 2 rounds, no overrides. It also gives the
  checks of the frozen plan and telemetry workload before `comparison-run`.
  `Comparison.test_the_documented_full_comparison_configuration_and_checks_hold` parses both
  documented blocks. Applied to an executor configuration, the configuration validates as
  `af.perf-executor/3` and launches. The documented check script, run as written on the
  launched journal, prints `('nightly', 'linux_x86_64_musl_claw', 4, 33, 31, 2, 2852)` and 12
  telemetry runs. A narrower telemetry selection fails it. A scratch plan of the suite gave
  33 instances, 31 judged, 2 exploratory and 2852 slots for `nightly`, and 11 instances and
  1004 slots for `release`, which is unchanged. These were the counts tested then. The recipe
  is now 8 shards and 2932 slots
  ([below](#eight-serial-shards-for-the-full-release-comparison-2026-10-30)).

The focused run started at 11:43 MSK from the repository root:

```sh
python3.14 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_comparison","tests.test_contracts","tests.test_report","tests.test_report_diagnostics","tests.test_diagnostics","tests.test_executor"],verbosity=1)'
```

**219 tests, 581.4 seconds: 1 failure, 3 errors, exit 1.** All four are this macOS host's,
already recorded above, and on paths this change does not touch. The three
`tests.test_executor.Journal` profile cases
(`test_local_diagnostic_uses_native_child_planner_report_and_admission` and both subtests of
`test_shipping_nightly_release_reach_both_builders_with_frozen_pinned_tools`) refuse with
`environment linux_x86_64_*_claw is the contained Linux profile; this host is macos`.
`test_diagnostic_build_removal_uncertainty_keeps_borrowed_executor_fence` reads
`host_mismatch`. The comparison, contract, report and diagnostics suites passed whole. The
protected harness was re-pinned after the last protected edit with
`python3 -I -B scripts/performance/cli.py harness-digest --write`, to
`sha256:6c8afd07290b0b4ff54082fd0861d83ac0f400871bdb01b7e981e49b164a667a`, and `--check`
passes. The native light gate is the full gate. The outer operator owns the repaired verifier
on the actual native export, Linux proof and the final head's CI.

## Eight serial shards for the full release comparison (2026-10-30)

The full release comparison recipe of the previous record (`shards = 4`) cannot finish the
v0.13.0 (`e638219ff4a2a65d5403288cb186c9d2b73ec5f4`) against v0.14.0-rc.1
(`e47741ac401754be822d92eca29b61f5afbe8418`) pair inside the unchanged per-shard window. Native
evidence on Claw, producer `cbd8661c010e7bcb65e6cd3adf44f36be83a720b`, comparison
`cmp_b748bbb15eb5c84455ea0e6669570c2089b31726a62a43ed`:

- The first shard expected 776 records and sealed 738, all valid: 0 failed, 38 unstarted,
  `interrupted` with stopping reason `sampling_deadline`. It ran 2326.044 seconds and refused to
  start sequence 738, because that slot's 345-second worst-case budget would exceed what
  remained of the 45-minute shard window. Sealed shard
  `sha256:3d388d5cc9f4a8b7a5c808966b41fbafe92cb53be05fefafb22603e67210eec5`, samples
  `sha256:8adac03caad80c4a009d7943eeb9907b35dbf6fd0e19038394a074b01c622898`.
- A native SIGTERM during the next shard (1) settled that original `interrupted` with its
  cleanup confirmed. The later originals never started, and the fence was cleared.
- The failed original, its configuration and its evidence stay as they are: not retried, not
  relabelled. This is an admission and time-budget observation, not a performance verdict.

The recipe now uses 8 smaller serial shards (`core.cases = []`, `tier = "nightly"`,
`environment = "linux_x86_64_musl_claw"`, `shards = 8`). The workload is unchanged: 33 case
instances, 31 judged and 2 exploratory, 40 pairs and the same warmups, and the 12 `manual`
telemetry runs. No window, deadline, cap or sentinel changes, and the same single machine lock
and fence apply. The plan has 2932 slots: the 2852 of 4 shards plus the 80 health-sentinel slots
(5 start and 5 end pairs, both sides) of 4 more shards. The schedule partitions the 8 shards into
4 serial originals of 2 shards each (worst case 6630 seconds for the first, which builds, and
6030 for each later one, which reuses its immutable build, of 7200). The suite's `release` tier
(11 cases, 1004 slots) is unchanged. Smaller shards adapt to the measured run; the preflight
checks worst-case bounds and does not predict how fast a host samples, so it guarantees no
completion on other hardware.

A native copied 8-shard plan on producer `cbd8661c010e7bcb65e6cd3adf44f36be83a720b` was
verified by the outer operator: comparison
`cmp_cb9089869680e466235977010fdbbeda55b297b661b7b049`, plan
`sha256:97325fb52b873b193f8cab28d0c3af125b3a08bea5d0adffe20eb40dcecf1b69`, slots
`[440, 356, 356, 356, 356, 356, 356, 356]`, partitions `[[0, 1], [2, 3], [4, 5], [6, 7]]`, 12
telemetry runs. It is preliminary configuration and timing evidence on that producer, not the
final report: the final comparison must be launched on the exact final repaired source, whose
installed authority the verifier requires every measured authority byte to equal. That final
Claw comparison, its report and export, and their verification on Claw and macOS are pending
with the outer operator. Nothing here claims them.

Recorded in the AF implementation sandbox on macOS, Python 3.15.0, `TMPDIR` a fresh
`mktemp -d` directory; nothing was measured. A scratch probe drove the real launch path of
`tests.test_comparison.Comparison`'s fixture with the documented configuration and read the
retained plan and declaration. With `shards = 4` it gave slots `[776, 692, 692, 692]` (2852) and
2 originals; with `shards = 8`, slots `[440, 356, 356, 356, 356, 356, 356, 356]` (2932),
partitions `[[0, 1], [2, 3], [4, 5], [6, 7]]`, worst cases `[6630, 6030, 6030, 6030]` seconds of
7200. `Comparison.test_the_documented_full_comparison_configuration_and_checks_hold` now
expects `('nightly', 'linux_x86_64_musl_claw', 8, 33, 31, 2, 2932)`; it still launches the
documented configuration, runs the documented check script and fails a narrower telemetry
selection. The stray blank line at the end of `scripts/performance/tests/test_comparison.py`
was removed. The protected harness was re-pinned after that last protected edit with
`python3 -I -B scripts/performance/cli.py harness-digest --write`, to
`sha256:266f29f9be021c8934f1eb5bd9e226bd5f62c190b884a00c2ed6ca618bbcb37c`, and `--check`
passes.
`git diff --no-index --check /dev/null <file>` (the sandbox tree is not a Git checkout)
reports no whitespace error in any changed file; it reports `new blank line at EOF` on a
control file ending in one. markdownlint-cli2 0.22.1 reports 0 errors in the changed Markdown.

The focused run, from the repository root:

```sh
python3 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); unittest.main(module=None,argv=["unittest","tests.test_comparison"],verbosity=1)'
```

**PASS: 9 tests, 42.5 seconds, exit 0.** The native light gate is the full gate.

## Integration with main `fa85b96` (2026-10-30)

The source is Git's merge-tree of the PR head `894cdb97428daa0bcec60caace700289671cf12d` and
main `fa85b966410b24f2ab59f0285bedc42149495f4c` (merge base
`955e24bff603675feb5680d0cf0e4cc60cd4b6f2`). Exactly three files held conflict markers, and only
they changed, with this section and one change note:

- `docs/adr/README.md` lists the PR's 0265 and main's 0267 in numeric order; both ADR files were
  already in the tree and neither decision changed.
- `fixtures/performance/kernel-measure/code-policy-measures.toml` and `inventory.json` were
  seeded with the PR side, then regenerated with
  `python3 -I -B scripts/performance/cli.py harness-digest --write`, and `--check` passes. The
  digest of the integrated source is
  `sha256:0d9ea79c4aba911c7df48426a20786ceecd5274a264e71dc6a7ca4a465d87936` for **684 files**;
  neither side's previous digest (`266f29f9…`, `b8d689fd…`) remains in the fixtures. The
  measures, repetitions, wall and authority are unchanged.

Recorded in the AF implementation sandbox on macOS, Python 3.15.0, `TMPDIR` a fresh
`mktemp -d` directory. No conflict marker remains in the tree. `tests.test_comparison`, run with
the command above, **PASS: 9 tests, 48.0 seconds, exit 0**, including
`Comparison.test_the_documented_full_comparison_configuration_and_checks_hold` (eight serial
shards, 33 cases, 2932 slots, and a narrower telemetry selection refused);
`tests.test_kernelguard` **PASS: 29 tests**. Nothing was measured: no new VM or Claw
measurement and no green final CI is claimed. The final comparison on the integrated producer,
its report, export and verification are pending with the outer operator.

## Integration with main `26d8a6d` and the merged nightly executor (2026-10-30)

The source is Git's mechanical merge-tree `327d6448c198245ffcc184f73ed2b50a5adb74b6` of the PR
head `6d8208d4dc8de6413d5ec154f19524fd67b271bd` and main
`26d8a6da424c125db2bed32694dc5a9bd5b08481`, with conflict markers in ten files. Both sides are
kept; the shared contracts were reconciled:

- **Request versions.** Main's `af.perf-executor-request/2` is its nightly submission; the PR
  had independently used request/2 for a profile's `parent` and a comparison's `partition`.
  The combined shape is `af.perf-executor-request/3` (main's tagged submission, `parent`,
  `partition`); every writer writes it. Request/1 and main's request/2 stay readable for a
  journal's immutable originals, as ADR-0266 states, and a reader of either treats an absent
  `parent` or `partition` as none. A request/3 shape labelled request/1 or request/2 is
  refused (`invalid-v2-partition.json`), as is a request/2 shape labelled request/3
  (`invalid-v3-missing-partition.json`). A nightly is never a partition or a parent-selected
  profile. A durable comparison (`comparison.Bundled`) admits only current request/3 and
  result/3 members, so its members and its export carry only those versions. The standalone
  `overview` reads each named original through `overview.member`, which still reads an earlier
  request version and treats its absent `parent` as none, as ADR-0266 states. `preflight`,
  `comparison-preflight` and `overview` refuse a nightly. Result/3 is main's, unchanged.
- **Windows.** Main gave the `nightly` tier 15 sampling and 180 run minutes. The full release
  comparison recipe keeps the 45 and 120 its largest shard (440 records) was measured in, by
  an explicit `[core.windows]` of `af.perf-executor/4`. The plan freezes the windows in place
  of the tier's, under the unchanged hard maxima; they are part of the configuration and plan
  digests and of the recomputation `verify` performs, and a calibration original refuses
  them. Executor v2 and v3 configurations are refused. Declared windows are a `local` backend
  setting: `performance.yml` has no windows input, so a `github_actions` configuration that
  declares them is refused when it is admitted (see the record below). So the 6d full comparison's executor/3
  journal is never verified under this source.
- **ADR.** Main's ADR-0265 (nightly build), 0266, 0267 and 0268 are unchanged. The PR's
  partition ADR is renumbered ADR-0269, with a dated amendment, and every reference to it
  follows.
- **Generated files.** `python3 -I scripts/performance/schemas.py --write` rendered
  `performance-executor-request-v1.json`, `-v2.json` and `-v3.json` and
  `performance-executor-v4.json`; the stale `performance-executor-v3.json` was removed, and the
  check passes. The kernel-measure fixtures were seeded with the PR side, then
  `python3 -I -B scripts/performance/cli.py harness-digest --write` re-pinned them to
  `sha256:fae94f3dad2dc1deca60739920bb7b669d7c727d30e031ad1864ea1231c9cdf4` (722 files);
  `--check` passes.

Recorded in the AF implementation sandbox on macOS, Python 3.15.0; nothing was measured, and no
Docker, network or benchmark was used. Each command below ran from `scripts/performance` as
`python3 -I -B -c` over `unittest`:

- `tests.test_comparison`: **9 tests OK**. These include the documented full-comparison
  configuration and checks (`('nightly', 'linux_x86_64_musl_claw', 8, 33, 31, 2, 2932)`,
  windows `(45, 120)`, partitions `[[0, 1], [2, 3], [4, 5], [6, 7]]`, each later one reusing
  the first one's build, and 12 manual telemetry runs), and a narrower telemetry selection is
  refused.
- `tests.test_contracts tests.test_localcalibration tests.test_planning tests.test_suite
  tests.test_kernelguard`: **104 tests OK**.
- `tests.test_runner tests.test_compare tests.test_report`: **130 tests OK**. A partition
  authority with any total but the plan's is refused, and one past 180 minutes fails the
  shape check.
- `tests.test_diagnostics tests.test_report_diagnostics tests.test_whole_source_repairs
  tests.test_builds`: **185 tests OK** (1 skipped).
- `tests.test_executor`: **64 tests, 4 cases fail on this macOS host**, as in the earlier
  records. Each stops in `diagnostics.check_mode` with `host_mismatch`:
  `test_local_diagnostic_uses_native_child_planner_report_and_admission`, the two profile
  subtests of `test_shipping_nightly_release_reach_both_builders_with_frozen_pinned_tools`,
  and `test_diagnostic_build_removal_uncertainty_keeps_borrowed_executor_fence`. With
  `diagnostics.host_os` patched to Linux, all three tests pass. The other tests pass. Among
  them, the ordinary nightly's 4-shard plan is ready with 15 and 180 minutes and the declared
  windows freeze 45 and 120. Main's nightly tests pass, and so does its request/1-result/2
  readability test, now also refusing request/3 under request/1 and request/2.
- `tests.test_nightly_build tests.test_hostobserver tests.test_hostlock`: **63 tests, 1
  failure**. `CargoRewrite.test_a_rewritten_copy_of_this_tree_still_resolves_locked_and_offline`
  expects Cargo's `needs to be updated`, but this host's Cargo prints `cannot update the lock
  file ... because --locked was passed`. This change does not touch that test or its paths.

The 6d full comparison running under the frozen PR head remains that producer's evidence. It
is not this candidate's validation, and nothing here claims a Claw run, a final report or green
CI for this source.

## Hosted windows refusal and integration evidence corrections (2026-10-30)

**Reproduction.** On the integrated candidate `208fab5b84e7b7f292dd051b91532440f3055a6b`, with
Python 3.14.7, the outer operator drove the native `ExecutorFixture`, the real `Journal.submit`
and `dispatch_actions` with a fake transport (no network, nothing measured). A
`github_actions` executor/4 configuration declaring quick windows 20 and 110 (tier defaults 25
and 60) was admitted, and dispatch returned `submitted` with a payload of only `base`, `head`,
`control`, `scope`, `tier`, `cases`, `shards` and `authority`: `performance.yml` has no windows
input, so the declared windows were silently ignored.

**Change.** `validate_config` refuses a non-null `core.windows` unless the backend is `local`,
so a hosted configuration that declares them fails when it is loaded, when a `Journal` opens
it, or when `submit` is given it, before any original is reserved or dispatched. The workflow
is not extended and the windows are never cleared. The executor/4 shape is unchanged, so no
schema version, schema file or fixture changes. `Journal.test_hosted_declared_windows_refuse_before_any_original_or_transport`
proves the refusal at all three points with no original and no transport call, that the same
hosted configuration without windows still dispatches `performance.yml` with exactly those
eight inputs, and that a local configuration keeps the declared windows.

**Corrections.** The executor guide said an absent or empty `[core.windows]` keeps the tier's
own; an empty table is refused (`executor.core.windows missing field shard_sampling_minutes`),
so it now says absent (or JSON `null`) keeps the tier's own and a declared table supplies both
numbers. The guide's 30 minutes 52 seconds for the 440-record shard is the preliminary producer
`cbd8661c`'s, not `6d8208d`'s. The frozen producer `6d8208d`'s shard 0 sealed complete, 440 of
440 records, 0 failed and 0 unstarted, started `1793354884652` and ended `1793356916852` Unix
ms, 2032.200 seconds (33 minutes 52.2 seconds); its native `shard.json` has SHA-256
`0a6ff1e81cb090ebd32aaa9f22c10e1266c2aa133bc4a0cc1faa5a7e8ff2d8d7`. Both are evidence of their
own producers only, neither is this source's validation, and the full `6d` comparison
continues as that producer's evidence. The integration record above said a comparison's
overview accepts only request/3 and result/3; it now distinguishes the durable comparison's
current-version members and export from the standalone overview, which still reads earlier
request versions.

## The nightly lock-rewrite proof resolves for the host platform (2026-10-30)

**Reproduction.** GitHub CI run 41050608459 on `fb3b632d`, performance job 124045553040:
`scripts/test-performance.py` ran 1268 tests with one failure,
`CargoRewrite.test_a_rewritten_copy_of_this_tree_still_resolves_locked_and_offline`. After the
nightly rewrite, `cargo metadata --locked --offline` exited 101 with `failed to download
anstyle-wincon v3.0.11` and `attempting to make an HTTP request, but --offline was specified`.
Unfiltered `cargo metadata` downloads every platform's packages, and the CI Cargo cache holds
only the Linux build's. On macOS with Cargo 1.88, a Cargo home holding the index and only the
187 archives `--filter-platform x86_64-unknown-linux-gnu` resolves gave exactly that error. The
stale-lock step already failed before any download.

**Change.** The test now runs `cargo metadata --locked --offline --filter-platform <host>`,
where `<host>` is what `cargo -vV` prints. It first resolves the unchanged copy through the
test's own Cargo home. From that, it builds a private Cargo home with the shared index, the
host's archives and their unpacked sources, and nothing else. It asserts the lock names
packages this home does not hold, so the proof runs with the CI cache's gap on every host.
Rewriting only `Cargo.toml` must fail with Cargo's stale-lock refusal: `the lock file ... needs
to be updated but --locked was passed` (Cargo 1.88) or `cannot update the lock file ... because
--locked was passed` (Cargo 1.94). The refusal must also contain no `failed to download`,
`--offline was specified` or `no matching package`. After `rewrite_source`, the resolution must
succeed and every workspace member must report the nightly version. Without the filter, the
same test fails on macOS with the CI error. The nightly build, its `cargo fetch`, the CI cache
and the test selection do not change.

**Focused validation on `b909` (operator).** On `b909353dac34882326c5636677206ed8061d0949`, the
sealed Task 23 candidate
`sha256:08cfba24762a7dd3a7a631591f6a0bd0dbdd2a3e84d6942c0a5a9d652ba048d7`, the outer operator
ran, at the repository root with Python 3.14.7 and the pinned Rust 1.88.0 through
`rustup run 1.88.0`:

```sh
rustup run 1.88.0 python3.14 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); suite=unittest.defaultTestLoader.loadTestsFromName("tests.test_nightly_build.CargoRewrite"); result=unittest.TextTestRunner(verbosity=2).run(suite); raise SystemExit(not result.wasSuccessful())'
```

`tests.test_nightly_build.CargoRewrite`: 3 tests, `OK`, 6.378 seconds, exit 0. That is the
operator's observation on `b909`, not a run of any later candidate. CI run 41050608459 on
`fb3b632d` (above) is the predecessor failure CI706. CI707, GitHub run 41057018586 on `b909`,
then passed all 11 jobs, among them 1268 Python performance tests and the selected Rust
performance tests. Neither CI run is a run of the candidate below, and no CI run or VM execution
of that candidate is claimed here.

## Diagnostic lanes need two complete sides and the aggregate's selection (2026-10-30)

**Findings.** The required correctness review of `b909` found that a comparison's trace lane
was complete when any trace of a selected case had no omission: a one-sided trace, a trace whose
other side was omitted, a `partial` or a setup-only `region_missing` trace (never complete under
ADR-0176) completed it. It also found that each profile's coverage was checked against the
profile's own selection, so otherwise valid trace and CPU children of a parent-plan case the
aggregate verdict did not select, with their sealed results, states and the export manifest
resealed, completed both lanes while the report's diagnostic selection named the actual
regression.

**Change.** `comparison.status`, which `comparison-report` and `comparison-verify` both derive
the report through, recomputes the selection record from the aggregate's own verdict
(`diagnostics.select` with the frozen configuration's `max_cases` and that verdict's digest) and
requires each profile's selection to be exactly it: mode, limit, parent verdict digest, cases
with their reasons, and the cases left over the limit. Each expected case needs, for the trace
lane, one baseline and one candidate trace without omission whose region coverage is
`complete`, and for the CPU lane a differential flamegraph. A profile whose request's parent
receipt is not the aggregate's (its comparison, declaration digest, result, plan and verdict
hashes) is refused as not the comparison's member. Each failure keeps its lane `failed` with a
reason naming the case, the side and what the trace holds; the evidence stays bundled, and an
honest incomplete bundle verifies as incomplete. Standalone profiles, the `not_applicable` lane
of a verdict with nothing to diagnose, measurement, capture budgets and contracts do not change.

**Tests.** `tests.test_comparison.Comparison.test_full_comparison_partitions_aggregates_diagnoses_reports_and_exports`
no longer injects profile usability: its profile stand-in writes a contract-valid
`profiles.json` of the selection the frozen parent verdict derives, and the honest complete
bundle verifies complete through `cli.py executor comparison-verify`. For a missing candidate
trace, an omitted one, a `partial` one and a `region_missing` one, the journal's report leaves
the trace lane failed with its reason, the complete claim over the resealed evidence is
refused, and the same evidence reported as it is verifies incomplete. Children of an unselected
case with every receipt resealed fail `comparison-verify` (exit 4) naming both lanes'
selections, and a profile whose parent receipt names another aggregate result is refused. With
each of the three checks removed in turn, the test fails at its attack.

**Focused validation of this candidate.** Run by this repair Task's implementer on its
candidate, the source tree of `b909` with this change, at the repository root, offline, with
Python 3.14.7 and Rust 1.88.0 (`cargo 1.88.0 (873a06493 2025-05-10)`) through
`rustup run 1.88.0`, on 2026-10-30 at 14:10 to 14:15 UTC. The commands are the one above with
the named tests in place of `tests.test_nightly_build.CargoRewrite`:

- `tests.test_nightly_build.CargoRewrite`: 3 tests, `OK`, 0.928 seconds, exit 0.
- `tests.test_executor.Journal.test_hosted_declared_windows_refuse_before_any_original_or_transport`:
  1 test, `OK`, 0.242 seconds, exit 0.
- `tests.test_comparison` and `tests.test_report_diagnostics`: 46 tests, `OK`, 233.742 seconds
  on a shared, loaded host, exit 0.

These are this candidate's focused runs, recorded by its native Task record; no commit SHA, CI
run or VM execution of it exists yet, and none is claimed. The earlier focused run of the
hosted-windows test, 1 of 1 in 0.676 seconds, belongs to Task 22's candidate. The full 33-case
`6d8208d` comparison and its offline verification remain that producer's evidence; the
integration on Claw and the final head's CI are the outer operator's, after this change is
adopted.

## CPU lanes need admitted captures; verified reports derive the aggregate (2026-10-30)

**Findings.** Whole-PR verification 08 left two major correctness findings open on
`perflab/comparison.py`. First, for each case the aggregate selected, the CPU lane accepted a
differential render without baseline and candidate captures, and offline admission
(`report.admit_children`) neither requires captures nor binds a render to them. A copied
bundle could keep its valid SVGs, remove the captures, reseal the child and the original, and
still complete the lane. Second, `status` took the aggregate's `result` and `coverage` as the
core lane's completeness. `comparison-report` and `comparison-export` verified each original
but never derived those fields again, as `verify_export` did through `aggregate_claims`. So an
incomplete aggregate edited in the journal before the diagnostic lanes were submitted produced
matching child parent receipts and a locally complete report, which the copied-bundle
verifier contradicted.

**Change.** For each case the aggregate selects, the CPU lane now needs a usable baseline
capture and a usable candidate capture. A usable capture has no omission, at least the frozen
registry's `capture.minimum_samples` (unchanged at 200) and its own `collapsed.folded`
artifact. Both directions of the difference must name exactly those two captures' collapsed
stacks as inputs: forward is baseline then candidate, reverse is the opposite. This is the
input `diagnostics.Profile.render` hashes into each render. The minimum is read from the
installed producer's `fixtures/performance/diagnostics.toml`, and only when its bytes are the
registry the profile names (`registry_sha256`). Otherwise the lane fails, naming both digests.
The loss rule is unchanged: the native contract's (decoded counts, or unknown with a reason).
A decoded loss is reported, never a usability threshold, as in the producer. Standalone
profiles, the producer, the selection and parent checks, the trace lane, sampling, budgets,
schemas and persisted shapes do not change.

`status` with `verify` (`comparison-report`, `comparison-export`, and `comparison-verify`
through `Bundled`) now runs `aggregate_claims` over the members it admitted, in a journal and
a bundle alike. That covers partition entries against their originals' states and sealed
results, inclusion only of admitted members, the run's shards byte for byte those of the
included partitions (a bundle's manifest hashes, a journal's files read within the export's
bounds), the first partition's build, the run evaluated again to its verdict
(`_same_verdict`), and the result, coverage and regressions read from that verdict. An
aggregate that judged nothing must claim no build, result, coverage or regressions. A claim
that does not derive leaves the core lane `failed` with `core aggregate refused: …`, fails
every diagnostic lane under it, and makes the report's verification `refused` with an empty
core verdict. The reason uses journal-relative paths, so the journal's report and its
bundle's derivation are the same document. `comparison-verify` no longer refuses such a
bundle outright: it derives the same refused, incomplete report and refuses any other claim.
`comparison-status` (`verify` absent) still reads cheaply and verifies nothing.

**Tests.** In `tests.test_comparison`:

- `stand_in_profiles` now writes, for each selected CPU case, a contract-valid first perf
  capture per side at exactly the registry minimum, with the renders bound to their collapsed
  stacks and the installed registry's digest. The honest complete bundle still verifies
  complete through `cli.py executor comparison-verify`.
- `Comparison.test_full_comparison_partitions_aggregates_diagnoses_reports_and_exports`, new
  helper `assert_cpu_lane_needs_admitted_two_sided_captures`: the selected case's captures
  removed, the candidate missing, the candidate omitted (`too_few_samples`), the candidate at
  199 samples, and the render's inputs substituted, each with the render surviving. In each
  case the journal's report leaves the CPU lane failed with its reason. The complete claim over
  the resealed evidence (original, result, state, report and manifest) is refused: through
  `comparison-verify` (exit 4) for removed captures, through `verify_export` for the rest.
  With the captures removed and the report as derived, the bundle verifies `incomplete`.
- The same test, journal side: an aggregate whose claims alone are injected as judged whole is
  `incomplete`, `refused`, core `failed`, core verdict empty. Only with the verdict injected
  as well (`overview._core`, as the offline derivation already did) is it complete.
- `Comparison.test_a_locally_altered_aggregate_never_reports_complete`: the real
  `aggregate.json` is rewritten as judged whole before `advance` submits the lanes. Both
  profiles' parent receipts name the altered bytes. `cli.py executor comparison-report` and
  `comparison-export` exit 4 with core, trace and CPU lanes failed and verification
  `refused`, and the exported bundle's offline derivation equals the journal's report.
- `CpuEvidence.test_the_declared_dwarf_capture_renders_after_an_omitted_first_attempt` and
  `CpuEvidence.test_a_registry_that_is_not_the_installed_producers_proves_no_sample_minimum`:
  the producer's DWARF second attempt is usable after an omitted first; a render of the
  omitted attempt is not; an unknown or unreadable registry fails the lane.

With the `_cpu_case` check replaced by the earlier render-only check, the full test fails at
the removed-captures variant (`('complete', 'complete', 'complete') != ('incomplete',
'complete', 'failed')`). With the `aggregate_claims` call in `status` removed, the full test
fails at the injected-claims assertion, and the local-tamper test fails at
`comparison-report` (`0 != 4`).

**Focused validation of this candidate.** Run by this repair Task's implementer on its
candidate at the repository root, offline, with Python 3.14.7, no benchmark, Docker, perf or
network, on 2026-10-30 at 15:28:59 to 15:33:09 UTC:

```sh
python3.14 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); suite=unittest.defaultTestLoader.loadTestsFromNames(["tests.test_comparison","tests.test_report_diagnostics"]); result=unittest.TextTestRunner(verbosity=1).run(suite); raise SystemExit(not result.wasSuccessful())'
```

`tests.test_comparison` and `tests.test_report_diagnostics`: 49 tests, `OK`, 249.316 seconds
on a shared host, exit 0. That is 46 tests before this change plus the 3 new ones. An earlier
run of the same selection on this candidate failed one test only because `comparison.py` was
edited while it ran. The telemetry authority digest covers `perflab/`, so the stand-in
telemetry result, produced before the edit, was refused when verified after it. Nothing was
changed in response, and the run above saw no edit. `python3.14 -I -B
scripts/performance/cli.py harness-digest --write` re-pinned the protected harness after the
last protected edit, to
`sha256:8d540a668f6376d9520af4c46f0b287f0d263f97d1b3d40fb620e807153af79c`, and `--check` exits 0.
No commit SHA, CI run or VM execution of this candidate exists yet, and none is claimed. The
full 33-case `6d8208d` comparison and its offline verification remain that producer's
evidence, and nothing here changes them. The integration running on Claw for `4caf525f`, its
final evidence and the final head's CI belong to the outer operator, after this change is
adopted.

## The aggregate names its declaration and includes every admitted partition (2026-10-30)

`aggregate_claims` checked admission only for partition entries the aggregate marked
`included`. It did not compare the aggregate's `comparison_id` or `plan_sha256` with the
declaration. So an aggregate with only those identity fields changed, or one that marked a
complete, admitted partition `refused` and dropped its shards, still derived a verified
report. The diagnostic children submitted under it carried matching parent receipts.

`aggregate_claims` now requires the aggregate's `comparison_id` and `plan_sha256` to equal the
declaration's. It derives each partition entry from the original, as `aggregate` writes it:

- The original's state must be terminal.
- A partition is included and `verified` exactly when its original is this comparison's member
  (`_binding`), complete, among the admitted originals, and, for a later partition, carries
  its declared execution window (`_window`).
- Otherwise the entry is `refused` when the original is complete and `not_complete` when it is
  not, and it is never included.

Both directions are checked: an included entry that does not derive is refused as before, and
an excluded entry that derives is refused with `the aggregate excludes partition N, whose
original … is this comparison's admitted complete member`. The run's shards are checked
against the derived inclusion. An incomplete, foreign or unadmitted partition still reports
honestly as incomplete. `comparison-status` (`verify` absent) is unchanged and verifies
nothing. Schemas, the writer, shard and verdict checks and persisted shapes do not change.

**Tests.** In `tests.test_comparison`:

- `Comparison.test_an_aggregate_of_another_identity_or_excluding_an_admitted_partition_is_refused`
  uses three fresh journals. In each, the real aggregate is rewritten before `advance` submits
  the lanes, so both profiles' parent receipts name the rewritten bytes:
  - the comparison ID alone changed;
  - the plan digest alone changed;
  - the writer run again with the admitted later partition reported refused, which drops its
    shard from the family's run and re-evaluates its verdict.

  Each time, `cli.py executor comparison-report` and `comparison-export` exit 4 with core,
  trace and CPU lanes failed, verification `refused` and the guard's reason. The exported
  bundle passes `comparison-verify` (exit 0, `incomplete`), and `verify_export` derives exactly
  the journal's report. Then the excluded partition's sealed verdict is changed, so it is
  genuinely not admitted. The same aggregate bytes then report without an aggregate refusal:
  `core partition 1 … is complete, refused`.
- `Comparison.test_failed_partition_and_lost_controller_leave_the_family_incomplete` now also
  checks that the verified report of an uncertain and a failed partition is `incomplete` and
  `verified`, with no aggregate refusal.

Each guard was mutated alone in the candidate, and the new test was run against each mutant:

- comparison-identity check removed: the comparison-ID variant fails;
- plan conjunct removed: the plan variant fails;
- exclusion and verification derivation removed, with inclusion taken from the entry as
  before: the exclusion variant fails.

Each mutant failed with `('incomplete', 'verified', 'failed', 'complete', 'complete') !=
('incomplete', 'refused', 'failed', 'failed', 'failed')`. Those runs took 12.4 s, 23.5 s and
40.3 s. The candidate was restored byte for byte, and its digest was checked.

**Focused validation of this candidate.** Run by this Task's implementer at the repository
root, offline, with Python 3.14.7, no benchmark, Docker, perf or network, on 2026-10-30 from
17:09:40 to 17:14:56 UTC:

```sh
python3.14 -I -B -c 'import sys,unittest; sys.path.insert(0,"scripts/performance"); suite=unittest.defaultTestLoader.loadTestsFromNames(["tests.test_comparison","tests.test_report_diagnostics"]); result=unittest.TextTestRunner(verbosity=1).run(suite); raise SystemExit(not result.wasSuccessful())'
```

`tests.test_comparison` and `tests.test_report_diagnostics`: 50 tests, `OK`, 315.765 seconds
on a shared host, exit 0. That is 49 tests before this change plus the new one. Alone, the new
test took 36.7 s. `python3.14 -I -B scripts/performance/cli.py harness-digest --write`
re-pinned the protected harness after the last protected edit, to
`sha256:1e472f28ae1edece3a9fec7bd899579683aca2b3524f688dbb2ed690ea6f510e`, and `--check`
exits 0. No commit SHA, CI run or VM execution of this candidate exists, and none is claimed.
The full 33-case `6d8208d` comparison and the cancelled `4caf525f` integration keep their
recorded status. This change produces no new comparison evidence.
