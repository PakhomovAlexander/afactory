# Pi Provider validation record

This record separates retained historical observations from the current source-bound checks
at the end. Everything before “Current machine-checked validation” is historical; its “fresh”
and “this Task” wording refers to the stated earlier Task, never the present candidate.
Historical/input Snapshot IDs below are not final candidate identities.
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

## Current machine-checked validation

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

### Explicit current-claim contract (#534)

`scripts/pi-validation.py` implements the exact `af.pi.validation-source/1` recipe above with
only its three fixed exclusions: root `.git`, root `target` and this record. Exclusions cannot
come from claim data. Both the original documented recipe and the helper returned the same
candidate projection/count before measurement. The helper neither writes the record nor reads
ambient profiles, credentials or an implicit developer-specific log. Symlink targets are hashed
as bytes, never followed. It is deterministic and uses only Python's standard library.

One delimited JSON claim below uses `af/pi-validation-claim@1`, with the closed contract in
[`pi-validation-claim-v1.json`](../../schemas/pi-validation-claim-v1.json). Duplicate fields,
missing/extra fields, unsupported versions, duplicate claim blocks and malformed claims refuse.
The helper reads that same schema; `pi_validation_current_claim_is_versioned_and_closed` also
checks it through the real JSON Schema validator, including old shapes and missing fields.

```sh
python3 -I -B scripts/pi-validation.py --projection
python3 -I -B scripts/pi-validation.py --check
# Explicit optional verification of the actual local log; not needed by normal CI:
python3 -I -B scripts/pi-validation.py --check \
  --measurement-log /private/tmp/pi-provenance-perf-kernel-measure.log
```

`--check` compares digest and entry/file counts to candidate content; it never silently repairs
a stale claim. Without `--measurement-log`, it explicitly reports that no log was supplied or
read. With that option, it checks the real file's SHA256 and its exact source binding, command,
filter, Make exit and nextest summary. A missing ephemeral log is never asserted to be verified.
The log binding lines use the canonical `--projection` output followed by `command:`, `filter:`
and the actual process outcome `make exit:`; the original nextest summary remains unmodified.

The existing, classified `scripts/test-pi-host.py` suite now always runs
`ValidationTests.test_current_claim_matches_this_candidate`, including hermetic CI mode.
Its five private-tree regressions passed before measurement: source mutation/stale digest/counts,
record-only edits and precisely scoped exclusions (including nested/hidden paths), independent
projection rows/Unicode/symlinks/executable bits, closed versioned unique claims, and optional
log hash/source/filter/outcome verification. They use `-I -B` subprocesses; the imported helper
also suppresses bytecode writes. All three existing host tests are preserved.
No gate, classification, retry, budget, profile, sandbox boundary, `.af/`, `.github/` or accepted
ADR 0270–0273 changed. No Linux-only helper or caller in `provider_auth_handoff.rs`,
`task_interrupt.rs` or `campaign_loop/heartbeat.rs` changed.

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
Its actual SHA256, filter, source projection/counts and successful outcome are in the current
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

### Current machine-readable claim

This is the sole current claim. The helper verifies it against candidate content; optional
explicit log verification also checks the actual log bytes and source/outcome binding.

<!-- af-pi-validation-current:v1 -->
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
<!-- /af-pi-validation-current -->

### Actual checks of this claim and candidate

After adding the current claim, both the ordinary helper check and explicit real-log check
passed. Their actual output was:

```text
pi-validation source: {"entries":3812,"files":3127,"source_sha256":"sha256:8d78f6e86956cb6d311a94dee56121eb62cdacafdf706b5763095960ddf8231f"}
current claim matches candidate source; measurement log not supplied (not read)
pi-validation source: {"entries":3812,"files":3127,"source_sha256":"sha256:8d78f6e86956cb6d311a94dee56121eb62cdacafdf706b5763095960ddf8231f"}
current claim matches candidate source; measurement log verified
```

The full existing Pi Python suite, including the new normal-CI current-source assertion, passed
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
