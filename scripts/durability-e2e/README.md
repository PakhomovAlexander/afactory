# Manual AF durability E2E runbook

Manual qualification suite, design and executable contract prototype. The unit of delivery is one AF Scenario invocation. A Task, Attempt, worker session and external check each have their own identity and lifecycle. A Scenario can remain waiting on the same child across several failures and several workers. An Ant can start a Scenario; it does not define how that Scenario recovers.

## Deployment and trust

The operator starts each trial manually. The two-VM profile uses a compatible broker control API. Select a coordinator and an independently disposable worker. Place the dependency fixtures, append-only evidence collector, and judge outside both failure domains. Record machine IDs and storage placement. A worker replacement must have a different instance identity. Loss of the coordinator must preserve whichever storage the engine normally promises to preserve. Run fixture traffic over an isolated route and retain management access. Use a bounded volume for disk trials and witness a failed system write there.

For broker runs, fetch the live guide using your deployment's pinned controller SSH helper. Use running AF-profile allocations with available capacity. The current API provisions plain Debian and supports job submission only on existing AF-profile allocations. Journal allocation requests before submission, label resources by trial, and reconcile ambiguous replies. Make bounded jobs and their retained evidence discoverable by trial ID. Release an allocation only on a separate owner request naming its ID. Missing independent placement or durable storage is unsupported.

An engine adapter installs the pinned build, starts its ordinary services, submits one Scenario, exports AF evidence, and maps logical crash targets to the actual processes. Restart only the service affected by the test. No harness call may replan, resubmit, resume a Task, manufacture a wake, or repair receipts. Record each actor and management command. AF's normal background recovery is part of the implementation under test. A local development topology is useful for fixture work; only witnessed distinct allocations qualify as distributed results.

## Executable contract

cases.json defines required milestones and causal edges. An arrow means the later fact must have the earlier fact in its causal ancestry. Workers need no synchronized wall clocks. Observations carry a unique ID, named fact, parent observation IDs and a reference to source evidence. The prototype checker consumes a completed trace. Production collection must bind observations to their run, subject, evidence digest, source sequence and authenticated producer. Gaps, incomplete exports and inaccessible evidence prevent a pass. A poll that jumps between two states cannot prove the intermediate transition.

Use native AF Run statuses ready, running, waiting, succeeded, failed and cancelled and its decision, condition, reconcile and task wait types in adapter output. Keep Task outcomes and Attempt events in a separate stream. The tests below specify required semantic transitions. An adapter may map multiple internal transitions onto one public milestone. It must explain the mapping and link a durable receipt. A normalized fact such as budget.exhausted does not assert that AF currently exposes a resumable budget state.

Always judge the complete history and final external ledger. Acceptance requires the exact candidate, every required gate and evaluator receipt. Terminal Runs are immutable. Stale writers cannot advance the Run. Pins, grants, reservations and settled charges survive restart. An ambiguous charge retains its exposure. Count logical external effects separately from physical requests. Every accepted child eventually settles its parent's wait and admission. Validate release by admitting a second eligible fixture objective. A trace ending early or missing a decisive boundary is inconclusive.

## Case semantics

| Case | Workflow contract and discriminator |
|---|---|
| Clean | The child produces a real patch in a temporary Git repo. Run a real local check. Verify candidate-bound acceptance, parent success and admission reuse. |
| CI reply and notification lost | Witness one check creation. Drop its response, return 503 on reads, kill the coordinator, finish CI during downtime and lose the notification. Restore reads and boot normally. Reconcile that check on that revision. Repeat with a genuine failing check. |
| Required gate fails | Run a real failing check on candidate A and preserve its exit status and evidence. In the repair-authorized variant, retain A's failure, produce B through the captured repair policy and rerun the required gate for B. Verification of A or acceptance using A's receipts fails the trial. With repair forbidden, require the declared unsuccessful outcome and release admission. |
| Task budget exhausted | Admit the first call, settle known usage, then make the remaining grant insufficient for the next reservation. Include a sibling Task with available funding. Across restart the blocked Task causes zero further paid calls under the old grant and the sibling progresses. Run a terminal budget-policy variant and a replenishment variant only where a supported explicit grant or linked continuation exists. Preserve original Task identity in historical evidence and sum spend across the objective. Repeat with unknown usage to catch accidental release of reserved exposure. |
| GitHub authentication expires | Expire a fixture credential after the child seals its candidate. Return genuine HTTP 401 until the fixture rotates a scoped credential. Witness the auth failure and preserve the check identity. The dependency must distinguish invalid credentials from 503, permission denied and a red check. On rotation, reconcile and complete within the deadline without another implementation call. Until rotation, bound attempts by the captured retry policy. Keep credential material out of traces. |
| Network and provider failures | Interrupt once before acceptance and once after acceptance. Use the provider truth ledger to distinguish retries, ambiguous charges and duplicate work. Exercise slowness, malformed usage, duplicated usage and untrusted instructions as separately witnessed cases. |
| Worker loss and disk exhaustion | Kill an executing worker, admit its replacement and deliver the stale completion. Separately force ENOSPC during artifact write and journal write, restore space, and cold boot. Previously committed evidence must remain verifiable. |
| Cancellation and pause | Cancel a child while its parent is stopped and verify eventual slot reuse. Pause during retry and reboot before releasing the hold. An Ant replay later must respect occurrence identity and owner pause. |

## Provider and reporting

Implement the mock provider as a standalone process with versioned behavior scripts, thin real runner protocol adapters, and independent data, control and private ledger endpoints. Record received, accepted, produced, charged and delivered independently. The engine receives only what the case allows it to see. Optional Codex Personal and Kimi via pi-personal trials exercise real integration; publish those separately with actual usage and limits. Deterministic comparisons use the same scripted workload. Neither profile grants publication authority.

Pin both engines, workload and fixture versions, fault schedule, resource envelope and deadlines in a trial manifest. Use collector monotonic time for recovery latency. Report recovered objectives / eligible witnessed recoverable objectives, correct policy outcomes / policy cases, safety violations, scope of unsupported cases, missed faults, timeouts, duplicate calls, repeated steps, tokens, CPU seconds and settlement gaps. A legitimate budget stop or red gate is its own successful policy case. Include raw evidence, causal trace, allocation receipts and matched per-case deltas. Compare only like profiles. Any safety violation is reported even if the run later succeeds.

First distributed qualification requires one clean control and both CI outcomes, real AF submission, provider and Checks fixtures, independent ledger, cold restart and verified job completion. Save the pre-refactor baseline before changing native recovery. Then add gate, budget, authentication, worker death and disk cases. Backend two qualifies by changing only adapter and deployment. No CI workflow or periodic monitor is part of this design.

## Local prerequisites and native smoke

Run native trials on a disposable Linux machine with Git, Python 3.11 or later (including SQLite),
and a pinned AF executable built for that machine. Disk and artifact faults also require util-linux
`unshare`, `mount`, and working unprivileged user and mount namespaces. The provider contract,
comparison tests, and native capability probe only need Python and can run without AF or a broker.

Run from the root of a clean Afactory clone. Replace the example scratch base with an operator-owned
location outside the clone on a volume with enough space. Each invocation retains a fresh trial
directory with private AF state, a test repository, and evidence. Keep all raw evidence there.

```sh
cargo build --locked --bin af
af_bin="$PWD/target/debug/af"
scratch_base=/absolute/path/to/operator-scratch
export AF_DURABILITY_SCRATCH="$(mktemp -d "$scratch_base/af-durability.XXXXXX")"
suite=scripts/durability-e2e

python3 "$suite/native_smoke.py" "$af_bin" "$PWD" green
python3 "$suite/native_smoke.py" "$af_bin" "$PWD" red
```

The smoke uses command Workers, a real local gate, independent evaluation and zero Provider tokens.
It emits the binary digest, source checkout, usage and outcomes. The red control must fail the
required gate. Each launcher exits nonzero when an invariant fails.

## Process restart and metered Provider

These commands use the same variables and private scratch directory as the smoke examples.

```sh
python3 "$suite/native_recovery.py" "$af_bin" "$PWD" green
python3 "$suite/native_recovery.py" "$af_bin" "$PWD" red
python3 "$suite/native_recovery.py" "$af_bin" "$PWD" green --fault expired-auth
python3 "$suite/native_recovery.py" "$af_bin" "$PWD" green --fault wrong-revision
python3 "$suite/native_recovery.py" "$af_bin" "$PWD" green --fault disk-full

python3 "$suite/native_budget.py" "$af_bin"
python3 "$suite/native_budget.py" "$af_bin" --fault lost-usage
python3 "$suite/native_budget.py" "$af_bin" --fault hostile-output
python3 "$suite/native_budget.py" "$af_bin" --fault malformed-usage
```

The restart trials use separate processes on one VM. `recovery-summary.json` records the terminal
outcome, state invariants, HTTP counts, elapsed recovery time and budget and Attempt evidence. The
disk case requires a real ENOSPC from the pressure writer and AF's full-Store diagnostic on a
private 4 MiB tmpfs. The mount disappears with the namespace. JSON and logs live outside the mount.

The budget case uses a private Provider registry, mock executable and separate Scenario and grant
for the funded control. The mock retains its private ledger even when the transport drops usage.
Its HTTP protocol uses `X-Fixture-Operation` and a fixture bearer token. Export the independent ledger
on the fixture host with `mock_provider.py --ledger PATH --export`.

The following checks use temporary fixtures and do not start native fault trials.

```sh
python3 -B -m unittest discover -s "$suite" -p '*_test.py'
baseline="$suite/baselines/2026-11-20"
python3 -B "$suite/compare.py" "$baseline" "$baseline"
```

The baseline self-comparison checks report pairing and retains the actual verdicts in the
[baseline report](BASELINE-20261120.md).

## Compare matched qualification reports

Run `python3 scripts/durability-e2e/compare.py BASELINE_DIR CANDIDATE_DIR` over directories containing JSON trial reports. Report schema `af-durability/trial-report@1` describes the adapter output boundary. Each report has `case`, `profile`, `engine.build`, `inputs`, `expectation`, `fault_witnessed`, `verdict`, `violations`, `metrics`, and `evidence`.

- `inputs` holds SHA-256 digests of the workload, fault and placement manifests, a clock mode, and the integer seed. Pin the manifest bytes outside each engine so both runs receive the same declared workload and fault.
- `expectation` is `control`, `recover`, or `policy`. Use `pass` or `fail` for witnessed cases, `invalid` for an unwitnessed fault or harness error, and `unsupported` for a missing profile capability.
- `metrics` can name `first_progress_s`, `terminal_s`, `outage_s`, `repeated_steps`, `additional_calls`, `simulated_tokens`, `provider_tokens`, `cpu_s`, or `unknown_settlements`. Omit unmeasured fields. `evidence` points to the inspection data and independent ledger that substantiate the verdict.
- Paired results require matching case, profile, seed, expectation, and pinned inputs. The report retains unpaired entries and mismatches, and calculates timing or cost deltas for paired passes.

The neutral comparison schema and fixture contract are exercised with `python3 scripts/durability-e2e/compare_test.py`. Native trial directories retain raw evidence. The two-VM profile exports comparison records with the shared case manifests.

For journal exhaustion, use `native_recovery.py … green --fault disk-full` on Linux with user and mount namespaces. The job uses an unprivileged 4 MiB tmpfs and requires both a real ENOSPC from the pressure writer and AF's own full-Store diagnostic. The private mount disappears with the job. Preserve the already exported JSON and logs outside it.

## Bounded load on existing AF workers

Use Python 3.11 or later on the controller and workers. Choose at least two distinct allocated,
running AF-profile VMs and an exclusive operator window. Stage the same suite on the controller
and each VM. On each VM, use a pinned executable, a fixture checkout and an existing private
scratch directory owned by the broker job user (mode `0700`). Use canonical absolute paths.
Keep scratch separate from the checkout, suite, binary and any live Store. The driver creates a
new run directory and each concurrent case uses isolated repos, Stores, home and fixture services.
The controller uses its loopback API or the configured SSH helper. Fixture workers use loopback
fixtures and synthetic credentials created inside the case.

Save the worker bindings outside Git. Each worker entry has this shape. Replace the example
paths and pins with actual values, and include one entry per allocation in the `workers` array.

```json
{
  "schema": "af-durability/load-workers@1",
  "workers": [
    {
      "worker": "r-101",
      "binary": "/srv/af/bin/af",
      "binary_sha256": "<sha256 of that worker's af executable>",
      "source": "/srv/fixtures/afactory",
      "source_commit": "<git rev-parse HEAD of that checkout>",
      "source_workload": "<load_suite.py pins digest for those fixture bytes>",
      "suite": "/srv/suite/durability-e2e",
      "scratch": "/srv/durability-private"
    }
  ]
}
```

Compute `source_workload` on the controller from the staged fixture checkout with
`python3 -B scripts/durability-e2e/load_suite.py pins /path/to/fixture-checkout`.
Each worker verifies the actual fixture bytes, binary digest, source commit and suite bytes
before admission and again at case launch. The same binary and fixture bytes must be staged on
all selected workers. The controller saves the schedule before submitting any broker jobs.
Preflight refuses active unrelated jobs and checks distinct VM boot identities, scratch ownership,
CPU affinity, available memory, disk and inodes. Reserve one CPU and 1 GiB of available RAM and
disk per slot, plus 256 MiB and 4,096 inodes per retained trial and 4,096 inodes per slot.
Filesystem namespaces for disk faults are probed on each relevant worker. A verified namespace
permission refusal or independent-writer capability refusal reports `unsupported`.

With a fresh evidence directory outside Git, start with the short cohort and repeat it against
the second pinned build using the same placement and fixture inputs.

```sh
python3 -B scripts/durability-e2e/load_suite.py run \
  --profile short --workers /private/baseline-workers.json --repeat 2 --concurrency 2 --seed 0 \
  --evidence /private/baseline-load
python3 -B scripts/durability-e2e/load_suite.py run \
  --profile short --workers /private/candidate-workers.json --repeat 2 --concurrency 2 --seed 0 \
  --evidence /private/candidate-load
python3 -B scripts/durability-e2e/load_suite.py compare \
  /private/baseline-load /private/candidate-load > /private/comparison.json
```

Add `--connect /absolute/path/to/controller-ssh-helper` off-controller. The versioned
[case manifest](manifests/load-cases.json) supplies the ordered profiles. Use `--profile full`
for qualification across all required cases, including both scheduler restart outcomes, both
ENOSPC boundaries and the independent-writer capability probe. The `optional-fixtures` profile
covers authorized gate repair, hostile output and malformed usage. The two-VM broker Checks
profile below remains a separate experiment. Other contract prototype boundaries, such as a
replenished grant, live Providers or remote stale completion, require their own adapter and
evidence. For a pinned custom cohort, pass `--manifest PATH` with the same schema and case IDs.

`report.md` summarizes verdicts and effective load. `report.json` includes evidence references,
fault receipts, native Task and Scenario outcomes, invariants, timing, simulated Provider charges
and separate real Provider token measurements. A correct terminal red gate or budget refusal can
pass. A missed fault, timeout or incomplete harness receipt is invalid and retains its phase and
diagnostic. Checks restart, expired authentication and wrong revision require both a scheduler
SIGKILL with the fixture still alive and ordered create and fault read receipts for the same
persisted check and intent in `ledger-before.json`. Their expected identity comes independently
from the native `before.json` Scenario list: the completed implementation's candidate Snapshot
and the single uncertain Checks intent named by `root/checks` reconciliation. The Python-only
`recovery_evidence.py` validator requires the complete native list, Run, root Frame, both Progress
records, pin, limits, Waiting and accepted Task result for `native_recovery.configure()`'s fixed
two-step projection. It checks the pinned definition and policy shapes, recomputes native pin,
definition, intent and input digests, and binds the Task receipt and charge to the pinned target.
It accepts only matched list/Run/pin v12 with bundle/definition v11 tags, or the retained
v11 list/Run/pin with v10 bundle/definition tags. Mixed versions fail closed. It also checks
lifecycle, timestamps, safe-integer deadlines and dispatch bounds. Every pinned step timeout must
fit the definition wall limit; a Run may still select a lower wall limit than its definition.
Other Scenario projections cannot establish these fault witnesses. Missing, malformed or ambiguous before evidence is
invalid, even if a summary claims recovery succeeded. The before receipt is retained with the
ledger and crash receipt; witnessing a fault does not require an after-Scenario receipt, so failed
recovery remains distinguishable from invalid setup. No native invocation or schema package is
needed to validate the witness; execution prerequisites and offline binary pins are unchanged.

`fixtures/recovery-before-native.json` retains complete before lists and ordered ledgers from the
four fresh native Checks recovery trials supplied for this follow-up (successful and red
lost-reply, expired authentication and wrong revision). Only before receipts and ledger exports
were imported. `fixtures/recovery-before-native-v12.json` adds the same four complete receipt
pairs captured from the staged native v12 runtime, with canonical schema and native Run roundtrip
coverage. The v11 receipts remain unchanged as retained evidence for this harness only; native
runtime readers and contracts are unchanged. Repository, Task Store and adapter paths are replaced
by stable `/fixture/` paths;
the affected pin, definition, intent, input and charge identities are recomputed together with
the ledger's intent references. No credentials or original private paths are retained. Tests
remove and corrupt required native contract fields and check identity near misses through the
same witness and judge paths used by the worker. Rehashed short-wall definitions and overflowing
Progress deadlines remain invalid even with matching ledger links. A witnessed restart-red
recovery with a terminal Scenario and running Checks step remains a failure. These fixtures
establish validator correctness, not a new recovery execution or a performance measurement.

Case overlap uses complete monotonic subprocess intervals on each VM, including
failed trials. The interval ends when the subprocess returns or times out, before required
orphan-descendant cleanup; cleanup time does not inflate terminal timing or overlap. Worker CPU
covers user and system time reported for the case's reaped subprocesses and excludes preflight
and broker overhead. Missing measurements appear as null in the suite
report and are omitted from the load trial records in `trials/`. An unresolved job submission or
wait stops further submissions to that worker. A 503 acknowledgement is reconciled by job ID
without sending a second POST. Keep the retained broker receipts and raw VM directories private.

Compare uses `af-durability/load-trial@3` records and reports aggregate transitions, per-trial verdicts, input
mismatches, missing pairs and missing metrics. Matching binds the case, repeat seed, actual fixture
bytes (including the catalog generated for budget cases), ordered cohort, fault arguments and
deadline, and worker placement and concurrency. The workload fingerprint also binds all native
suite implementation bytes, including the controller (`load_suite.py`, `control.py`) and comparison
code. The producer identity includes the controller Python version and the exact SSH helper file
bytes when `--connect` is used. Stage the same complete suite on the controller and every worker.

Placement includes bounded, read-only Linux CPU vendor/family/model/stepping/features and usable
CPU topology/affinity, total memory capacity, visible cgroup v2 ancestor CPU quotas/weights,
cpusets and memory/swap constraints, and inherited process resource limits. Preflight and each
case retain compute identity; each case checks it again after execution. Process IDs, cgroup path
names, boot IDs and current free memory do not enter this stable capacity identity. Raw preflight
boot and available-capacity evidence remains retained and keeps its admission role.

Missing or incomplete compute identity on any cohort worker or trial, an unavailable producer,
or missing or invalid retained ancestry proof makes comparison `unavailable_inputs`, with no deltas. Changed
known producer or placement makes it `different_inputs`. Unknown CPU formats (currently only the
required x86 fields are recognized), legacy/hybrid cgroups, hidden ancestors, read failures or
exceeded bounds retain case outcomes and receipts but cannot prove comparable placement.
Full ancestry requires both a full cgroup v2 mount and the known initial Linux cgroup namespace
identity from `/proc/self/ns/cgroup` (`cgroup:[4026531835]`, the kernel's reserved
[`PROC_CGROUP_INIT_INO`](https://github.com/torvalds/linux/blob/v6.12/include/linux/proc_ns.h)).
An apparent mount root and membership of `/` alone cannot prove ancestry: a private cgroup
namespace can hide enclosing quotas. Equality with PID 1 is insufficient, including inside a
nested PID namespace. Missing, unreadable or unrecognized namespace evidence makes identity
unavailable; bare Linux workers and ordinary KVM guests with the recognized initial namespace
and full hierarchy remain supported. Covered or stacked mounts, submounts inside the hierarchy,
and enclosing mounts outside its parent chain cannot prove visibility, regardless of mountinfo
entry order. The configured cgroup root must resolve to itself.
The version 2 compute identity retains the initial namespace and unambiguous full-root proof;
version 3 trials retain each cohort worker's identity and the case identity, which must match
its preflight worker. Comparison validates that proof independently of fingerprints and requires
equivalent complete evidence on both sides. The wire schemas live in
`../../schemas/durability-compute-identity-v2.json` and `manifests/load-trial-v3.schema.json`.
A disabled cgroup controller is recorded as inapplicable only when the parent controller inventory
proves it; a missing enabled control makes the identity unavailable. The load schedule, preflight,
run report and comparison remain version 2. Saved version 2 load trials remain readable with
outcomes, raw evidence references and metrics intact, but always return `unavailable_inputs`
without deltas: their completeness flag and fingerprints cannot prove ancestry. No proof is
inferred or migrated from an older record. The separate neutral adapter protocol
remains `af-durability/trial-report@1`.

Use measurements from paired passes and check
the observed overlap and measurement coverage before interpreting timing or throughput changes.

## Two-VM profile through the control API

The control host runs the trial driver as a finite detached process. AF runs on one existing AF worker and the Checks ledger lives on another. The driver uses the authenticated control API to relay the check request and final receipt. The engine receives a durable pending response until receipt delivery. Separate loopback HTTP cases exercise real socket failures and HTTP status codes. The broker profile exercises cross-VM placement, durable effects, and process recovery within the existing worker network controls.

Use existing AF-profile allocations with no active jobs. On the operator machine, run the preflight
from the repository root with your deployed SSH helper.

```sh
connect=/absolute/path/to/controller-ssh-helper
python3 "$suite/control.py" --connect "$connect" inventory
python3 "$suite/control.py" --connect "$connect" plan af-durability-e2e 4 8 40
```

For a broker smoke, use the controller client with a fresh job name and evidence path. Substitute
your worker allocation, pinned binary, checkout and private scratch path on the worker.

```sh
python3 "$suite/control.py" --connect "$connect" native-smoke --gate green \
  --work-base /absolute/engine/private-scratch \
  --evidence "$AF_DURABILITY_SCRATCH/broker-smoke" \
  r-101 durability-smoke /absolute/engine/bin/af /absolute/engine/afactory
```

For the two-VM run, make this directory available on the controller and each worker, for example by
checking out the same commit on all three. Use a callable control API on the controller's loopback
and existing AF worker job access. Run from the controller clone root with a fresh trial name, two
distinct allocation handles, and an owned private scratch directory on each worker.

```sh
python3 scripts/durability-e2e/distributed_trial.py trial-001 r-101 r-102 \
  /absolute/engine/afactory/scripts/durability-e2e/distributed_worker.py \
  /absolute/fixture/afactory/scripts/durability-e2e/distributed_worker.py \
  "$AF_DURABILITY_SCRATCH/trial-001/control" --result green \
  --source /absolute/engine/afactory --binary /absolute/engine/bin/af \
  --engine-scratch /absolute/engine/private-scratch \
  --fixture-scratch /absolute/fixture/private-scratch
```

The critical sequence is native dispatch, remote check commit, confirmed scheduler death, remote check completion, receipt delivery, and ordinary scheduler cold start. The controller supplies no Run ID or resume command. A nonzero exit from the restart job records an invariant failure when the run settled and emitted complete evidence.

Store the controller's evidence directory under `control/` and the fixture's closed
`checks.sqlite` under `fixture/`. Copy `before.json`, `after.json`, `task-before.json`,
`task-after.json` and `ant.json` from the engine trial directory (the outbox's `root`) into
`engine/`. The `ant.json` is the generated Ant definition the worker passed to
`af ant validate` and `af ant add`.

For older outbox evidence without `fixture_git_tree`, include `engine/provenance.json` with
the engine host in `host`, the trial checkout path (`<outbox root>/repo`) in `repository`,
and that checkout's recorded `HEAD^{tree}` in `fixture_git_tree`. Run the evidence exporter
with the pinned inputs manifest for the case.

```sh
python3 scripts/durability-e2e/export_distributed.py TRIAL_EVIDENCE \
  scripts/durability-e2e/manifests/broker-green.json > BASELINE_DIR/green.json
```

The exporter opens the SQLite ledger read-only and checks the remote commit identity against the candidate snapshot and both AF histories. Compare a new build's report against the prior baseline with the same manifest. The checked-in
manifests pin the historical placement and workload. Use a separate private manifest for a new
placement and keep the original manifests intact. The comparator reports different inputs when pins
change.

## Gate repair, admission and artifact faults

```sh
python3 "$suite/native_gate_repair.py" "$af_bin" "$PWD"
python3 "$suite/native_admission.py" "$af_bin" "$PWD"
python3 "$suite/native_admission.py" "$af_bin" "$PWD" --pause
python3 "$suite/native_artifact_fault.py" "$af_bin" "$PWD"
python3 "$suite/native_writer_loss.py" "$af_bin" "$PWD"
python3 "$suite/native_child_cancel.py" "$af_bin" "$PWD"
```

- Gate repair uses the checked-in implementation generator. It retains the first failed gate
  and the repaired candidate's exact check and evaluation receipts.
- Admission holds one Ant's active slot with a pending check, witnesses `skipped_busy` and verifies
  the next eligible occurrence starts a distinct Task and check. The pause survives restart.
- Artifact pressure uses a 4 MiB mount for the fixture Task CAS. It witnesses a failed CAS temporary
  write, checks all previous objects and observes progress after ordinary scheduler restart.
- Writer loss verifies and kills the fixture command Worker's exact AF Writer ancestor. The
  scheduler recovers through the production lease. The trace records any attempted orphan reply.
- Child cancellation retains the exact process-scoped cancel receipt and requires replay to
  honor it as a child-level stop.

Use `--clean` with the two-VM driver for the undisturbed broker control.

Supply `--trace TRACE_FILE` to the distributed exporter to write the neutral sequence with verified evidence digests. Check the exported sequence separately with `check_trace.py CASE TRACE_FILE --evidence-root TRIAL_EVIDENCE`. A transition pass and a terminal invariant failure can appear together.

## Portable Worker replacement contract

```sh
python3 scripts/durability-e2e/worker_failover.py ENGINE_ADAPTER \
  scripts/durability-e2e/manifests/worker-failover.json TRIAL_EVIDENCE
```

The driver sends JSON to an executable adapter on stdin and receives JSON on stdout. Every call carries the pinned trial inputs and the private evidence directory. The adapter surface is:

| Action | Evidence |
|---|---|
| `capabilities` | Return the engine build, supported capabilities, and provenance. The independent Worker profile requires remote Worker placement/replacement and a stale-completion delivery path. |
| `start` | Submit an authorized Scenario through the engine's ordinary entry point and return its stable identity. |
| `observe` | Export an append-only causal trace with evidence file and SHA-256 digest for every observation. Worker facts carry the allocation and Attempt identity. |
| `isolate_worker` | Apply the loss to the original fixture-owned Worker. The separate Worker-loss observation proves the fault fired. |
| `deliver_stale` | Release the original buffered completion after a different allocation verifies its replacement. Preserve the original Attempt identity and independent usage evidence. |

The engine owns fencing, replacement, acceptance, and release of admission. The test adapter owns fault injection and extraction of durable evidence. Keep the final check pending until the stale delivery has been witnessed. Assertions require all milestones including late usage reconciliation and admission reuse. An unsupported capability exits 3 before any Scenario starts. The native baseline demonstrates that path with `native_worker_capabilities.py` and includes its result in the comparison cohort. The driver tests exercise the phase ordering, the same-allocation rejection, and the unsupported outcome.

Run the native capability probe from a clean clone with the same scratch variable.

```sh
python3 scripts/durability-e2e/worker_failover.py \
  scripts/durability-e2e/native_worker_capabilities.py \
  scripts/durability-e2e/manifests/worker-failover.json \
  "$AF_DURABILITY_SCRATCH/native-capability"
```

This exits 3 and writes the capabilities and unsupported report. The live probe exercises the
adapter contract without inspecting a binary, so it reports the build as `unprobed`.
The [retained unsupported result](baselines/2026-11-20/worker-failover.json) records the historical
build digest.
