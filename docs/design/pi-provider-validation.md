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

## Current Task: executable boundary, terminal accounting and observed ordering

The current Worker's input Snapshot is
`sha256:8edb684e28d142b1ccefed34eaaf74ae9f2f946e76d4e26f3a934bfd9d75f9c8`.
It is the input, **not** the corrected candidate Snapshot. This sandbox has neither Git metadata
nor `../base`; the supplied PR head `5851cc94` and main `9ea` are Task provenance, not locally
verified commit identities. The feature was assessed against the complete requirements and the
available implementation/tests; no unavailable main diff or live-service execution is claimed.
Work began at **2026-11-01 23:06:49 UTC** (02:06:49 MSK on November 2).

### Fresh before-fix evidence

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

### Pinned corrected-source checks

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

### Current measured-source binding

All edits outside this excluded validation record finished before the fresh measurement started
at **2026-11-01 23:14:54 UTC**. The exact recipe in “Reproducible measured-source projection”
above, with only its three explicit exclusions, yielded:

`sha256:8d9fd457d008476b08d87d4eff00275deec93ce75f7fc5d2fa42bad397d9ad9f`

**3771 entries, 3096 regular files**. It includes the new changelog note, all source/tests,
schema metadata, docs and fixture-launcher bytes. The projection is a non-self-referential
content binding, not a Task input/final Snapshot claim. Only this record may change after
measurement starts; build/receipt scratch remains excluded under root `target/`.

### Fresh manual measurement result

The Worker actually ran `make perf-kernel-measure` from **23:14:54** to **23:27:37 UTC**
on **2026-11-01**, on the exact current projection above. Rust/Cargo were **1.88.0**,
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
measurement. The final focused selection is repeated after the record is finished; its actual
result and completion time are carried in the Task report, with no edits after that run.
