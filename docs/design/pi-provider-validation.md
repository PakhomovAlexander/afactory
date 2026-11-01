# Pi Provider validation record

This record separates supplied historical observations, previous-Task checks and fresh checks
of the present source. Historical/input Snapshot IDs below are not final candidate identities.
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
checked by the previous Worker against its then-current files. These are historical byte
bindings; changed host/protocol/test files below no longer claim these hashes:

| File | SHA256 |
|---|---|
| `crates/af/tests/it/perf_kernel_measure.rs` | `b787c51fab11b971d3b0b5c23253fab133d9b259109f2ae6599610c9aa183de4` |
| `crates/review-runner-pi/src/host.mjs` | `47da5b3bec9ad300c7f852e0a38f20915e0d22b04ef69d7c3ef91a67ef2981cc` |
| `scripts/test-pi-host.py` | `60381406ce89d8e19c3b04dba77e7059dd64ec3e633b3dcb41c478fe0337cac2` |
| `fixtures/performance/kernel-measure/code-policy-measures.toml` | `da3c34c28d058719a6ae35f76049aae6e91abdb4d968663ae16e501b61ee2a32` |
| `fixtures/performance/kernel-measure/inventory.json` | `09496d74a63e68680d7f0d5c04a10da1b3621112db30cb0ddc1d57754f6ba9e9` |

## Historical previous-Task validation

The previous Worker's **input** source Snapshot was
`sha256:5f67eb573d50691e983184cd7d46018b4c9e0ee2852bd2f2df163337c82dceca`,
plus the changed runtime/test bytes identified below. This historical input Snapshot was not
the previous Worker's corrected head or final candidate. Its consumer copied crate sources and
the protected build/instrument closure. That sandbox had no Git metadata; no candidate commit
identity was invented. The following historical runtime and regression hashes were unchanged
during that Worker's checks, but are not claims about the present modified source:

| File | SHA256 |
|---|---|
| `crates/af/src/providers/pi.rs` | `c962cf340182f7efa715ca95150ff03ec706c558cd8563237464163160539194` |
| `crates/af/tests/it/provider_onboarding.rs` | `51c6fdd037c468b192ea13b3009ab03e8825d1207887d110daaee6cbed59e29c` |
| `crates/review-runner-pi/src/tests.rs` | `a45f7b44a9f7e5fbc03b60c9f3c614056c3862bc38c89a9d85221b016391fd25` |

The previous Worker recorded that on 2026-11-01 the following selected nextest run passed
under pinned Rust 1.88.0:

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
lacked the expected stderr detail. Those were that previous Task's #511/#513 reproductions,
now historical and distinct from the original #494 observation above.

That Worker's initial broad selection exposed that the cancellation fixture's stdin wait immediately
saw EOF; it completed before readiness (`202 passed, 1 failed`). The corrected fixture waits for
supervisor cancellation, preserving observed charge 9 and the owned-leader reaping assertion.
The historical corrected selection above passed both cancellation and early-failure regressions.

`cargo fmt --all`, `cargo clippy --workspace --all-targets --offline -- -D warnings`,
`python3 -I scripts/test-classes.py audit` and protected-harness `harness-digest --check` passed.
The previous Worker's protected-harness digest equaled the coordinator's historical digest above; no protected input
or generated fixture needed changing. Both script modes passed all three tests:
`AF_PI_TEST_HERMETIC=1 python3 -I scripts/test-pi-host.py` and
`AF_PI_TEST_SDK=/Users/apakhomov/.pi/agent/install/releases/1.4.0/node_modules/@earendil-works/pi-coding-agent/dist/index.js python3 -I scripts/test-pi-host.py`.
The latter printed official SDK construction/tools and full mocked agent-turn success; neither
used a real profile or service. Markdownlint 0.22.1: **not run**, package absent from inspected
npm caches (`npx --offline` returned `ENOTCACHED`); no network installation was attempted.
The configured fast local gate remains the kernel's post-seal check, not a Worker-run claim.

### Historical previous-Worker manual measurement

The previous Worker ran `make perf-kernel-measure` on the source binding above on **2026-11-01 UTC**,
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
No boundary, assertion, budget or profile was weakened. This was that Worker's execution, separate from the coordinator result; both are now
historical evidence, not executions of the present modified code or performance-improvement claims.

Previous Worker log: `/tmp/pi-managed-perf-kernel-measure.log`, SHA256
`329b1e777ce4fdf76619d32d7dce903274bbbed1f794d50c0274e91f4a3db49c`.
The 203-test selected-run log `/tmp/pi-targeted.log` has SHA256
`636db109516aeb0bd81cb1e7106801225f4589209e41aa07286a7dda303f6282`.
At the end of that previous Task all eight historical file hashes matched its files; its
protected-harness digest remained
`sha256:7baa6b8185b84ce0338e5bf1c12992b12b13e5dfead71fcd26707c2ef71bbfc6`.

## Present Task: prompt ordering and uniform setup refusals

The present Task's **input** Snapshot is
`sha256:a4bd6fee6222e702cb3434b8d1cc37a2ced9493be7587001f4bf71c0b7f0aff3`.
It is the starting tree, not the final candidate Snapshot or a corrected-head identity.
There is no Git metadata here. The source-content projection below binds the measured modified
source; it is deliberately not represented as a kernel Snapshot or a commit.

### Actual before-fix probes

Before changing the runtime, three new/strengthened regressions were run against the input's
`providers/pi.rs` SHA256 `c962cf340182f7efa715ca95150ff03ec706c558cd8563237464163160539194`
and `review-runner-pi/src/lib.rs` SHA256
`9e9afd66db7d7fe4cae62f149cfa472effbd20c5200f55eef18f7085acac3b5d`.
The complete-response fixture emitted acceptance, usage, a typed final reply and settlement
before reading the private prompt; it then read that prompt and exited zero. The adapter
accepted it, failing the new refusal assertion. Early setup emitted generic `af/error@2`,
including the raw invalid ID, instead of a Pi setup report; unsupported-backend setup also
failed schema validation.

```text
Summary [2.410s] 3 tests run: 0 passed, 3 failed, 3960 skipped
```

This fresh before-fix reproduction used the initially resolved Homebrew Rust **1.94.0**;
it is not a pinned-toolchain validation pass. All required corrected-source validation and the
manual measurement use the installed pinned Rust **1.88.0**. Actual reproduction log:
`/tmp/pi-before-confirmed.log`, SHA256
`e6bd0bed31cecfe11c877177d4f5226a94924f9b978fb91a7d4e136369b67f42`.
This is separate from the supplied historical #494 observation above; no new #494 historical
run is invented.

### Reproducible measured-source projection

All source edits outside this record finished before measurement began. The projection hashes
all source-relative paths, types and content, including code, tests, fixtures, schemas,
`.af/`, `.config/`, manifests, lockfiles, toolchain pins, scripts, Makefile and documentation.
Its **only exclusions** are the root `.git` entry and subtree (Git metadata), the root `target`
entry and subtree (build/dependency/test scratch), and this record
`docs/design/pi-provider-validation.md` (to avoid self-reference). It does not exclude other
policy/configuration files, hidden entries or nested paths merely because of their names.

The algorithm recursively uses `lstat` without following links. Each entry is a four-element
JSON array: relative path, type (`directory`, `file`, `symlink`), executable-file Boolean,
and lowercase SHA256 of file bytes or raw symlink-target bytes (null for directories).
Rows sort by the exact relative path, use ASCII JSON escaping and compact separators, and each
ends in LF. SHA256 covers the domain prefix `af.pi.validation-source/1` followed by NUL and
then all rows. Timestamps and write permission bits are absent: sealing may change them.
The recipe runs from the repository root with Python's standard library only:

```sh
python3 - <<'PY'
import hashlib
import json
import os
import stat
from pathlib import Path

root = Path.cwd()
excluded = {'.git', 'target', 'docs/design/pi-provider-validation.md'}
entries = []

def visit(relative):
    with os.scandir(root / relative) as children:
        for child in children:
            path = (relative + '/' if relative else '') + child.name
            if path in excluded:
                continue
            mode = child.stat(follow_symlinks=False).st_mode
            if stat.S_ISDIR(mode):
                entries.append([path, 'directory', False, None])
                visit(path)
            elif stat.S_ISREG(mode):
                content = (root / path).read_bytes()
                entries.append([path, 'file', bool(mode & 0o111),
                                hashlib.sha256(content).hexdigest()])
            elif stat.S_ISLNK(mode):
                content = os.fsencode(os.readlink(root / path))
                entries.append([path, 'symlink', False,
                                hashlib.sha256(content).hexdigest()])
            else:
                raise ValueError('unsupported source entry: ' + path)

visit('')
entries.sort(key=lambda entry: entry[0])
projection = b'af.pi.validation-source/1\0' + b''.join(
    (json.dumps(entry, ensure_ascii=True, separators=(',', ':')) + '\n').encode('ascii')
    for entry in entries)
print('sha256:' + hashlib.sha256(projection).hexdigest())
print('entries=' + str(len(entries)) + '; files=' +
      str(sum(entry[1] == 'file' for entry in entries)))
PY
```

Projection recorded immediately before `make perf-kernel-measure`:
`sha256:ab51cdee376f4ef5b53a0b08dbd936eac25036de0dae87c55e494536dc57f6b4`,
**3770 entries, 3095 regular files**. Updating this excluded record leaves it unchanged.
The measurement log starts with this digest/count and the actual Cargo/rustc versions.
Recomputing after measurement and after the record updates produced the identical digest/count.
Only this excluded record changed after the measurement began.

### Fresh manual measurement on that projection

The present Worker actually ran `make perf-kernel-measure`, starting **2026-11-01
22:02:15 UTC**, with pinned Rust/Cargo **1.88.0**, `CARGO_NET_OFFLINE=true`,
`TMPDIR=/private/tmp`, and a private `CARGO_HOME` below this checkout's `target/` seeded only
with the locked package index entries and archives from a readable cache. No shared cache was
mutated; no real credential file was inspected or copied. `AF_PERF_KERNEL_RECEIPTS` selected
`target/pi-perf-receipts` for the existing receipt export. The command exited **0**:

```text
PASS [750.876s] (1/1) af::it perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample
Summary [750.891s] 1 test run: 1 passed (1 slow), 3965 skipped
[perf-kernel-measure] 763.052s, exit 0
```

The unchanged Make target selected exactly
`test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)`, with
`--workspace --locked --profile ci --run-ignored only`. Skips are the rest of the inventory,
not weakened or removed validation. This is the real manual kernel test, not a simulated run
or a product speed claim. Its disposable baseline's deliberate slowdown is part of the test.

Actual fresh log: `/tmp/pi-ordering-perf-kernel-measure.log`, SHA256
`6d8387aa8ab102c734cc3c9631c7c59fac70fe9e87b1e55f006cce08566703e8`.
The test exported and schema/reader-validated these `af.perf-kernel-receipts/3` documents:

| Scratch receipt | SHA256 | Actual result |
|---|---|---|
| `target/pi-perf-receipts/receipts-17031-improve.json` | `fff565f27aa0b93c9c1bf22e546187a1015db61f6c238047d078a51a92bc7554` | Baseline and candidate each passed 3 repetitions; comparison passed; Task verified |
| `target/pi-perf-receipts/receipts-17031-candidate-refusal.json` | `943bfbf28b6c30d824442fb871177e4d00982acbc2dfbc3dbb89dbd8f2c28178` | Six tampered-harness candidates refused after measured baselines; two invalid metric-contract cases also refused |

The six harness cases are `timing-boundary`, `golden-fixture`, `build-inputs`,
`case-variant-build-inputs`, `symlink-to-file` and `bytecode`: their baselines passed 3
repetitions, candidates failed on repetition 1 with command exit 2, and comparisons were
inconclusive. `wrong-unit` and `undeclared-key` failed baseline and candidate with
`unit_mismatch` and `malformed_report`, respectively. No refusal became a successful result.
Receipt-internal Snapshot IDs name the test's disposable consumer revisions, not this source's
final candidate. The durable compact evidence here is the result, exact source projection and
log/receipt hashes; scratch paths are not promised as permanent hosted artifacts.

The current generated protected-harness fixtures still match
`sha256:7baa6b8185b84ce0338e5bf1c12992b12b13e5dfead71fcd26707c2ef71bbfc6`.
`harness-digest --check` passed. No protected input, generated fixture, budget, assertion,
profile, `.af/` authority, accepted ADR 0270–0273 or `.github/` file changed.

### Corrected-source checks

With the same source projection, the present Worker ran this focused selection under pinned
Rust 1.88.0, then repeats it as the final step after completing this excluded record:

```sh
cargo nextest run --workspace --locked --offline --profile ci -E \
  'package(review-runner-pi) | (binary_id(af::it) & (test(/^provider_onboarding::/) | test(/pi_registration_requires/) | test(/pi_task_admission/) | test(/pi_doctor_and_review/))) | (binary_id(af::bin/af) & (test(/^providers::/) | test(/^shipped_schemas::/) | test(/pi_setup_guidance/))) | (binary_id(review-core::schema_parity) & test(/pi_host_and_setup_protocols/))'
```

Actual post-measurement run:

```text
Summary [28.407s] 213 tests run: 213 passed (2 leaky), 1909 skipped
```

Log `/tmp/pi-targeted-measured-source.log`, SHA256
`cf7369021564d3126f73168282c84d00cba3ad0c4975fce6b89b2189a4eaa378`.
Nextest reported `LEAK` for
`provider_onboarding::an_authenticated_context_registers_without_a_login` and
`provider_onboarding::documents_redact_identity_and_provider_output`; both are unchanged
Claude fixture tests and neither exercises Pi. No assertion failed. This records the observed
warnings, without claiming their cause or changing cleanup, timeouts or test policy.
Earlier corrected-source selections passed all 213 without those warnings; the final repetition
is also reported in the operator's Task report.

The selection includes the deterministic pre-prompt reply refusal, immediate and fragmented
legitimate replies, cumulative/partial usage, cancellation/reaping and bounded readiness,
missing authority and identity drift, exact model/effort/backend/version refusals, common
Task retries/accounting, Review/doctor and existing Claude/Codex Provider contracts.
Setup regressions check malformed managed versions, both unsafe-path CLI cases, omitted-entrypoint
success, invalid/reserved IDs, missing/unsupported backends, missing/relative auth paths,
missing SDK/launcher, failed probes, registration conflict, repeat registration and malformed
registry data. Their real stdout validates against setup v2 and their stderr stays AF-authored;
invalid identity/backend fields are null and synthetic secret markers stay absent.

`cargo fmt --all` and its final read-only `--check`,
`cargo clippy --workspace --all-targets --offline -- -D warnings`,
`python3 -I scripts/test-classes.py audit`, `harness-digest --check` and
`python3 -I scripts/changelog-notes.py --check` passed. The final clippy log
`/tmp/pi-clippy-pinned-final.log` has SHA256
`7e6add39b63ded121ab2dd66c41a3562a515c1bb7ae872c028102fdcf971d5e0`.

Both `AF_PI_TEST_HERMETIC=1 python3 -I scripts/test-pi-host.py` and the explicit official SDK
mode passed all three tests. Official SDK construction, actual confined tools, full mocked
agent turn, typed reply and native usage ran; malformed/missing prompt challenges refused
before acceptance or inference. No real profile, credential exchange or paid call was used.
Official SDK log `/tmp/pi-host-sdk-task-final.log`, SHA256
`0213fd506e0a05b47aa4099c97aa5e422dfde402b3b7c5b9a4355568c5cc21ad`;
hermetic log `/tmp/pi-host-hermetic-task-final.log`, SHA256
`6ad622ec2d2fa3a2da39fe95f287b3904b58cb62296f18eb29bede1e368095f5`.

The required `npx --offline --yes markdownlint-cli2@0.22.1` invocation could not resolve
uncached registry metadata (`ENOTCACHED`); no network install was attempted. Separately,
markdownlint-cli2 **0.22.1** was actually run directly from the readable installed npm cache,
with **0 errors** across all six changed Markdown files. All 338 local links in these docs
and the ADR index resolved. The unchanged configured full fast local gate is the kernel's
post-seal check; this Worker does not invent its future receipt or claim to have run it.
