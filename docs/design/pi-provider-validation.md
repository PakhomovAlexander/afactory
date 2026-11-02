# Pi Provider validation record

All measurements in this record are historical and bind only their recorded source projections.
The “fresh”, “current” and “this Task” wording in retained observations and quoted command output
refers to the stated earlier Task, never the present candidate. Historical/input Snapshot IDs
are not final candidate identities. The historical evidence contract below and the combined RC5
correction at the end describe today's explicit verification interface; no measurement of the
combined RC5 source is claimed.
**Paid live smoke: not run.** No real profile credentials, OAuth exchange or paid inference
were used. The official SDK tests use synthetic credentials and mocked upstream transport.

## Historical evidence contract (#551)

Historical Pi measurements are source-bound evidence, not a forever-current release gate.
[ADR-2026-11-02](../adr/2026-11-02-verify-pi-measurements-as-historical-evidence.md)
corrects the former #534 requirement that normal functional CI compare one past measurement
with every future checkout. A fast release does not mandate a fresh Pi measurement. Functional,
e2e and uix gates, architecture execution, archive smoke, signatures and exact-SHA release
selection remain required. A historical pass says nothing about the combined RC5 source.

`scripts/pi-validation.py` retains the exact `af.pi.validation-source/1` algorithm above:
only root `.git`, root `target` and this record are excluded. Claim data cannot select
exclusions. Symlink targets are hashed as bytes, never followed. The helper is deterministic,
uses only Python's standard library and never writes evidence or runs measurement.

Each historical JSON claim retains its unchanged `af/pi-validation-claim@1` shape, checked
against [`pi-validation-claim-v1.json`](../../schemas/pi-validation-claim-v1.json).
Blocks use the neutral versioned `af-pi-validation-claim:v1` delimiter; none is marked current.
Select one by its recorded `source_sha256` using `--claim-source`. Unsupported versions,
duplicate source claims or fields, missing/extra fields and malformed blocks refuse.
The Rust parity test `pi_validation_historical_claim_is_versioned_and_closed` checks the same
schema through the real JSON Schema validator. No reader or fallback for an older shape is added.

```sh
# Projection alone makes no measurement claim; it defaults to this checkout.
python3 -I -B scripts/pi-validation.py --projection
# Supply the historical source tree and its actual retained log explicitly:
python3 -I -B scripts/pi-validation.py --check \
  --root /path/to/historical/catalog-source \
  --claim-source sha256:7f9261ba019aa397fb84cae3673d0e04d3e3ca0e3d11afc75e1575dcce16d8a3 \
  --measurement-log /path/to/adr-confinement-perf-kernel-measure.log
```

`--check` requires explicit `--root` and `--claim-source`; it never assumes today's source is
measured. The record defaults to this document; `--record` can select another historical record
or a controlled fixture. Digest and entry/file counts must exactly match the selected source.
Without `--measurement-log`, the output says the log was not supplied or read; only source
matching has been checked. With it, the actual log SHA256, exact source line, command, filter,
Make exit and sole nextest summary must also match. Missing or tampered logs refuse.
The log's binding lines use canonical `--projection` output, `command:`, `filter:` and
`make exit:`; its original nextest summary is unmodified.

Normal functional CI uses `scripts/test-pi-host.py ValidationTests`: controlled private source
and synthetic log fixtures exercise parser, explicit selection, source mutation/stale counts,
fixed exclusions, independent projection rows, Unicode, symlinks, executable bits and all log
bindings. Synthetic logs test verification; they are never measurement results. Parsing the
retained records does not compare them with the future checkout. The future-source regression
proves unrelated source changes require no performance work, while explicit verification of a
wrong source still refuses. The historical observations below describe commands and output
from their original Tasks; old implicit `--check` invocations are no longer supported.

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

## Historical preceding Task: prompt ordering and uniform setup refusals

That preceding Task's **input** Snapshot was
`sha256:a4bd6fee6222e702cb3434b8d1cc37a2ced9493be7587001f4bf71c0b7f0aff3`.
It is the starting tree, not the final candidate Snapshot or a corrected-head identity.
That sandbox had no Git metadata. The source-content projection below bound that Task's
measured modified source; it is not a kernel Snapshot, commit or the current candidate identity.

### Actual before-fix probes

Before changing the runtime, three new/strengthened regressions were run against the input's
`providers/pi.rs` SHA256 `c962cf340182f7efa715ca95150ff03ec706c558cd8563237464163160539194`
and `review-runner-pi/src/lib.rs` SHA256
`9e9afd66db7d7fe4cae62f149cfa472effbd20c5200f55eef18f7085acac3b5d`.
The complete-response fixture was written to emit acceptance, usage, a typed final reply and
settlement before reading the private prompt, then read it and exit zero. That regression
asserted only the adapter result; it did not observe the prompt-read marker or child exit.
The adapter accepted the response, failing its refusal assertion. This historical test therefore
did not independently prove those last child events; the current ordered-transport regression
below supplies that missing evidence. Early setup emitted generic `af/error@2`,
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

### Historical manual measurement on that projection

The preceding Worker recorded running `make perf-kernel-measure`, starting **2026-11-01
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

That source's generated protected-harness fixtures matched
`sha256:7baa6b8185b84ce0338e5bf1c12992b12b13e5dfead71fcd26707c2ef71bbfc6`.
`harness-digest --check` passed. No protected input, generated fixture, budget, assertion,
profile, `.af/` authority, accepted ADR 0270–0273 or `.github/` file changed.

### Historical corrected-source checks

With that historical source projection, the preceding Worker recorded this focused selection
under pinned Rust 1.88.0:

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

## Historical pre-integration Task: executable boundary, accounting and ordering

The pre-integration Worker's input Snapshot was
`sha256:8edb684e28d142b1ccefed34eaaf74ae9f2f946e76d4e26f3a934bfd9d75f9c8`.
It was the input, **not** the corrected candidate Snapshot. That sandbox had neither Git metadata
nor `../base`; the supplied PR head `5851cc94` and main `9ea` were Task provenance, not locally
verified commit identities. Its observations below are historical: main subsequently advanced
and the merged source has a different content projection. The feature was assessed against the
complete requirements and available implementation/tests; no unavailable main diff or live-service
execution is claimed.
Work began at **2026-11-01 23:06:49 UTC** (02:06:49 MSK on November 2).

### Historical pre-integration before-fix evidence

The new boundary, accounting and title regressions first ran against the unchanged input runtime
under pinned Rust 1.88.0. The result was:

```text
Summary [3.104s] 16 tests run: 13 passed, 3 failed, 1916 skipped
```

The failures were the repository Node regression (the expected path refusal was absent),
`settlement_requires_every_observed_inference_complete_without_inventing_usage`
(`no-usage`: reply success was true), and
`schema_listing_names_provider_status_v5_and_its_sdk_host_scope`
(actual title `Provider status@4`). The new ordered-transport test passed against the existing
production nonce validation: #521 needed stronger evidence, not a relaxed cancellation path.
Log `/tmp/pi-gaps-before.log`, SHA256
`5649bd947069d6b96c629c94d074a1f434e188bef67154e79e43e977aef28bb6`.

A second precise #523 probe restored only the input `providers/pi.rs` bytes, SHA256
`70f25970028995531d7d2497433b051d853c27c4ef84c3f6e1f39d0a9d9219d3`,
while keeping the explicit fixture launcher and asserting the marker immediately after the CLI
returned. It observed **`executed repository Node for git-directory`**, failing as intended:

```text
Summary [2.430s] 1 test run: 0 passed, 1 failed, 963 skipped
```

Log `/tmp/pi-node-before-marker.log`, SHA256
`ae016cb96375a535d0e05ff5e63d95b3dc14c694a222879cceab9e4f756e653b`.
The corrected runtime was restored before validation and measurement. These are fresh probes
of this Task's input, separate from the retained historical #494 observation.

### Corrected behavior and feature assessment

- #523: SDK and Node resolution use the same ancestor checks. A `.git` directory, `.git`
  worktree marker file or `.af` directory refuses before spawn, including absolute PATH entries
  above nested CWD. `pi_node_resolution_and_setup_refuse_repository_owned_path_before_spawn`
  checks token-free status/Node resolution, setup JSON/stderr and an unexecuted marker trap
  for all three markers; a legitimate external fixture launcher succeeds from the same CWD.
- #524: `State::record` refuses settlement without observed inference 1 or with any unfinished
  inference. Existing usage admission enforces sequential identities. The faithful process
  fixture checks no usage, incomplete-only usage and incomplete inference 1 followed by complete
  inference 2. Refusals preserve unknown usage or the observed charges **9** and **18**.
  A complete snapshot with all counters unknown still permits the reply and retains unknown
  accounting; immediate legitimate replies and cumulative updates remain covered.
- #521: `observed_early_reply_then_exact_prompt_read_and_zero_exit_is_rejected_on_replay`
  records all six wire records through settlement **before** sending the prompt, checks the
  typed early reply, then checks exact Unicode prompt bytes in the read marker and actual zero
  child exit. The real shared supervisor bounds child, read and join lifetimes; the writer's
  channel waits share one deadline and disconnect on reader failure. Production `State`
  rejects replay at the observed pre-delivery boundary and also rejects the stale challenge
  after delivery. `adapter_refuses_a_complete_reply_emitted_before_prompt_consumption` and
  `supervised_host_partial_jsonl_unicode_and_immediate_terminal_settlement` remain separate.
- #525: status v5's shipped title is `Provider status@5`. Metadata describes official CLI
  providers and the Pi SDK host, including Pi's token-free unavailable/unsupported states.
  The schema-list test checks the actual listing title and Pi scope. The real CLI result in
  `pi_setup_status_and_exact_model_catalog_are_token_free_and_secret_free` validates against
  v5 and explicitly asserts Pi kind/authentication and document version. No wire shape changed.

The original feature's remaining deliverables were already present and remain exercised:

| Requirement area | Implementation and exercised evidence |
|---|---|
| Explicit native registration, binding, exact model/effort, doctor and Review | `providers/pi.rs`, `providers/task.rs`; `pi_registration_requires_explicit_authority_before_starting_or_changing_a_profile`, `pi_doctor_and_review_share_the_native_admission_and_readonly_sdk_host`, runner drift refusals |
| Isolated per-Attempt SDK host and shared supervision | `review-runner-pi/src/lib.rs` command/invoke and `host.mjs` createSession; readonly/writable, partial JSONL, Unicode, EOF/limits, error/cancellation/reaping tests |
| Versioned authority, schemas, retries and accounting | existing plan/admission/registry/setup/host versions and `pi_host_and_setup_protocols_reject_old_shapes_and_secret_fields`; `pi_task_admission_typed_result_retry_and_account_drift_use_the_common_path`, cumulative/partial usage tests |
| Official SDK, guarded OAuth identity, confined tools, no shell or ambient resources | `host.mjs` principal/guardedCredentials/createSession/confinedTools; official and hermetic `test_confined_tools_identity_rotation_and_resource_contract`, including the mocked full agent turn |
| Nested file creation, #494, base/outside/symlink refusal | existing confined write implementation and the same real-SDK tool/agent-turn test; original historical failing probe retained above |
| Unique Pi ADR and unchanged main decisions | proposed ADR-0274 remains uniquely indexed; ADRs 0270–0273 and their index entries are byte-for-byte unchanged by this Task |
| Managed launcher discovery and safe setup failures | `pi_setup_discovers_managed_launcher_without_executing_it_and_refuses_unsafe_versions`, malformed-version unit test, early setup/refusal/schema tests |
| Bounded cancellation readiness and retained accounting | existing `cancellation_during_inference_reaps_the_process_and_keeps_usage`, `cancellation_readiness_observes_early_failure_before_accept_or_pid` |

The fixture helper `crates/af/tests/it/support/fake_models.rs` now writes an explicit, quoted
Node launcher rather than symlinking to the host's Node path. The first corrected-source run
had six Pi integration failures because Homebrew's real executable is beneath
`/opt/homebrew/.git`: this is now a correctly refused installation under the required boundary.
No production exception was added. The synthetic SDK/auth fixture launcher separates its known
transport from that host installation layout; the marker traps still exercise production refusal.
No Linux-only helper, timeout, assertion, test profile or classification was relaxed.

### Historical pre-integration corrected-source checks

The focused command is the same full Pi/existing-Provider selection shown in the historical
section above, with `--workspace --locked --offline --profile ci`. On the corrected source:

```text
Summary [30.351s] 217 tests run: 217 passed, 1909 skipped
```

Log `/tmp/pi-corrected-targeted.log`, SHA256
`1e4e05b8cb0ce2b309bd2da94da9257a2276d936bcb3c2af1284d463929b129b`.
The four additional tests belong to existing audited functional/e2e modules. Skips are outside
this focused selection, not removed from the configured gate. There were no flaky assertion
failures or retries on this corrected-source selection.

`cargo fmt --all` and its read-only check, pinned Rust 1.88.0
`cargo clippy --workspace --all-targets --offline -- -D warnings`, classification audit,
changelog-note check and protected-harness `--write`/`--check` passed. The regenerated fixtures
were byte-identical, retaining digest
`sha256:7baa6b8185b84ce0338e5bf1c12992b12b13e5dfead71fcd26707c2ef71bbfc6`.
Clippy log `/tmp/pi-clippy-corrected.log`, SHA256
`af843ceba80e35e05b7e879ff28096f7737d4375c96d6c0be9748ac86cb66b7d`.

Both script modes passed three tests. Explicit official SDK mode printed successful real SDK
construction/tools and full agent turn with mocked transport, nested write, typed reply and
native usage. **No real OAuth profile was inspected and no paid/live smoke was run.**
Official SDK log `/tmp/pi-host-sdk.log`, SHA256
`49800c4f11119bd4812edd977950b3415057fbbcc367eedee407020c15ffa3d9`;
hermetic log `/tmp/pi-host-hermetic.log`, SHA256
`c27c8e500f989019be468b9f1cf7f08bae19a3465c0605973729edaa66560466`.

The initial build could not create rustc temporary directories under the sandbox's inherited
TMPDIR. Required checks used the documented writable `TMPDIR=/private/tmp` and a private
`CARGO_HOME` under this checkout's `target/`, seeded from readable locked caches, without
changing shared caches. `npx --offline` could not resolve uncached metadata (`ENOTCACHED`);
the exact cached **markdownlint-cli2 0.22.1 executable** was run directly, reporting zero errors
for all four changed Markdown files. No package was installed or network workaround used.
The configured fast local gate remains the kernel's post-seal action, not a Worker-run receipt.

### Historical pre-integration measured-source binding

All edits outside this excluded validation record finished before the fresh measurement started
at **2026-11-01 23:14:54 UTC**. The exact recipe in “Reproducible measured-source projection”
above, with only its three explicit exclusions, yielded:

`sha256:8d9fd457d008476b08d87d4eff00275deec93ce75f7fc5d2fa42bad397d9ad9f`

**3771 entries, 3096 regular files**. It includes the new changelog note, all source/tests,
schema metadata, docs and fixture-launcher bytes. The projection is a non-self-referential
content binding, not a Task input/final Snapshot claim. Only this record may change after
measurement starts; build/receipt scratch remains excluded under root `target/`.

### Historical pre-integration manual measurement result

The Worker actually ran `make perf-kernel-measure` from **23:14:54** to **23:27:37 UTC**
on **2026-11-01**, on the exact historical projection above. Rust/Cargo were **1.88.0**,
`CARGO_NET_OFFLINE=true`, `TMPDIR=/private/tmp`, with the same private checkout-local Cargo
home. `AF_PERF_KERNEL_RECEIPTS` selected `target/pi-terminal-perf-receipts`. Exit was **0**:

```text
PASS [758.625s] (1/1) af::it perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample
Summary [758.639s] 1 test run: 1 passed (1 slow), 3969 skipped
[perf-kernel-measure] 762.982s, exit 0
```

The unchanged Make target selected
`test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)` with
`--workspace --locked --profile ci --run-ignored only`. The skipped tests are outside this
manual selection. No budget, boundary, assertion, retry or test profile was changed.
Actual log `/tmp/pi-terminal-perf-kernel-measure.log`, SHA256
`42832d7cd072d1fdc99359d4e1611ecef5890c25e6ed35aa6c2e64379075ce43`.

The test exported these schema/reader-validated `af.perf-kernel-receipts/3` documents:

| Scratch receipt | SHA256 | Observed result |
|---|---|---|
| `target/pi-terminal-perf-receipts/receipts-10618-improve.json` | `4789352817fb126d5e38658e63982c2210af700f45b9fb555563a17f736cb0c4` | Baseline and candidate each passed three repetitions; comparison passed; Task verified |
| `target/pi-terminal-perf-receipts/receipts-10618-candidate-refusal.json` | `6aa6ca241ebc43a306bbbe36edb3b7adab5cf2a6138936b6d348b1e13c457c4b` | Six tampered candidates refused after successful measured baselines; both invalid metric-contract cases refused |

For `timing-boundary`, `golden-fixture`, `build-inputs`, `case-variant-build-inputs`,
`symlink-to-file` and `bytecode`, each baseline passed three repetitions, each candidate
failed its first repetition with command exit 2, and every comparison was inconclusive.
`wrong-unit` and `undeclared-key` failed both baseline and candidate with `unit_mismatch`
and `malformed_report`, respectively. No failed/refused candidate became verified.
The receipts' internal Snapshot IDs belong to disposable consumer revisions; they do not name
this Task's final candidate. Scratch paths need not survive cleanup; the retained evidence is
this result, reproducible source binding and exact log/receipt hashes. This is validation,
not a claim of measured product speed improvement.

Recomputing the projection after measurement and this record update returned the identical
`sha256:8d9fd457d008476b08d87d4eff00275deec93ce75f7fc5d2fa42bad397d9ad9f`,
**3771 entries, 3096 files**. Only this explicitly excluded record changed during/after the
measurement. That Task repeated its final focused selection after finishing the record; its result and
completion time belong to that historical Task report, not the integration validation below.

## Historical integration validation on merged main

Work began **2026-11-01 23:40:08 UTC**. This Task's input Snapshot is
`sha256:35bc816c2e1ea3033e7a366ca7c9d0003c0997a0dc16547e40b684a2b7347cd5`.
It identifies the supplied starting tree, **not** this Task's final candidate Snapshot.
The coordinator describes that tree as PR506 source `877b0952` integrated with main
`609a0982`. Those are supplied provenance, not locally verified commit identities: this
sandbox contains neither Git metadata nor `../base`. No unavailable Git comparison is claimed.
All earlier projections, hashes, measurements and before-fix probes in this record remain
historical observations of their stated sources. In particular, the original #494 observation
is retained as supplied evidence; it was not reconstructed or rerun against invented old bytes.

### Integration inspection and existing feature coverage

No missing runtime integration was found. The earlier fixes are already present in this Task's
input and were verified through their real paths rather than reimplemented:

| Area | Existing implementation and fresh exercised tests |
|---|---|
| Explicit native setup/status/catalog, no ambient auth and safe refusal output | `crates/af/src/providers/pi.rs`; `pi_setup_status_and_exact_model_catalog_are_token_free_and_secret_free`, `pi_registration_requires_explicit_authority_before_starting_or_changing_a_profile`, early/failed setup JSON and stderr tests |
| Shared Task/Review/doctor binding, typed retries and identity currentness | `crates/af/src/providers/task.rs`; `pi_task_admission_typed_result_retry_and_account_drift_use_the_common_path`, `pi_doctor_and_review_share_the_native_admission_and_readonly_sdk_host` |
| Host lifecycle, isolation, observed accounting and shared process supervision | `crates/review-runner-pi/src/lib.rs` and `host.mjs`; all `review-runner-pi` tests, including readonly/writable tools, drift, framing/Unicode, EOF/limits/errors and cumulative/partial usage |
| #523 repository-owned Node refusal before spawn | `safe_installation` applies to Node and SDK; `pi_node_resolution_and_setup_refuse_repository_owned_path_before_spawn` checks directory/file `.git`, `.af`, nested CWD, absolute PATH, marker nonexecution and legitimate external Node |
| #524 complete sequential inference settlement | `State::record`; `settlement_requires_every_observed_inference_complete_without_inventing_usage` refuses missing/incomplete observations and retains observed/unknown accounting |
| #521 actual observed early reply, exact later prompt read and zero exit | `observed_early_reply_then_exact_prompt_read_and_zero_exit_is_rejected_on_replay` asserts the ordered process observations before production State replay rejects them; adapter refusal and immediate success remain separate passing tests |
| #525 status v5 metadata and actual Pi result | `schemas/provider-status-v5.json`; `schema_listing_names_provider_status_v5_and_its_sdk_host_scope` and the setup/status/catalog CLI test validate title, Pi scope and real result against v5 |
| Managed launcher discovery and bounded cancellation readiness | `pi_setup_discovers_managed_launcher_without_executing_it_and_refuses_unsafe_versions`, malformed-version unit test, `cancellation_during_inference_reaps_the_process_and_keeps_usage`, `cancellation_readiness_observes_early_failure_before_accept_or_pid` |
| Closed authority and protocol schemas, old-shape refusal | `pi_authority_is_closed_versioned_and_required_only_for_pi_models`, `pi_host_and_setup_protocols_reject_old_shapes_and_secret_fields` |
| Official SDK confinement, nested writes and a full typed agent turn | `scripts/test-pi-host.py`, both hermetic and installed official SDK modes; real SDK tool execution and mocked native transport/usage, described below |

The mechanically merged `shipped_schemas` table retains all three main issue-triage schemas
and Pi setup v2. Its full unit-test module passed, including exact parity with `af help json`.
The design index retains main's staged triage status plus both Pi entries. All local links in
those indexes, the Pi design, this record and the new note resolved. Proposed ADR-0274 has
exactly one file and index entry; accepted ADRs 0270–0273, their entries, `.af/` and `.github/`
are byte-for-byte unchanged from this Task's input. No payload or schema shape changed here.

Only this validation record, the Pi design's historical-measurement wording and
`changelog.d/pi-main-integration-validation.md` change. No Linux-only helper or caller in
`provider_auth_handoff.rs`, `task_interrupt.rs` or `campaign_loop/heartbeat.rs` changed.

### Actual merged-source checks

Checks use pinned Rust/Cargo **1.88.0**, `CARGO_NET_OFFLINE=true`, `TMPDIR=/private/tmp`
and a private `CARGO_HOME` under this checkout's root `target/`, seeded with the 248 locked
registry archives/index entries from readable caches. No shared Cargo cache was mutated,
network dependency installation attempted, real credential inspected or paid call made.

The original 217-test Pi/existing-Provider selection passed on the merged tree:

```text
Summary [30.182s] 217 tests run: 217 passed, 1912 skipped
```

Log `/private/tmp/pi-integration-targeted.log`, SHA256
`c7eab6884b90e1f1c3baa6ec1010fd5bb35559d693102285921346aa13502857`.
The combined selection additionally covers main's two `task_triage` integration tests and
Pi's captured-authority schema parity:

```sh
cargo nextest run --workspace --locked --offline --profile ci -E \
  'package(review-runner-pi) | (binary_id(af::it) & (test(/^provider_onboarding::/) | test(/pi_registration_requires/) | test(/pi_task_admission/) | test(/pi_doctor_and_review/) | test(/^task_triage::/))) | (binary_id(af::bin/af) & (test(/^providers::/) | test(/^shipped_schemas::/) | test(/pi_setup_guidance/))) | (binary_id(review-core::schema_parity) & test(/pi_/))'
```

```text
Summary [29.326s] 220 tests run: 220 passed, 1909 skipped
```

Log `/private/tmp/pi-integration-combined-targeted.log`, SHA256
`c7ba6f24256e5cdbacf6f4f32280f24302a839a75499a7895fdb34cbd497fcdf`.
Skips are outside the focused selection, not removed tests or gate exclusions. There were no
failed assertions, flaky reruns or leaky-test warnings in either selection.

`cargo fmt --all`, its read-only check and
`cargo clippy --workspace --all-targets --offline -- -D warnings` passed. Clippy log
`/private/tmp/pi-integration-clippy.log`, SHA256
`7c3bf0929ac381d40aa8459efecb64a30c5fdcf0d350eea46169d2257b49b977`.
`python3 -I scripts/test-classes.py audit` and the changelog-note check passed.
Protected-harness `harness-digest --check` passed with the supplied merged digest
`sha256:d182eaa9f53784005e0fb97c119836705d8ca28d8da0720f7d40479de5205c82`.
The generated fixtures were already correct; no protected source or fixture was edited.
The cached **markdownlint-cli2 0.22.1** executable ran directly without installation and
reported zero errors for all three changed Markdown files.

Both host script modes passed **3 tests**:

```sh
AF_PI_TEST_HERMETIC=1 python3 -I scripts/test-pi-host.py
AF_PI_TEST_SDK=/Users/apakhomov/.pi/agent/install/releases/1.4.0/node_modules/@earendil-works/pi-coding-agent/dist/index.js python3 -I scripts/test-pi-host.py
```

The official mode printed successful real SDK construction/tools and full agent-turn,
read/nested-write, typed-reply and native-usage validation. It used synthetic OAuth-shaped
credentials and mocked upstream HTTP, without reading the real profile or calling a service.
Hermetic mode explicitly printed official SDK coverage **not run** and passed its fixture
contract. The two log hashes are respectively:

- `/private/tmp/pi-integration-host-hermetic.log`:
  `45776aeefcd0fbd1fee42395c2e2530a009b090e25c0141064809ce0c48dabc5`.
- `/private/tmp/pi-integration-host-sdk.log`:
  `5420fcbea220e141e7486bff68eb7e145159c07c073a45d58b5c9183ecc46509`.

The merged triage script tests also passed **50 tests**, no skips, with the freshly built
`af` on PATH for their private-HOME catalog-digest fixture:
`python3 -I scripts/test-backlog-check-triage.py`. An initial invocation without that binary
on PATH passed 49 and reported its existing optional digest test skipped; the complete
invocation exercised it. Complete log `/private/tmp/pi-integration-triage.log`, SHA256
`7c17060680239032b18f5b4c4b4380f3aa9fa0024f3f8ecff4d69d50004ecc1b`.

**Paid live smoke: not run.** This evidence proves fixture/SDK behavior and local contracts,
not an actual OAuth refresh exchange or service inference. The configured fast local gate is
the kernel's post-seal action; no second whole-suite or Worker-run gate pass is claimed.

### Historical merged-source measurement binding

All edits outside this excluded record finished before measurement began at
**2026-11-01 23:44:37 UTC**. The exact projection recipe above, with only its three exclusions
(root `.git`, root `target`, this record), returned:

`sha256:1937edbcf84c0b741fd9f43257f3ee478b22d29c69f907fad35a3d3b8991231e`

**3809 entries, 3124 regular files**. It includes the merged main source/fixtures/indexes,
all Pi implementation and tests, the new changelog note and the Pi design wording correction.
This is a reproducible source-content projection, not the input or final kernel Snapshot ID.
Only this excluded record changes after measurement starts; all build/dependency/receipt
scratch stays under the excluded root `target/`.

### Historical merged-source manual measurement outcome

The Worker ran `make perf-kernel-measure` from **2026-11-01 23:44:37 UTC** to
**2026-11-02 00:00:25 UTC** on the exact merged-source projection above. Make exited **0**:

```text
PASS [941.890s] (1/1) af::it perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample
Summary [941.896s] 1 test run: 1 passed (1 slow), 3972 skipped
[perf-kernel-measure] 947.764s, exit 0
```

The unchanged Make target selected exactly
`test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)` with
`--workspace --locked --profile ci --run-ignored only`. Skips are the remaining inventory,
not weakened tests or exclusions from normal validation. Pinned Rust/Cargo **1.88.0** and the
private/offline environment above were used. No boundary, budget, deadline, assertion, retry,
profile, protected input or generated fixture changed. This is a real guarded-kernel validation
run, not a measured product speed-improvement claim.

Full log `/private/tmp/pi-integration-perf-kernel-measure.log`, SHA256
`3d236ceb9f45648f2823a1d1734ebd6e2534f5eba85370ab3c52055a9eaa745d`.
The log includes the exact starting projection/count, toolchain versions and command, followed
by nextest's real output, end time and Make exit. `AF_PERF_KERNEL_RECEIPTS` selected
`target/pi-integration-perf-receipts`. The test exported and checked these
`af.perf-kernel-receipts/3` documents through the existing schema and reader:

| Scratch receipt | SHA256 | Observed outcome |
|---|---|---|
| `target/pi-integration-perf-receipts/receipts-43551-improve.json` | `27e4b9ff7329dfb370ecbae1b45b856551168c94742811dd32a1cf58835d98a1` | Baseline and candidate passed three repetitions; comparison passed; Task acceptance satisfied and conclusion verified |
| `target/pi-integration-perf-receipts/receipts-43551-candidate-refusal.json` | `a596d01e02f788faf2bdc0c44ff4235a4c09f9f2ffefe910f0da1da49641649e` | All six tampered candidates and both invalid metric-contract cases refused; every conclusion remained incomplete |

For `timing-boundary`, `golden-fixture`, `build-inputs`, `case-variant-build-inputs`,
`symlink-to-file` and `bytecode`, each baseline passed three repetitions; each candidate failed
on its first repetition with command exit 2; comparisons were inconclusive. For `wrong-unit`
and `undeclared-key`, both measurements failed despite their command exiting zero; comparisons
were inconclusive. Each refused Task exited 4, with inconclusive acceptance and incomplete
conclusion. No refusal became verified. The receipts carry the merged protected-harness digest
`sha256:d182eaa9f53784005e0fb97c119836705d8ca28d8da0720f7d40479de5205c82`.
Their internal Snapshot IDs identify disposable test consumer revisions, never this Task's
input or final candidate. Scratch files may be removed; this indexed record retains the actual
outcome, exact log/receipt hashes and reproducible source-content binding.

The projection was recomputed at measurement completion and again after this record was
finished, yielding the identical
`sha256:1937edbcf84c0b741fd9f43257f3ee478b22d29c69f907fad35a3d3b8991231e`,
**3809 entries, 3124 regular files**. Only this explicitly excluded record changed during or
after measurement. The combined 220-test selection is repeated after finishing this record;
its actual completion time and result belong to the Task report, with no edits after that run.

## Historical machine-checked validation: preceding issues 533 and 534 Task

This Task closes documentation/validation issues #533 and #534; the runtime fixes above were
already implemented. The prior merged-source evidence was present and valid, not missing or
stale when supplied: projection `1937edbcf84c0b741fd9f43257f3ee478b22d29c69f907fad35a3d3b8991231e`,
3809 entries/3124 files, actual Make exit 0. This Worker independently read the retained prior
log and confirmed SHA256 `3d236ceb9f45648f2823a1d1734ebd6e2534f5eba85370ab3c52055a9eaa745d`.
That measurement is now historical because the helper, tests and changelog corrections change
source bytes. No input Snapshot is represented as this candidate's final Snapshot.

### Rendered release-note links (#533)

Every Pi fragment was audited. Five existing notes linked repository documentation with
`../docs/` or `docs/`; neither relative form works in both fragment browsing and the root
release notes. Those destinations now use absolute repository documentation URLs; the text
is preserved. `Fragments.test_pi_documentation_links_resolve_in_fragments_and_rendered_root_notes`
first failed on the old `../docs/adr/0274-...` destination from `CHANGELOG.md`, resolving to
`/PakhomovAlexander/afactory/blob/docs/...` instead of `/blob/main/docs/...`.
The corrected suite passed all **10 tests**. The regression runs the real release-note writer
in a private Git repository, verifies unchanged inclusion of every Pi fragment and resolves its
documentation targets from both browsing contexts to existing repository files. A permanent
representative note keeps the regression active after release consumes the shipped fragments.
The real root `CHANGELOG.md` was never edited.

### Fresh manual measurement of the frozen candidate

All edits outside this excluded record finished before **2026-11-02 01:33:20 UTC**.
The Worker ran the unchanged `make perf-kernel-measure` until **01:44:23 UTC**, using pinned
Rust/Cargo **1.88.0**, `CARGO_NET_OFFLINE=true`, `TMPDIR=/private/tmp` and a private
`CARGO_HOME` at `target/pi-cargo-home`, seeded from the readable locked registry caches.
No installation, shared-cache mutation, real credential inspection or paid inference occurred.
The documented recipe and the helper both returned the binding below: **3812 entries,
3127 regular files**. It includes every nonrecord edit; only this excluded record changed
while/after measurement ran. This is a content projection, never a kernel Snapshot identity.

The real command exited **0**:

```text
PASS [ 659.122s] (1/1) af::it perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample
Summary [ 659.141s] 1 test run: 1 passed (1 slow), 3973 skipped
[perf-kernel-measure] 662.940s, exit 0
```

The unchanged Make target selects the exact filter in the claim, with
`--workspace --locked --profile ci --run-ignored only`. Skips are the other tests outside this
manual selection, not weakened validation. This is guarded-kernel validation, not a product
speed-improvement claim. Full log: `/private/tmp/pi-provenance-perf-kernel-measure.log`.
Its actual SHA256, filter, source projection/counts and successful outcome are in that historical
claim. The log starts with the exact source binding, command/filter, start time and toolchain;
it ends with the observed process exit, end time and `source_unchanged=true`.

`AF_PERF_KERNEL_RECEIPTS` exported the existing schema/reader-validated
`af.perf-kernel-receipts/3` documents below. Both carry protected-harness digest
`sha256:d182eaa9f53784005e0fb97c119836705d8ca28d8da0720f7d40479de5205c82`;
`harness-digest --check` passed and no protected input/generated fixture needed changing.

| Scratch receipt | SHA256 | Actual outcome |
|---|---|---|
| `target/pi-provenance-perf-receipts/receipts-74006-improve.json` | `4955f940e71d4ecff637a5cafd73011461105fa0c9445439371166f22bd5936a` | Controlled baseline/candidate assertions passed; Task exited 0 |
| `target/pi-provenance-perf-receipts/receipts-74006-candidate-refusal.json` | `c0f6b72c64f9c8e5e9b6a3a29a0af21e47fba3eedf933234e0ad6c12f853f0ce` | All six tampered-harness and two invalid metric-contract Tasks refused, each exiting 4 |

These receipts' Snapshot IDs belong to disposable test consumers, not this candidate.
Scratch paths need not survive cleanup. The retained evidence is the actual result, exact
log/receipt hashes and reproducible content projection. All preceding measurements/projections
remain explicitly historical, including the original supplied #494 failing observation.

### Historical machine-readable claim

This was that Task's historical claim. The helper verified it against that candidate; optional
explicit log verification also checks the actual log bytes and source/outcome binding.

<!-- af-pi-validation-claim:v1 -->
```json
{
  "schema": "af/pi-validation-claim@1",
  "entries": 3812,
  "files": 3127,
  "source_sha256": "sha256:8d78f6e86956cb6d311a94dee56121eb62cdacafdf706b5763095960ddf8231f",
  "command": "make perf-kernel-measure",
  "filter": "test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)",
  "exit_code": 0,
  "summary": "Summary [ 659.141s] 1 test run: 1 passed (1 slow), 3973 skipped",
  "log_sha256": "sha256:d04b133d924c994e3cafb9f8a383cc0f439b5ba1ec24140de7b723fac73cf267"
}
```
<!-- /af-pi-validation-claim -->

### Actual checks of this claim and candidate

At the time, after adding the claim, both the ordinary helper check and explicit real-log check
passed. Their actual output was:

```text
pi-validation source: {"entries":3812,"files":3127,"source_sha256":"sha256:8d78f6e86956cb6d311a94dee56121eb62cdacafdf706b5763095960ddf8231f"}
current claim matches candidate source; measurement log not supplied (not read)
pi-validation source: {"entries":3812,"files":3127,"source_sha256":"sha256:8d78f6e86956cb6d311a94dee56121eb62cdacafdf706b5763095960ddf8231f"}
current claim matches candidate source; measurement log verified
```

The full existing Pi Python suite, including the then-required normal-CI current-source assertion, passed
**9 tests** in each mode: `AF_PI_TEST_HERMETIC=1` and the explicitly selected installed official
Pi 1.4.0 SDK. The official mode exercised SDK construction, confined tools, nested writes and
the actual SDK agent turn with mocked upstream transport, typed final reply and native usage.
Hermetic mode explicitly reports official SDK execution as not run and passes the fixture path.
The changelog suite passed **10 tests**. No existing host test was removed or skipped.

Before measurement, the focused Rust selection below passed **220 tests**, no failures,
**1910 skipped** outside the selection. It covers the existing Pi runtime and other Provider
contracts, the new claim schema parity test and full schema compilation:

```sh
cargo nextest run --workspace --locked --offline --profile ci -E \
  'package(review-runner-pi) | (binary_id(af::it) & (test(/^provider_onboarding::/) | test(/pi_registration_requires/) | test(/pi_task_admission/) | test(/pi_doctor_and_review/))) | (binary_id(af::bin/af) & (test(/^providers::/) | test(/^shipped_schemas::/) | test(/pi_setup_guidance/))) | (binary_id(review-core::schema_parity) & (test(/pi_/) | test(/every_schema/)))'
```

Actual premeasurement summary:
`Summary [  27.609s] 220 tests run: 220 passed, 1910 skipped`.
This selection is repeated after completing the record; the final run's actual time and result
are retained in the Task report, with no subsequent edit.

Pinned `cargo fmt --all`, `cargo clippy --workspace --all-targets --offline -- -D warnings`,
`python3 -I scripts/test-classes.py audit`, the changelog-note check and protected-harness
`harness-digest --check` passed. No flaky assertion needed a rerun. Offline `npx` returned
`ENOTCACHED` for registry metadata: **markdownlint through npx: not run**. The exact cached
markdownlint-cli2 **0.22.1** executable ran directly without installation, reporting zero errors
for all seven changed Markdown files. No network workaround or new dependency was used.

The helper's ordinary and explicit real-log checks are repeated after this final record update;
the measured source projection remains identical. Only this excluded record changed after the
measurement began. No full workspace suite or Worker-run fast gate is claimed: the configured
`scripts/verify.sh --profile fast` gate remains the kernel's post-seal check.
**Paid live smoke: not run.** No real OAuth profile, refresh exchange or service inference was used.

## Historical machine-checked validation: Task14 ADR restructuring

This Task integrates the supplied Pi/Scenarios source and resolves issue #538's recurring ADR
conflicts. All observations above, including the preceding Task's formerly current claim, are
historical. Its `8d78f6e86956cb6d311a94dee56121eb62cdacafdf706b5763095960ddf8231f` projection
was superseded by the main merge and this restructure. No historical run is attributed to this
candidate, and no Task input Snapshot is claimed to be the final candidate Snapshot.

### Independent decisions and current-main integration

`docs/adr/README.md` now holds stable authoring/lifecycle guidance and GitHub directory browsing,
without a per-decision index. New decisions use unique `YYYY-MM-DD-descriptive-topic.md`
identities; accepted legacy identities, historical numbering anomalies and lifecycle obligations
stay intact. `python3 -I scripts/adr.py list` derives titles, recorded statuses and relative
links in basename order on stdout, without writing a catalog or allocating a sequence.

`CatalogTests.test_independent_same_date_additions_merge_without_shared_edits_and_are_discovered`
creates two real disposable Git branches whose only changes are their distinct same-date ADR
files, merges them cleanly and discovers both while the shared guide stays byte-identical.
All **5 catalog tests** passed, including legacy 0107/0240 exceptions, status wording, missing
metadata, deterministic output, read-only listing and discovery of every file without a manifest.
The suite is classified functional and runs through existing class selection and full preflight.
The gate-ordering fixture now includes that exact added preflight receipt and payload; its
unchanged ordering/failure assertions passed in all **52 test-class script tests**.

Only the unmerged Pi proposal was renamed, to
[the Pi SDK host decision](../adr/2026-11-02-own-the-pi-sdk-host-and-confine-its-tools.md).
Its architecture body is unchanged. The new
[authoring-policy decision](../adr/2026-11-02-add-independent-adrs-and-derive-the-catalog.md)
supersedes sequence/index requirements; ADR-0113 gained only its specific status-paragraph note
about index-entry removal. Its accepted body, accepted Scenarios ADR-0274, ADRs 0270–0273 and
all other accepted files/identities were preserved. Active contributor guidance and live Pi
links now follow the stable guide; older numbering/index wording in shipped Task prompts is
explicitly superseded without editing `.af/`. No `.github/` or released changelog was changed.
`Fragments.test_pi_documentation_links_resolve_in_fragments_and_rendered_root_notes` passed
with the renamed target; the **10-test changelog suite** passed and its isolated post-release
representative still exercises both fragment browsing and root release-note rendering.

The first focused Rust run exposed a real mechanical-merge defect: `scenario-v1.json` still
referenced removed `urn:af:schema:task-contracts:1`, and the Scenario test helper named the
removed v1 file. The run reported **210 passed, 18 failed**; the failures refused registry
preparation with `Unretrievable { uri: "urn:af:schema:task-contracts:1", source: DefaultRetrieverError }`.
The reference and test helper now use the current v2 definition bundle. `TaskResultV1` and the
Scenario wire shape did not change: this repairs resolution of the same typed result, without
adding a compatibility reader or changing a payload version in place.

`scenario_task_receipts_resolve_current_task_contracts_and_keep_closed_results` checks a real
typed receipt and its serialized nested Task result against the published definition, roundtrips
it and rejects extra result fields through both schema and reader. With the original schema
copied into a private probe directory, this new regression failed on the old dangling reference;
it passes against the corrected source. Root inputs still cannot manufacture receipts.
The full **254-test** focused Provider/Pi/Scenario/Ant selection then passed. No runtime behavior,
assertion, budget, deadline, profile, quarantine or sandbox boundary was weakened.
Raw issue filing for the merge defect was attempted with the required three headings;
`gh issue create` was blocked because this Worker has no authenticated GitHub CLI. The defect
and before/after evidence remain recorded here.

### Preserved Pi runtime coverage

The existing common Task admission/retry, doctor and captured Review paths are preserved.
The focused selection covers exact identity/backend/model/effort refusal before private input,
readonly/writable grants, partial JSONL/Unicode, errors/EOF/limits, cancellation and cleanup,
cumulative/partial/unknown usage, complete-inference settlement and typed retry handling.
The deterministic ordered early-response/exact-prompt-consumption/zero-exit regression and
immediate legitimate-response tests remain unchanged and pass. Setup failures retain their
closed JSON reports, safe AF stderr and null invalid identity/backend fields; managed discovery
and repository-owned Node refusals pass. Status v5 schema metadata/listing assertions also pass.

The official Pi 1.4.0 SDK host tests passed using the explicit external installation, synthetic
credentials and mocked upstream transport. They exercise actual SDK construction, confined tools,
read/nested write, a typed final reply and native usage without a paid call. The full suite,
including normal current-source equality checks, passed **9 tests** after writing that historical
claim in each mode: explicit official SDK and hermetic CI fixture. Official mode printed
real SDK construction/tools and full agent-turn/native-usage success; hermetic mode explicitly
reported official SDK execution not run and passed its fixture path. The original #494 failing write observation
above remains historical; it was not recreated or claimed as this Task's discovery.
**Paid live smoke: not run.** No real OAuth profile, exchange or service inference was used.

### Fresh measurement of the frozen integrated source

All edits outside this record finished before **2026-11-02T10:37:22.287580+00:00**. The Worker ran the real,
unchanged `make perf-kernel-measure` until **2026-11-02T10:48:37.873728+00:00**, with pinned Rust/Cargo **1.88.0**,
`CARGO_NET_OFFLINE=true`, `TMPDIR=/private/tmp` and a private `CARGO_HOME` at
`target/pi-cargo-home`, seeded only with locked registry archives/index entries from a readable
cache. No dependency installation or shared-cache mutation occurred.

The existing `scripts/pi-validation.py --projection` recipe returned
`sha256:0bb660f71b82b479bfbc1fd34362b01a242721b025e8655e1b1cb586e8c1cf4f`, **3856 entries / 3169 regular files**.
Its only exclusions are root `.git`, root `target` and `docs/design/pi-provider-validation.md`.
All other paths, including new ADRs, scripts, documentation, tests, `.af/` and `.github/`, are
included. This is the versioned `af.pi.validation-source/1` content projection, never a kernel
Snapshot identity. Recomputing after measurement returned exactly the starting projection;
`source_unchanged=true` is recorded in the actual log. Only this excluded record changes after
measurement. The Task input Snapshot is not claimed as the final candidate Snapshot.

Make exited **0**. Its actual nextest result was:

```text
Summary [ 673.942s] 1 test run: 1 passed (1 slow), 4009 skipped
```

The Make target retains `--workspace --locked --profile ci --run-ignored only` and exact filter
`test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)`.
Skipped tests are outside this manual selection, not weakened validation. This is a guarded-kernel
contract check, not a product speed-improvement claim. The actual complete log is
`/private/tmp/adr538-perf-kernel-measure.log`, SHA256 `sha256:5dc2278409fcdc577aaa7d4fefeeab9e5bc0f6f13dd87a3d513aa06cd6ba907a`.
Its binding, command, exact filter, observed exit and summary are checked with the explicit
`--measurement-log` option below. Normal CI reads no implicit local log.

The test exported these `af.perf-kernel-receipts/3` documents and validated them through its
existing schema/reader assertions. The protected-harness digest remains
`sha256:d182eaa9f53784005e0fb97c119836705d8ca28d8da0720f7d40479de5205c82`.
Official `python3 -I scripts/performance/cli.py harness-digest --write` and `--check` agreed;
the generated fixtures already matched, so no generated or `.af/` identity was edited.

| Scratch receipt | SHA256 | Actual tested outcome |
|---|---|---|
| `target/pi-adr538-final-receipts/receipts-37147-candidate-refusal.json` | `3fbfeb1353bc4f1645bb8d756a1ddb6b7e58f5e5f00aac709b8df84cb57a9239` | Six tampered-harness and two invalid metric-contract Tasks refused; each exited 4 |
| `target/pi-adr538-final-receipts/receipts-37147-improve.json` | `454e662c3dfeb42f4b6a1cee342223b740e608dadb0e50008b84d0e1151cde9a` | Controlled baseline/candidate comparison and acceptance assertions passed; Task exited 0 |

Receipt Snapshot IDs identify disposable test consumers, not this candidate. Scratch logs and
receipts may be cleaned up; the retained record gives their exact hashes, outcomes and the
reproducible content binding.

An earlier run in this Task is historical and unusable as final-source evidence: it started
at `2026-11-02T10:24:49.370030+00:00` on `sha256:253570055dc12b0a36575411b8361e46d3067ce95eed3c7ef720aed8072d2516` before the new receipt fixture and
final author-guidance edits were complete. Make exited 0, but the driver
correctly detected `source_unchanged=false` and exited 99. Its retained log hash is
`sha256:4abc88f20d3129e37ab4be83a325d0ddd6f69b3f928a81d47c5b674edcb2adff` at
`/private/tmp/adr538-historical-prefreeze-perf-kernel-measure.log`.
Its passing Make result was not substituted for the later frozen-source run.
The fresh run above began only after the final nonrecord source was frozen.

### Historical machine-readable claim

This is Task14's historical `af/pi-validation-claim@1` claim. Its closed schema and source
projection are retained. The claim cannot select exclusions or repair a stale source digest.

<!-- af-pi-validation-claim:v1 -->
```json
{
  "schema": "af/pi-validation-claim@1",
  "entries": 3856,
  "files": 3169,
  "source_sha256": "sha256:0bb660f71b82b479bfbc1fd34362b01a242721b025e8655e1b1cb586e8c1cf4f",
  "command": "make perf-kernel-measure",
  "filter": "test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)",
  "exit_code": 0,
  "log_sha256": "sha256:5dc2278409fcdc577aaa7d4fefeeab9e5bc0f6f13dd87a3d513aa06cd6ba907a",
  "summary": "Summary [ 673.942s] 1 test run: 1 passed (1 slow), 4009 skipped"
}
```
<!-- /af-pi-validation-claim -->

### Historical verification commands (former CLI)

```sh
python3 -I -B scripts/pi-validation.py --check
python3 -I -B scripts/pi-validation.py --check \
  --measurement-log /private/tmp/adr538-perf-kernel-measure.log
```

Both compare the exact candidate projection; the explicit log check additionally verifies the
actual log SHA256, source binding, Make command/filter, observed exit and unchanged nextest summary.
Pinned `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` passed
with the private offline Cargo cache and permitted scratch directory. The catalog (5 tests),
changelog (10 tests), test-class script (52 tests), classification audit and official protected-harness
check passed. No flaky assertion required a rerun. Initial integration failures and the new
receipt/gate fixture corrections described above were deterministic, not classified as flakes.
No helper/caller in Linux-only `provider_auth_handoff.rs`, `task_interrupt.rs` or
`campaign_loop/heartbeat.rs` changed; no Linux execution is claimed from this macOS Worker.

`npx --offline --yes markdownlint-cli2@0.22.1` could not run because npm registry metadata was
not cached (`ENOTCACHED`): **markdownlint through npx: not run**. The installed cached
**markdownlint-cli2 0.22.1** executable ran directly, without installation, and reported zero
errors for the changed Markdown files, including this record. The initial shell also selected
Homebrew Rust 1.94; the completed checks and both manual runs explicitly used pinned 1.88.0.
No source was changed to accommodate the wrong toolchain or unavailable default cache/TMPDIR.

The focused Provider/Pi/Scenario/Ant selection passed before the final record update:
`Summary [  33.469s] 254 tests run: 254 passed, 1912 skipped`.
Its exact command is:

```sh
cargo nextest run --workspace --locked --profile ci -E \
  'package(review-runner-pi) | (binary_id(af::it) & (test(/^provider_onboarding::/) | test(/pi_registration_requires/) | test(/pi_task_admission/) | test(/pi_doctor_and_review/) | test(/^scenario::/))) | (binary_id(af::bin/af) & (test(/^providers::/) | test(/^shipped_schemas::/) | test(/pi_setup_guidance/) | test(/^scenario::/))) | (binary_id(review-core::schema_parity) & (test(/pi_/) | test(/every_schema/)))'
```

The focused selection is repeated as the last test run after this record and all other checks;
its exact filter, completion time and result belong to the Task report, with no edits afterwards.
The configured `scripts/verify.sh --profile fast` gate remains the kernel's post-seal action;
no Worker-run whole suite, full gate, paid live smoke or real OAuth exchange is claimed.

## Historical machine-checked validation: catalog confinement

This Task confines ADR catalog reads and corrects the guide's legacy-status wording on the
supplied integrated source. All preceding observations and claims are historical, including
Task14's formerly current projection. The original #494 failing probe remains historical;
it was not re-executed here. No Task input Snapshot is claimed as the final candidate Snapshot.

### Catalog confinement and lifecycle wording

`scripts/adr.py` opens each directory component without following symlinks, then reads entries
relative to that pinned directory descriptor. Markdown symlinks are refused before content is
read. No-follow file opens close the stat/open race, and descriptor-based regular-file validation
precedes reading. Nonblocking opens prevent a raced-in FIFO from hanging. Listing remains sorted
and read-only; a refusal produces safe stderr and no partial catalog on stdout.

Before the fix, the new CLI probes observed exit 0 for an external Markdown symlink, a directory
symlink and an ancestor symlink, and for the skipped FIFO. Their refusal assertions failed.
After the fix, all **9 catalog tests** passed. The new regressions are:

- `test_symlink_entries_refuse_without_emitting_external_contents`: external content markers,
  broken links and directory links are refused, with no output even after a valid earlier entry.
- `test_entry_replaced_by_symlink_between_stat_and_open_is_not_read`: a deterministic replacement
  exercises the real no-follow open and refuses the raced-in external link.
- `test_directory_and_ancestor_symlinks_refuse_before_external_reads`: both directory and ancestor
  symlinks refuse without emitting external markers.
- `test_nonregular_markdown_entry_refuses_without_opening_a_fifo`: a nonregular entry refuses.

The existing independent-addition merge and deterministic derived-catalog regressions still pass.
The catalog discovers all **265 records**. The guide now calls legacy 0240 records and ADRs
0270–0273 existing records, without fabricating acceptance. Their recorded statuses and all
accepted identities/bodies remain unchanged. The Pi date-topic proposal, Scenarios/Ant behavior,
`.af/`, `.github/`, and main's quarantine/performance changes are preserved.

### Actual frozen-source measurement

All nonrecord edits finished before **2026-11-02T12:59:15.856586+00:00**. The unchanged
`make perf-kernel-measure` ran until **2026-11-02T13:16:23.739495+00:00** and exited **0** using pinned
Rust/Cargo 1.88.0, offline registry caches in `target/pi-cargo-home`, the default checkout
`target/`, and `TMPDIR=/private/tmp`. No gate, deadline, budget or assertion changed.

The versioned `af.pi.validation-source/1` projection was **`sha256:7f9261ba019aa397fb84cae3673d0e04d3e3ca0e3d11afc75e1575dcce16d8a3`**,
**3859 entries / 3172 regular files**, both before and after measurement.
Its only exclusions remain root `.git`, root `target` and this validation record
(`docs/design/pi-provider-validation.md`). `source_unchanged=true` is recorded in the real log.
This is a reproducible source-content projection, not a kernel Snapshot identity.
Only this excluded record changed after measurement.

The actual log is `/private/tmp/adr-confinement-perf-kernel-measure.log`, SHA256
`sha256:c264cfb6ccd88b5f55310bcff46c331769a9afe1442e60af995d6fd61e02790c`. The observed result was:

```text
Summary [ 912.403s] 1 test run: 1 passed (1 slow), 4010 skipped
```

The 4010 skipped tests were outside this exact manual selection. This guarded-kernel contract
check is not a product speed claim. The official protected-harness check matched the combined
source's required digest:
`sha256:c10a05c7d6b40f73f1a1da87d9c5e5ecced824314e74f1f87e2318635631b000`.
Generated fixtures already match; no harness or authority bytes were changed.

### Historical machine-readable claim

The unchanged closed schema and helper checked this historical claim. Its exclusions cannot be
selected by claim data. The explicit log check verifies the real hash, source binding, command,
filter, observed exit and nextest summary; normal CI requires no implicit local log.

<!-- af-pi-validation-claim:v1 -->
```json
{
  "schema": "af/pi-validation-claim@1",
  "entries": 3859,
  "files": 3172,
  "source_sha256": "sha256:7f9261ba019aa397fb84cae3673d0e04d3e3ca0e3d11afc75e1575dcce16d8a3",
  "command": "make perf-kernel-measure",
  "filter": "test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)",
  "exit_code": 0,
  "log_sha256": "sha256:c264cfb6ccd88b5f55310bcff46c331769a9afe1442e60af995d6fd61e02790c",
  "summary": "Summary [ 912.403s] 1 test run: 1 passed (1 slow), 4010 skipped"
}
```
<!-- /af-pi-validation-claim -->

### Focused verification and limits

Pinned `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` passed.
The catalog (9 tests), changelog (10 tests), test-class script (52 tests), classification audit
and protected-harness check passed. No flaky failure required a rerun. The Provider/Pi/Scenario
and schema selection passed **254 tests**, with 1912 tests outside its selection:
`Summary [  39.305s] 254 tests run: 254 passed, 1912 skipped`.

Official `HostTests` passed **3 tests** with explicit Pi SDK 1.4.0, synthetic credentials and
mocked transport: actual SDK construction, confined tools, nested writes, typed final reply
and native usage. Hermetic `HostTests` also passed **3 tests** and explicitly reported that
actual SDK execution was not performed in that mode. **Paid live smoke: not run.** No real
OAuth profile, credential inspection, exchange or paid inference was used.

After the claim update, all **6 validation tests** passed, including current-source equality,
stale/mutated claims and exact exclusions. The explicit helper check reported
`current claim matches candidate source; measurement log verified`:

```sh
python3 -I -B scripts/test-pi-host.py ValidationTests
python3 -I -B scripts/pi-validation.py --check \
  --measurement-log /private/tmp/adr-confinement-perf-kernel-measure.log
```

`npx --offline --yes markdownlint-cli2@0.22.1` lacked registry metadata (`ENOTCACHED`):
**markdownlint through npx: not run**. The cached markdownlint-cli2 0.22.1 executable is used
directly without installation. No Linux-only helper/caller in `provider_auth_handoff.rs`,
`task_interrupt.rs` or `campaign_loop/heartbeat.rs` changed; no Linux execution is claimed.
The configured local fast gate remains the kernel's post-seal check. No whole-suite Worker
run is claimed. The focused Rust selection is repeated after the record and all other checks;
its final completion time/result belongs to the Task report, with no edits afterwards.

## Historical GitHub adapter merge validation (2026-11-02)

PR #548 merges main `ca4e73be` while retaining its ADR discovery guide and both Pi and
GitHub adapter test registrations. Its measurement below binds only that historical adapter
source, separately from the catalog-confinement record. Neither binds the later combined RC5
tree. No gate or measurement contract was changed by that merge.

The real `make perf-kernel-measure` log is retained at
`/Users/apakhomov/bs/apkhmv-main/af/scenario-ant-workstream/logs/scenario-github-merge-perf-kernel.log`, with SHA256
`sha256:b9cc4fa3bd5c082f6e5a31adfbda4a1840670abc9e8c0758ac156f9c5fc43f5e`. The command exited 0, and the source projection was
identical before and after measurement: `sha256:aa0f0012b53e0b8ad6448b7213cd4bfd97c10f38bffed193ff64356991f22144`,
3883 entries and 3195 regular files. Only this excluded
validation record changed afterward. HOME and all XDG directories were isolated; build
artifacts remained in this checkout’s `target/`. This is a guarded-kernel contract
measurement, not a product speed claim. No live Pi or GitHub credentials were used.

```text
Summary [ 792.681s] 1 test run: 1 passed (1 slow), 4014 skipped
```

### Historical adapter machine-readable claim

The catalog claim above is restored from its original surrounding prose; #548 had overwritten
its JSON with these adapter values. Both historical observations remain separate and unchanged.

<!-- af-pi-validation-claim:v1 -->
```json
{
  "schema": "af/pi-validation-claim@1",
  "entries": 3883,
  "files": 3195,
  "source_sha256": "sha256:aa0f0012b53e0b8ad6448b7213cd4bfd97c10f38bffed193ff64356991f22144",
  "command": "make perf-kernel-measure",
  "filter": "test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)",
  "exit_code": 0,
  "log_sha256": "sha256:b9cc4fa3bd5c082f6e5a31adfbda4a1840670abc9e8c0758ac156f9c5fc43f5e",
  "summary": "Summary [ 792.681s] 1 test run: 1 passed (1 slow), 4014 skipped"
}
```
<!-- /af-pi-validation-claim -->

## Historical RC4 machine-checked validation

### Official RC4 source and retained history

This validation-only Task refreshes the whole-source binding for official **0.14.0-rc.4**.
The supplied release provenance is `d2e71da0`, branch `release/v0.14.0-rc.4`, PR #550,
prepared by `make release VERSION=0.14.0-rc.4 PROFILE=fast` from main `ca4e73be`.
This sandbox has no Git metadata; the independently recomputed content projection below,
not an invented commit or final candidate Snapshot identity, binds the measured source.

The official `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, generated harness fixtures and
consumed changelog fragments remain exactly as supplied. The release section already documents
Pi. No fragment is recreated and no new change note is added for this evidence-only refresh.
Only this excluded validation record changes; no product, authority, `.af/`, `.github/`,
checker, source exclusion, validation class, budget, deadline or assertion changes.
All preceding records, including the catalog-confinement claim and its checks, are historical.
Their source bindings and outcomes are preserved, not reused as RC4 measurement evidence.

The initial `python3 -I -B scripts/pi-validation.py --check` exited **1**, reporting
`current claim does not match candidate source`: the preceding claim still named
`sha256:7f9261ba019aa397fb84cae3673d0e04d3e3ca0e3d11afc75e1575dcce16d8a3`.
That is the expected stale claim after official release generation, not a passing RC4 check.
No failed measurement is omitted: the single measurement invocation below passed without rerun.

### Actual frozen RC4 measurement

The unchanged **`make perf-kernel-measure`** started at **2026-11-02T16:35:18.070847+00:00**
and finished at **2026-11-02T17:01:06.165090+00:00**, with observed Make exit **0**.
It used pinned Rust/Cargo **1.88.0**, a credential-free offline registry copy in
`target/pi-cargo-home`, the checkout's default `target/`, and `TMPDIR=/private/tmp`.
The initial `rustup run 1.88.0 cargo --version` probe could not create its ambient home;
the installed pinned toolchain was selected directly on `PATH` for all completed Cargo checks
and the measurement. Completed checks needed no new dependency, network fetch or additional
host permission.

The versioned `af.pi.validation-source/1` projection was
**`sha256:2339691c0b34404684686e27422b3d90f7f7db4fdda2ecc3c59dae137cf7ae14`**, **3799 entries / 3112 regular files**,
before and after measurement. Its only exclusions remain root `.git`, root `target` and
`docs/design/pi-provider-validation.md`. The real log records `source_unchanged=true`;
only this excluded record was edited afterwards.

The complete retained log is **`/tmp/rc4-perf-kernel-measure.log`** (also accessible through
`/private/tmp/rc4-perf-kernel-measure.log`), SHA256
`sha256:790aedf44dd4a37b8db3d4a61e11ebe0d0ab69b40fb1f0fb5694282c02185796`. It records the source line, command, exact filter,
observed exit, unchanged nextest summary and measurement start/finish times:

```text
Summary [1323.393s] 1 test run: 1 passed (1 slow), 4010 skipped
```

The selection remains `--workspace --locked --profile ci --run-ignored only` with exact filter
`test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)`.
The 4010 skipped tests are outside this manual selection, not exempted from normal validation.
This guarded-kernel contract check is not a product speed-improvement claim.
The canonical generated harness check independently returned
`sha256:6c80be30504684937effb4e3a8768966f15dca2e0c7d87de3e2ba47f118e5115`;
no generated fixture or authority digest was edited.

### Historical RC4 machine-readable claim

The existing closed schema, helper and six `ValidationTests` checked RC4's then-current claim,
source equality, exact exclusions, stale/mutated claim refusals and optional real-log binding.
Normal CI reads no implicit local log. The explicit check verifies the actual log SHA256,
source line, command, filter, Make exit and summary.

<!-- af-pi-validation-claim:v1 -->
```json
{
  "schema": "af/pi-validation-claim@1",
  "source_sha256": "sha256:2339691c0b34404684686e27422b3d90f7f7db4fdda2ecc3c59dae137cf7ae14",
  "entries": 3799,
  "files": 3112,
  "command": "make perf-kernel-measure",
  "filter": "test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)",
  "exit_code": 0,
  "log_sha256": "sha256:790aedf44dd4a37b8db3d4a61e11ebe0d0ab69b40fb1f0fb5694282c02185796",
  "summary": "Summary [1323.393s] 1 test run: 1 passed (1 slow), 4010 skipped"
}
```
<!-- /af-pi-validation-claim -->

### Historical verification and limits

```sh
python3 -I -B scripts/pi-validation.py --check
python3 -I -B scripts/pi-validation.py --check \
  --measurement-log /tmp/rc4-perf-kernel-measure.log
python3 -I -B scripts/test-pi-host.py ValidationTests
python3 -I -B scripts/performance/cli.py harness-digest --check
```

Pinned `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` passed.
The ADR catalog's **9 tests**, changelog's **10 tests**, changelog fragment check and test-class
audit passed. Both helper checks above passed, including actual-log verification; all six
`ValidationTests` passed. Cached `npx --offline --yes markdownlint-cli2@0.22.1` reported zero
errors without network.
The focused Rust selection passed **15 tests**, with 194 outside the selection, before this
record update; it is repeated after the record and all other checks as the final test run:

```sh
cargo nextest run --workspace --locked --profile ci -E \
  'package(review-runner-pi) | (binary_id(review-core::schema_parity) & test(=every_schema_is_a_valid_json_schema))'
```

The final completion time and exact result belong to the Task report, with no edits afterwards.
No flaky assertion required a rerun. **Paid live smoke: not run.** No live model inference,
private authentication read, real OAuth exchange, SDK live call or publication was performed.
No Linux-only helper/caller in `provider_auth_handoff.rs`, `task_interrupt.rs` or
`campaign_loop/heartbeat.rs` changed; no Linux execution is claimed from this macOS Worker.
The configured local fast gate remains the kernel's post-seal check; no whole-suite Worker
run is claimed.

## Historical premerge RC5 validation

### RC5 Linux release build runners

This Task updates the supplied official **0.14.0-rc.5** release source. Its generated version,
prior release sections and consumed fragments are preserved. The RC5 release notes describe the
runner fix directly; no unconsumed fragment is added.

Only the two Linux build matrix entries in `.github/workflows/release.yml` now use
`ubuntu-24.04`. This follows the
[GitHub migration notice](https://github.blog/changelog/2026-09-17-ubuntu-26-generally-available-and-latest-migration/)
for workflows that need packages affected by Ubuntu 26.04. The three targets, macOS runner,
validation jobs and classes, `qemu-user-static` installation, `qemu-aarch64-static` execution,
archive smoke, signatures, exact-SHA/version selection and permissions are preserved.
`LabelSetup.test_release_linux_builds_pin_ubuntu24_for_static_qemu_execution` failed against
the original `ubuntu-latest` matrix and passed after the two pins. It checks all three matrix
rows and the QEMU, smoke, embedded signing key and exact-source build requirements.

Earlier measurements, including RC4's complete claim and outcome, remain historical. The
catalog-confinement sentence formerly calling its historical claim current is corrected, as
are the stale current-claim headings and prose for preceding Tasks. The premerge RC5 claim
below is historical too; it is not evidence for the later combined RC5 tree.

### Actual frozen premerge RC5 measurement

All nonrecord source edits finished before **2026-11-02T18:25:55.310058+00:00**. The unchanged
**`make perf-kernel-measure`** finished at **2026-11-02T18:41:10.783625+00:00**, with observed Make exit **0**.
It used installed pinned Rust/Cargo **1.88.0**, a credential-free offline registry copy in
`target/pi-cargo-home`, the checkout's default `target/`, and `TMPDIR=/private/tmp`.
No dependency, network fetch or additional host permission was needed.

The versioned `af.pi.validation-source/1` projection was
**`sha256:41cf48452e806ca51167b55b05165fd22f197c286f662cba97cac2119084e955`**, **3799 entries / 3112 regular files**,
before and after measurement. Its exclusions remain exactly root `.git`, root `target` and
`docs/design/pi-provider-validation.md`. The log records `source_unchanged=true`; only this
excluded record changed afterwards. This content projection is not an invented commit or final
candidate Snapshot identity; this sandbox has no Git metadata.

The complete real log is **`/tmp/rc5-perf-kernel-measure.log`**, SHA256
`sha256:369a647a1c971b6756d25afdddd23ce9bd7f5d43086f59b66db67c83783f12a8`. Its source line, command, exact filter,
observed Make exit and sole nextest summary match the historical premerge RC5 claim:

```text
Summary [ 817.770s] 1 test run: 1 passed (1 slow), 4010 skipped
```

The existing selection remains `--workspace --locked --profile ci --run-ignored only` with
filter `test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)`.
Tests skipped by this manual selection are not excluded from normal validation. This guarded
contract check is not a product speed-improvement claim. The canonical harness check returned
`sha256:6c02e9002693abcb355390d5535b8a93cf7b26857c6178319c29014684d8153c`;
the existing RC5 pin is coherent, so no authority digest or generated fixture needed a change.

### Historical premerge RC5 machine-readable claim

The closed `af/pi-validation-claim@1` schema binds this historical premerge RC5 claim.
Normal CI reads no implicit local log. Explicit log verification checks its actual bytes,
source binding, command, filter, exit and summary.

<!-- af-pi-validation-claim:v1 -->
```json
{
  "schema": "af/pi-validation-claim@1",
  "source_sha256": "sha256:41cf48452e806ca51167b55b05165fd22f197c286f662cba97cac2119084e955",
  "entries": 3799,
  "files": 3112,
  "command": "make perf-kernel-measure",
  "filter": "test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)",
  "exit_code": 0,
  "log_sha256": "sha256:369a647a1c971b6756d25afdddd23ce9bd7f5d43086f59b66db67c83783f12a8",
  "summary": "Summary [ 817.770s] 1 test run: 1 passed (1 slow), 4010 skipped"
}
```
<!-- /af-pi-validation-claim -->

### Historical verification and limits

```sh
python3 -I -B scripts/pi-validation.py --check
python3 -I -B scripts/pi-validation.py --check \
  --measurement-log /tmp/rc5-perf-kernel-measure.log
python3 -I -B scripts/test-pi-host.py ValidationTests
python3 -I -B scripts/test-test-classes.py
python3 -I -B scripts/test-release-resolve.py
python3 -I -B scripts/test-changelog-notes.py
python3 -I -B scripts/changelog-notes.py --check
python3 -I -B scripts/test-classes.py audit
python3 -I -B scripts/performance/cli.py harness-digest --check
```

Pinned `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` passed.
The test-class contract suite passed **53 tests**, release resolver **25 tests**, and changelog
**10 tests**; the changelog fragment check and test-class audit passed. All **6 validation tests**
and both helper checks passed, including the real-log check. The focused Rust selection passed
**2 tests** (claim schema parity and compilation of every schema):

```sh
cargo nextest run --workspace --locked --profile ci -E \
  'binary_id(review-core::schema_parity) & (test(=task_contracts::pi_validation_current_claim_is_versioned_and_closed) | test(=every_schema_is_a_valid_json_schema))'
```

That selection is repeated after the record and all other checks; its final time and result
belong to the Task report. No flaky assertion required a rerun. Markdownlint was **not run**:
`markdownlint-cli2@0.22.1` was unavailable in the npm cache (`ENOTCACHED` in offline mode).
**Paid live smoke: not run.** No private authentication, paid inference, Provider registration,
remote gate, publication or push was performed. No Linux-only helper/caller in
`provider_auth_handoff.rs`, `task_interrupt.rs` or `campaign_loop/heartbeat.rs` changed.
No hosted Linux build, QEMU execution or signature publication was run from this macOS Worker;
the workflow preserves those required validations. The configured local fast gate remains the
kernel's post-seal check; no whole-suite Worker run is claimed.

## Combined RC5 evidence-contract correction (2026-11-02, #551)

The supplied source mechanically integrates main `08426525` (#548 GitHub adapter) with the
RC5 Linux runner repair. RC5 remains version `0.14.0-rc.5`; all five newly merged
`scenario-github-*.md` notes are consumed into its release section, preserving note content
and all prior sections through the release generator's collection semantics. Both Ubuntu 24.04
build pins and their regression remain intact. All merged adapter/product code is preserved.

The catalog, adapter, RC4 and premerge RC5 records above name separate historical projections
and actual log hashes. No single-current marker or fresh combined-source performance result is
claimed. The premerge RC5 measurement passed before main integration; its Task gate was
cancelled after the conflict. That cancelled Task is not verified. The second Task was cancelled
before measurement and is not verified either. No new measurement, paid smoke, private
authentication read, Provider registration, remote gate, publication, push or RC4 tag rewrite
is part of this correction. The kernel runs the configured local fast gate after sealing;
focused Worker checks do not replace that gate or hosted release validation.

### Focused checks for the combined RC5 correction

Pinned Rust/Cargo 1.88.0 `cargo fmt --all` and
`cargo clippy --workspace --all-targets -- -D warnings` passed using an offline registry copy
under this checkout's `target/pi-cargo-home` and `TMPDIR=/private/tmp`. The first offline Cargo
probe had no `num-bigint` in its isolated cache; a copy of the readable registry cache supplied
it without network access. The first compile used the host's unwritable default temp path;
completed checks used the permitted scratch directory. No source contract or limit changed.

The hermetic Pi host suite passed **11 tests** (eight validation fixtures and three host tests);
it makes no claim to official SDK execution in hermetic mode. The GitHub adapter suite passed
**25 tests**, ADR discovery **9**, changelog **11**, release resolver **25** and test-class
contract suite **53**. Changelog fragment validation and the test-class audit passed.
The canonical harness check still returned
`sha256:6c02e9002693abcb355390d5535b8a93cf7b26857c6178319c29014684d8153c`;
no protected input required a re-pin. The focused Rust claim/schema selection passed **2 tests**,
with **193 skipped** outside that selection:

```sh
cargo nextest run --workspace --locked --profile ci -E \
  'binary_id(review-core::schema_parity) & (test(=task_contracts::pi_validation_historical_claim_is_versioned_and_closed) | test(=every_schema_is_a_valid_json_schema))'
```

The selection is repeated as the last Worker check after all edits. The Task report records its
completion time and exact result. Cached markdownlint-cli2 **0.22.1** checked all three changed
Markdown files with no errors; npx's offline resolver lacked package metadata (`ENOTCACHED`),
so the installed cached executable was used directly. No flaky assertion required a rerun.
No Linux-only helper/caller in `provider_auth_handoff.rs`, `task_interrupt.rs` or
`campaign_loop/heartbeat.rs` changed. No hosted architecture/QEMU/archive/signature execution,
new performance measurement or whole-suite Worker gate run is claimed.
