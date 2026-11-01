# Pi Provider validation record

This record separates supplied historical observations from checks of this candidate.
**Paid live smoke: not run.** No real profile credentials, OAuth exchange or paid inference
were used. The official SDK tests use synthetic credentials and mocked upstream transport.

## Historical nested-write observation: issue 494

The coordinator supplied the original raw
[issue #494](https://github.com/PakhomovAlexander/afactory/issues/494), created
**2026-11-01T16:48:47Z**, titled “Pi write tool refuses files whose parent directory does not exist”.
It records Task `pi-provider-2`, default workstream, source base `957c5c71`, candidate
`crates/review-runner-pi/src/host.mjs`, `confinedTools` / `write`.
The observation was made during implementation, before the nested-write fix, on an unfinished
candidate, not shipped behavior:

```text
nested write failed: AF file operation refused
file exists: false
```

The original repro steps were:

1. Create an empty temporary directory.
2. Construct `confinedTools(root, [root], [root])` from the candidate host module.
3. Invoke its `write` tool with
   `{path: "new/module/file.txt", content: "synthetic marker"}`.
4. Observe refusal and no file. Creating new source directories should remain confined to
   the authorized writable root, with symlink and out-of-root destinations still refused.

The issue identified validation followed by an open without creating missing parents.
This is the original recorded failing probe, supplied as Task evidence, **not** a newly executed
before-fix probe. Current automated regressions in `scripts/test-pi-host.py` exercise nested writes
through both `confinedTools` and the actual SDK session tools, preserving symlink, outside-root
and base-write refusals. Its full SDK agent turn returns a typed reply and native usage through
mocked HTTP. Reconstructing the old implementation is unnecessary and would not be historical evidence.

## Historical coordinator measurement

The coordinator actually ran `make perf-kernel-measure` in worktree `afactory-wt-pi-provider`,
HEAD **`9cfc942c50805d8421f0197583a9656bee2cbba1`**, exit **0**. This is historical source,
not this corrected candidate. The selected test was
`perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample`, covering the
measured-baseline and tampered-candidate assertions. No gate, assertion or budget was modified.

```text
SLOW [>1800.000s] af::it perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample
PASS [1886.825s] (1/1) af::it perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample
Summary [1886.833s] 1 test run: 1 passed (1 slow), 3957 skipped
[perf-kernel-measure] 2111.750s, exit 0
```

The 3957 skipped tests were intentionally outside this manual command's selection, not excluded
from normal gates. Original external log: `/tmp/pi-provider-perf-kernel-measure.log`;
SHA256 `f0a70b3957d3a62d2ee1bd4b5c44c0075bada8720063ab6bd0325a1e4fc929a7`.
The run result and log digest are coordinator-supplied evidence; this Worker does not claim to
have independently read that original log.
Protected-harness digest:
`sha256:7baa6b8185b84ce0338e5bf1c12992b12b13e5dfead71fcd26707c2ef71bbfc6`.

The following measured-source SHA256 values were supplied by the coordinator and independently
checked against this candidate's unchanged files:

| File | SHA256 |
|---|---|
| `crates/af/tests/it/perf_kernel_measure.rs` | `b787c51fab11b971d3b0b5c23253fab133d9b259109f2ae6599610c9aa183de4` |
| `crates/review-runner-pi/src/host.mjs` | `47da5b3bec9ad300c7f852e0a38f20915e0d22b04ef69d7c3ef91a67ef2981cc` |
| `scripts/test-pi-host.py` | `60381406ce89d8e19c3b04dba77e7059dd64ec3e633b3dcb41c478fe0337cac2` |
| `fixtures/performance/kernel-measure/code-policy-measures.toml` | `da3c34c28d058719a6ae35f76049aae6e91abdb4d968663ae16e501b61ee2a32` |
| `fixtures/performance/kernel-measure/inventory.json` | `09496d74a63e68680d7f0d5c04a10da1b3621112db30cb0ddc1d57754f6ba9e9` |

## Corrected candidate validation

Current source binding: Task source Snapshot
`sha256:5f67eb573d50691e983184cd7d46018b4c9e0ee2852bd2f2df163337c82dceca`,
plus the changed runtime/test bytes identified below. The measurement's consumer copies crate
sources and the protected build/instrument closure; documentation-only edits are outside it.
This sandbox has no Git metadata; no candidate commit identity is invented.
Runtime and regression SHA256 bindings (unchanged during these checks):

| File | SHA256 |
|---|---|
| `crates/af/src/providers/pi.rs` | `c962cf340182f7efa715ca95150ff03ec706c558cd8563237464163160539194` |
| `crates/af/tests/it/provider_onboarding.rs` | `51c6fdd037c468b192ea13b3009ab03e8825d1207887d110daaee6cbed59e29c` |
| `crates/review-runner-pi/src/tests.rs` | `a45f7b44a9f7e5fbc03b60c9f3c614056c3862bc38c89a9d85221b016391fd25` |

On 2026-11-01 the following selected nextest run passed under pinned Rust 1.88.0:

```sh
cargo nextest run --workspace --locked --offline --profile ci -E \
  'package(review-runner-pi) | (binary_id(af::it) & (test(/^provider_onboarding::/) | test(/pi_registration_requires/) | test(/pi_task_admission/) | test(/pi_doctor_and_review/))) | (binary_id(af::bin/af) & test(/^providers::/))'
```

Result: `Summary [29.802s] 203 tests run: 203 passed, 1720 skipped`.
The skipped inventory is outside this selection, not removed from normal gates.
Before the setup fixes, the two new setup regressions ran against the original `pi.rs`
(SHA256 `2ebd1b5cefe204dcac289d3180c3852a8236ba4216d5d04e347153eb618a76b1`):
`Summary [2.286s] 2 tests run: 0 passed, 2 failed, 960 skipped`.
Managed discovery reported the missing installation at the final valid setup; failed setup
lacked the expected stderr detail. These are newly executed #511/#513 reproductions, distinct
from the historical #494 observation above.

The initial broad selection exposed that the cancellation fixture's stdin wait immediately
saw EOF; it completed before readiness (`202 passed, 1 failed`). The fixture now waits for
supervisor cancellation, preserving observed charge 9 and the owned-leader reaping assertion.
The corrected selection above passes both cancellation and early-failure regressions.

`cargo fmt --all`, `cargo clippy --workspace --all-targets --offline -- -D warnings`,
`python3 -I scripts/test-classes.py audit` and protected-harness `harness-digest --check` passed.
The current protected-harness digest equals the historical digest above; no protected input
or generated fixture needed changing. Both script modes passed all three tests:
`AF_PI_TEST_HERMETIC=1 python3 -I scripts/test-pi-host.py` and
`AF_PI_TEST_SDK=/Users/apakhomov/.pi/agent/install/releases/1.4.0/node_modules/@earendil-works/pi-coding-agent/dist/index.js python3 -I scripts/test-pi-host.py`.
The latter printed official SDK construction/tools and full mocked agent-turn success; neither
used a real profile or service. Markdownlint 0.22.1: **not run**, package absent from inspected
npm caches (`npx --offline` returned `ENOTCACHED`); no network installation was attempted.
The configured fast local gate remains the kernel's post-seal check, not a Worker-run claim.

### Fresh manual measurement of the corrected source

This Worker ran `make perf-kernel-measure` on the source binding above on **2026-11-01 UTC**,
with Rust 1.88.0, the readable Cargo registry cache, `TMPDIR=/private/tmp` and
`CARGO_NET_OFFLINE=true`. It completed by **21:02:05Z**, exit **0**:

```text
PASS [985.178s] (1/1) af::it perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample
Summary [985.186s] 1 test run: 1 passed (1 slow), 3960 skipped
[perf-kernel-measure] 990.308s, exit 0
```

Exact selected filter (the existing Make target):
`test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)`, with
`--workspace --locked --profile ci --run-ignored only`. Both the measured-baseline and
tampered-candidate assertions passed. The 3960 skipped tests are outside this manual selection.
No boundary, assertion, budget or profile was weakened. This is a current execution, separately
from the historical coordinator result; neither is a product performance-improvement claim.

Actual Worker log: `/tmp/pi-managed-perf-kernel-measure.log`, SHA256
`329b1e777ce4fdf76619d32d7dce903274bbbed1f794d50c0274e91f4a3db49c`.
The 203-test selected-run log `/tmp/pi-targeted.log` has SHA256
`636db109516aeb0bd81cb1e7106801225f4589209e41aa07286a7dda303f6282`.
All eight file hashes in this record still match; the protected-harness digest remains
`sha256:7baa6b8185b84ce0338e5bf1c12992b12b13e5dfead71fcd26707c2ef71bbfc6`.
