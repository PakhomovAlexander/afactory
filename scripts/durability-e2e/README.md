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
python3 -B "$suite/provider_contract_test.py"
python3 -B "$suite/broker_check_adapter_test.py"
python3 -B "$suite/compare_test.py"
python3 -B "$suite/worker_failover_test.py"
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

Store the controller's evidence directory under `control/`, the engine's `before.json`, `after.json`, `task-before.json` and `task-after.json` under `engine/`, and the fixture's closed `checks.sqlite` under `fixture/`. Run the evidence exporter with the pinned inputs manifest for the case.

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
