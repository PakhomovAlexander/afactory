# Startup projection experiment: verification note (2026-10-26)

Outcome: same-lock reuse was rejected; no runtime optimization is retained. Issue #255 remains
unresolved. See [ADR-0226](../../adr/0226-refuse-startup-projection-reuse-across-authority-callbacks.md).
This Worker note reports focused verification, not a kernel Measurement or acceptance verdict.

## Supplied prior evidence (not executed by this Worker)

The complete Claw phase diagnostic names source
`b2185fc4b6abaced9cdccca4d5a07176cae410a4`; its runtime bytes also occur in the later lab-only
compatibility repair. Each trace has 688 spans, zero dropped/unfinished spans and one measured
`probe_region`. Only that region's descendants were counted. Three disjoint projections occur:

| Case / side | Region ms | Projection spans ms | Projection share | Canonicalization share |
| --- | ---: | --- | ---: | ---: |
| medium / baseline | 1308.901465 | 485.997314, 335.661772, 320.333473 | 87.248% | 29.713% |
| medium / identical candidate | 1328.889563 | 489.721856, 336.985626, 320.008096 | 86.291% | 29.601% |
| large / baseline | 3909.164187 | 1485.328882, 956.123464, 957.950429 | 86.960% | 29.054% |
| large / identical candidate | 3713.856188 | 1387.028233, 902.971940, 915.832662 | 86.321% | 30.819% |

Phase totals are inclusive and overlap: projection and canonicalization shares must not be
added. This was shared-host attribution on diagnostic CPUs 10–11 with concurrent native
validation on 0–7, not quiet timing or a gain claim. The supplied profiles manifest SHA-256 is
`ca7268685bfcc41e58e389d29f18fb9013df5c9d21e5fe7486e58cd05f3b62a7`;
`profiles.json` SHA-256 is `85172e030e455d6dd7abbb28a8ca6f665ea837b5130789c0edc8647b5511864f`.
Raw artifacts were not independently re-admitted here.

The separate real hosted baseline had individually valid medium/large `authority_runtime_new`
cases, 20 pairs, baseline medians 1507.783204 ms and 2499.354924 ms. Its overall bootstrap was
incomplete because of the separately fixed help oracle and two inconclusive cases. These
individual cases establish no aggregate pass, and no cross-host comparison establishes a gain.

## New focused verification

The new `startup_rechecks_revision_integrity_after_the_authority_callback` integration test
uses the existing Task runtime fixture and real captured compiler. Its authority callback
finishes actual plan validation and then deterministically corrupts or removes the referenced
revision before returning. No sleep, timing threshold or budget change is involved. The
unchanged runtime refuses both changes before any invocation/Attempt. Restoring exact bytes
allows same-lease construction with no additional marker or event, and reopening projects
successfully. This is a real filesystem freshness counterexample to reusing pre-callback state;
it does not assert that ordinary authority implementations corrupt files.

`TaskAuthority::validate_plan` and applicable `authorization_current` calls occur after the
projection inside `checked_task_current`. There is no CAS immutability guarantee across them.
A shared mutex serializes one connection; another SQLite connection can still commit, including
the reserve heartbeat (ADR-0128). Neither that mutex nor a successful earlier authority check
makes a returned projection future effect authority. Existing dispatch checks and transactional
Task/Review fences must remain. This experiment does not attempt new synchronization, replace
post-callback validation, or claim an atomic database/filesystem snapshot.

Toolchains actually used: `rustc 1.88.0 (6b00bc388 2025-06-23)`,
`cargo 1.88.0 (873a06493 2025-05-10)`, Python 3.14.4. Builds use this checkout's `target/`.
The first ordinary Cargo attempt could not resolve `index.crates.io`. Available dependency
registries were copied into a private Cargo home, without credentials or host configuration:

```sh
mkdir -p target/worker-cargo-home
cp -a /var/lib/af-task-performance/home/.cargo/registry target/worker-cargo-home/
cp -a /var/lib/af-task-performance/dependencies/rust-20261024/registry/. target/worker-cargo-home/registry/
CARGO_HOME="$PWD/target/worker-cargo-home" cargo test --offline --locked -p review-pipeline --test it startup_rechecks_revision_integrity_after_the_authority_callback
```

After fixing the new test's initially missing trait method and its incorrect assumption that
construction creates execution records, the final regression passed: **1 passed, 0 failed**,
covering corruption and removal. The temporary reuse experiment below returned Cargo exit
**101**, **0 passed, 1 failed**, with `startup accepted authority changed during validation;
remove=false`. Thus the reuse loses an existing refusal. The removal lane is not reached in
that failing run. Both production files were restored in `finally`.

Additional focused checks on the restored runtime:

```sh
CARGO_HOME="$PWD/target/worker-cargo-home" cargo test --offline --locked -p review-pipeline --test it task_runtime::
CARGO_HOME="$PWD/target/worker-cargo-home" cargo test --offline --locked -p review-store --lib store::task::tests::recording::
```

Results: **45 passed, 0 failed** for the runtime group (normal execution/replay, domain
publication recovery, writer/lease fences, reservation, usage and off/content execution-marker
parity); **4 passed, 0 failed** for recording (expired recovery, exact selected output/report,
revocation, successor fencing and the in-transaction Review Round comparison). These unchanged
behavioral oracles remain intact. They are correctness checks, not performance samples.

```sh
CARGO_HOME="$PWD/target/worker-cargo-home" cargo test --offline --locked -p review-pipeline --test it -- task_campaign_review::host::integration:: task_campaign_review::host::expired_waiting::
cargo fmt --all -- --check
node /var/lib/af-task-performance/dependencies/markdownlint-0.22.1/node_modules/markdownlint-cli2/markdownlint-cli2-bin.mjs docs/adr/0226-refuse-startup-projection-reuse-across-authority-callbacks.md docs/adr/README.md docs/performance-runtime-new-investigation.md docs/performance-evidence/runtime-new-20261026/verification.md
```

Results: **14 passed, 0 failed** for Integration and expired Review recovery, including ordinary
startup refusal during the dedicated closed-Round Integration phase and the `spans` telemetry
mirror. Formatting passed (exit 0, after formatting the new test). Markdown lint passed
(**4 files, 0 errors**). The executable lint shim was not executable in this environment;
the supported Node entry point above ran the same installed markdownlint-cli2 0.22.1.

## Exact temporary experiment

Executed as `python3 target/worker-projection-experiment.py`; the complete script is below so
it is retained in this candidate's declared documentation input. It adds only a checked tuple
return and consumes it under the same reacquired lock, preserving the original plan-only API.
Its source replacements prepare the experiment; the regression's behavioral outcome decides it.
No production change from this script is part of the final candidate.

```python
from pathlib import Path
import os
import subprocess

store = Path('crates/review-store/src/store/task.rs')
runtime = Path('crates/review-pipeline/src/task.rs')
originals = {path: path.read_bytes() for path in (store, runtime)}
helper = '''    pub fn check_task_runtime_for_recording(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
    ) -> Result<(TaskProjection, ExecutionPlanV1), StoreError> {
        let _span = review_core::perf_trace::span(review_core::perf_trace::Phase::AuthorityPlanCheck);
        self.checked_task_recording(cas, lease, authority)
    }

'''
old = '''            let plan = locked
                .check_current_task_plan_for_recording(cas, &lease, authority)
                .map_err(|e| e.to_string())?;
            let projection = locked
                .task_projection(cas, lease.task_id())
                .map_err(|e| e.to_string())?
                .ok_or("Unknown Task")?;
'''
new = '''            let (projection, plan) = locked
                .check_task_runtime_for_recording(cas, &lease, authority)
                .map_err(|e| e.to_string())?;
'''
try:
    assert runtime.read_text().count(old) == 1
    store.write_text(store.read_text().replace('    fn checked_task_recording(\n', helper + '    fn checked_task_recording(\n', 1))
    runtime.write_text(runtime.read_text().replace(old, new, 1))
    env = dict(os.environ, CARGO_HOME=str(Path('target/worker-cargo-home').resolve()))
    command = ['cargo', 'test', '--offline', '--locked', '-p', 'review-pipeline', '--test', 'it',
               'startup_rechecks_revision_integrity_after_the_authority_callback']
    result = subprocess.run(command, env=env, check=False)
    print('Same-lock reuse experiment exit:', result.returncode, flush=True)
finally:
    for path, original in originals.items():
        path.write_bytes(original)
    print('Restored both production files byte-for-byte.', flush=True)
```

## Limits

No redundant projection was removed in the final candidate, so there is no reduction-count or
phase claim. No candidate trace, matched contained comparison or hosted check was run by this
Worker. Independent Terra/Astra work and the unchanged complete kernel/Markdown gate belong
to the enclosing pipeline. The coordinator's later actual profiles, matched uninstrumented
comparison under one trusted authority with unchanged cases and hosted observations are
separate acceptance-to-rollout evidence, not prerequisites for this Worker counterexample.
No event/receipt/replay oracle, protected harness or inventory, workload, seed, retry, timing
threshold, containment, telemetry payload, cohort, `.af/` authority or release policy changed.
