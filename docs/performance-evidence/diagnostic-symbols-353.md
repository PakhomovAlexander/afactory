# Diagnostic symbols: inherited executed build evidence for #353

This is an attributed record of **prior executed evidence**, supplied to this continuation as
`af/Requirements@1`. It is not a newly executed build, an `af.measure-report/1`, a native
Measurement, an accepted component, or a present-day speed/qualification claim. The old candidate
was rejected. New archive-admission and recorder behavior needs the separate tests below and
fresh coordinator capture proof.

## Evidence identity and source relationship

- Implementation report artifact:
  `sha256:405c721c9ba5f967fe8558bae1c829ebd39aa776c092bec1c23a7758199550d9`.
- Published report content identity:
  `sha256:6b9d35eef2692259d85547fcd618d2786c47b307bffa9005c14be2239cebbdab`.
- Public sealed candidate: `a6766675a3154b3b5c2dc4ed33a7182efeeaf265`,
  tree `f8005fe202001d09975192003b79dc7ae1a715d2`;
  snapshot `sha256:c8f8525d6e24f320bb6f181ede89fca0afb829517d708eeb808447452b145468`.
- Supplied source attestation:
  `sha256:b33847352dc7d9ed4cecb035305b3b37b2fb87385b611260d88f8d9eadb338c3`,
  manifest `sha256:8eae0ee5d51c24cc404db60a3f0d8801d42f6c7cb032c8be6882023d467ea65c`;
  all 2,665 committed files' bytes and modes were verified at materialization.
- This continuation's source input:
  `sha256:06dda0335e6e94440f97a0bd7e2e20d33a2d3f1fb65fd0c6dfa2cf4359d53152`,
  snapshot `sha256:6889626f349735d7fbefd10f68a7446c218a07ce9f9bad803c7369bb3676cb41`.

The prior report states that its final runtime/recipe bytes matched its build snapshot; later
changes restored a test import and regenerated inventory. Its sandbox had empty read-only Git
metadata, so the two independent builds used owned proof commits representing actual source
bytes, not upstream commit identities. Both recorded `shared_with=null`. Those private proof
identities and host locations are not published here.

This continuation inventoried every entry file's SHA-256 and mode before editing, then compared
the binary-affecting closure after editing. Every Rust source/example and build configuration,
Builder, adapter, suite, diagnostic registry, and the exact `diagnostic_recipe` function remained
unchanged. A subsequent read-only comparison against the retained source confirmed the same
bytes/modes for all 831 closure files and the exact recipe function, independently of the
sandbox-entry inventory. Edits to that function's module change only the profiles contract, not recipe
construction. The Python recorder/admission changes do not enter the compiled AF/probe/supervisor
images. No ordinary verdict/release recipe or accepted telemetry implementation changed. The binary
table below identifies only the prior artifacts; a future build must record its own source,
embedded-commit provenance and hashes even when its Rust bytes are unchanged.

Reproducible closure digests below hash sorted UTF-8 records
`<mode as three octal digits> <file SHA-256 hex> <repository-relative path>\n`.
The first group contains every file below `crates/`, every file named `Cargo.toml`, `Cargo.lock`,
`build.rs`, `rust-toolchain` or `rust-toolchain.toml`, and files under `.cargo/`; exclude `.git/`,
`target/` and `__pycache__/`. The second group is exactly the five files listed afterward.
Each group and each individual hash matched the sandbox-entry inventory after the change.

| Unchanged input group | Files | SHA-256 |
| --- | ---: | --- |
| Rust sources/examples and build configuration | 826 | `21c365de7e63975fa06b1394511af5417167010c1fe05362880204db55cce9fa` |
| Builder, adapter and recipe inputs | 5 | `5336bb919a3702f9e7b6c685cebd7a9217e2393440e8d20b54a2e48444efe6f1` |

| Unchanged input | Bytes | SHA-256 |
| --- | ---: | --- |
| `scripts/performance/perflab/builds.py` | 82,359 | `655e51d8f848f5f2bc5301d3ee4f8cbdbb11bbd2369116228d214433783d6b9f` |
| `scripts/performance/perflab/adapters.py` | 5,048 | `1b3c7845a84b2568bd950a1e871c015f08f3939b3624163e6e34308527d95d7f` |
| `fixtures/performance/adapters.toml` | 1,899 | `13c8cfa2a0cf3d57b992098ebf941112d0f6080f8acb42e7cea73bcaa95bc0d2` |
| `fixtures/performance/suite.toml` | 33,907 | `026e04c17ecd65b3a1fd3012f6b040db3073d5e333a222a5255f724c71e874b0` |
| `fixtures/performance/diagnostics.toml` | 8,591 | `7699198b4d50134d5bbf8d38cf359dc58e54f1318b1812f6cefc1471a9a9a1cd` |

The exact UTF-8 source segment of `diagnostic_recipe` (from `def` through its final return,
without a trailing newline) hashes to
`2da78ba4184ab925041c09fbbefc7b4683ed63d7baf57c2fd4d3a5466c02dca6`.
The unchanged inventory generator `scripts/performance/perflab/kernelguard.py` is 25,052 bytes,
SHA-256 `f07ee95953d261cebe82d747c82107546dd85750341c2667f9dda7fed2a79c3f`.

These checks establish no binary-affecting delta in this continuation. The link to the earlier
executed build relies on the supplied exact-materialization attestation and report; this
sandbox has no original proof binaries, proof script/logs or Git objects to independently
re-execute their inspection. If any binary-affecting input changes, this evidence does not
establish the resulting binaries and a fresh build is required.

## Prior executed results

The implementer used Python **3.14.4**, Rust/Cargo **1.88.0**, offline cached dependencies and the
actual Builder with two independent worktrees, on trusted-host Ubuntu. This was not the pinned
GNU container. The explicit diagnostic rustflag was `-Cdebuginfo=line-tables-only`;
`debug_symbols=true` and frame pointers remained enabled. No symbols were stripped.

| Original executable | Logical bytes | Binary SHA-256 | XZ bytes | XZ SHA-256 |
| --- | ---: | --- | ---: | --- |
| Baseline AF | 285,890,672 | `3dc11bc64f4fdd6ec14837eb1cddda750f8121becfb880a4b3cc1a13f3b2b442` | 45,428,388 | `143905cd2c413823a39672a1284548277b7b68c1c8eeb86b2b483746c12e1efb` |
| Candidate AF | 285,885,936 | `8a86cf090326a299bda974ed90b5f79f1e8ba8049df7b29477767b679e77fbee` | 45,436,760 | `b65dc7876fe4d046946205f9aa385dc3a02b81304426f331c346d54450cd1e02` |
| Baseline probe | 180,494,120 | `b47c82e07b70dc60e3b5d3aafa27f76b07193221c46555eef145b3a59a6a2469` | 25,518,360 | `86d2fbc79f2cc37655e3c6a3399309dae4ea17872b7ece4bd1727e4434787e4a` |
| Candidate probe | 180,494,288 | `e0598ce74c3e2c060e9df796a0a94312effc28d4ad6d032af05c3597f4f42a6a` | 25,508,424 | `ea7d7f3b120e6f8d41d3884e45a0acb34c11342ff0b48b35b0909430a8e475ba` |

The authority supervisor was **7,173,480 bytes**. Its hash was not supplied in the report.
All five executables plus manifest totaled **939,939,186 bytes**, below the unchanged
**1,073,741,824-byte** selected build cap. Worktree peak was **6,098,833,885 bytes**, below
**42,949,672,960**. Embedded commit checks, source-integrity checks, artifact rehashing and
cleanup passed on both sides.

Production `Profile.symbols`/`Budget` were exercised separately for AF and probe pairs:

| Separate capacity proof | Evidence used | Room before administrative reserve |
| --- | ---: | ---: |
| AF pair | 90,865,987 | 34,963,133 |
| Probe pair | 51,027,623 | 74,801,497 |

Each kept the **134,217,728-byte** cap and **8,388,608-byte** reserve, with zero overage.
Every archive decompressed to its admitted original hash; reusing the baseline added zero
bytes. These are separate proofs: all four archives together total **141,891,932 bytes**,
exceeding the **125,829,120-byte** evidence room. Mixed selections can require `referenced`
evidence rather than retaining every executable.

`readelf` showed DWARF v4, `.debug_info`, `.debug_line`, inlined call locations, `.symtab` and
`.eh_frame`. `nm`/`addr2line` resolved `af::main` at `main.rs:1176` and `perf_probe::main` at
`perf-probe.rs:31`. `objdump` showed `push %rbp; mov %rsp,%rbp` in both. The report supplied no
binutils versions or expanded inspection argv; none are invented here.

## Prior commands, tests and limits

The supplied native proof command was:

```sh
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=scripts/performance python3.14 -B target/diagnostic-proof/native-proof.py
```

That uncommitted proof script used Builder's locked/offline release commands for AF, perf-probe
and perf-supervisor with the declared flags. The script and raw receipts/logs are not inputs to
this continuation. The unchanged Builder constructs
`cargo build --release --locked --offline --target-dir <worktree>/target --target <recipe-target> -p <package> <target-kind> <name>`,
then the declared `--features` when present; this is a source-derived command template, not a
claim that an unavailable expanded argv was recovered. No private host path is required here.

The prior report records:

- `PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=scripts/performance python3.14 -B -m unittest tests.test_builds tests.test_diagnostics tests.test_report_diagnostics`:
  128 passed during implementation; final XZ diagnostics/report run: 98 passed. An intermediate
  import error was restored to the original gzip import, verified alone, then rerun.
- `python3.14 -B scripts/performance/schemas.py`: passed at that time without schema changes.
  Python 3.11 syntax admission passed; no Python 3.11 runtime was available.
- Unchanged `harness-digest --write`/`--check`: passed; prior digest
  `sha256:09dafdb30182aea9781428cd510f91204b0beeb64900f1f615ac455299c182ec`.

The cold distinct build took **804.872 seconds**. AF archive compression/verification took
about **101/105 seconds**, probe archives **53/37 seconds**, overlapping compilation. These
separate proofs do not establish an end-to-end profile within its unchanged 15-minute default.
No actual perf capture, symbolization, flamegraph, Docker qualification or hosted timing verdict
ran in that proof. It did not claim full native/Markdown gate or independent review acceptance.
The continuation request separately reports a prior full gate of 974 Python and 3,675 Rust tests
with 15 existing skips and no reruns; that is not this worker's current gate result.

## Coordinator #357 failure and new continuation evidence

The supplied coordinator report for the same public candidate independently built both source
sides: all five executables plus manifest totaled **940,299,242 bytes**, below the same cap.
Perf **7.0.14** decoded **305 `task-clock:u` capability samples**. The first medium baseline
capture then failed because automatic build-ID-cache population hard-linked the admitted probe
inside private HOME. Cleanup correctly returned `cleanup_uncertain`, retained scratch and
refused subsequent captures. There were **zero usable captures/flamegraphs**. Wall time was
**1,340,904 ms**, below an explicit **1,800,000-ms** budget. These figures describe a failed
run, not successful diagnostic qualification. Its original evidence/scratch are preserved
outside this sandbox and have not been touched.

New behavior follows [ADR-0224](../adr/0224-bind-diagnostic-symbol-archives-and-avoid-perf-cache-links.md).
The continuation's behavioral tests use recorder/probe/renderer fixtures, not authorized perf.
They run the actual Profile writer through collection and report admission for shared/distinct
archives, missing/unrelated/corrupt/truncated/concatenated/trailing evidence, exact compressed
size/hash, decompressed binary identity, strict states/version and resource bounds. A real
fixture hard link reproduces cleanup refusal without `--no-buildid-cache`; both capture paths
clean up with it, capability uses it, and hostile links remain refused. Build IDs, source-line
attribution policy and unknown-frame/loss semantics remain intact. The upstream
[Linux perf-record documentation](https://github.com/torvalds/linux/blob/v6.12/tools/perf/Documentation/perf-record.txt)
confirms that `--no-buildid-cache` preserves IDs in `perf.data`; `--no-buildid` discards them.

Current continuation checks used Python 3.14.4; `rustc` and `cargo --version` confirmed the
pinned 1.88.0 tools. The broader focused command was:

```sh
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=scripts/performance python3.14 -B -m unittest tests.test_builds tests.test_diagnostics tests.test_report_diagnostics tests.test_contracts tests.test_history tests.test_archive tests.test_telemetry_diagnostics
```

It passed **192 tests**. Two subsequently added collection-boundary tests passed separately.
The final focused selection below includes those tests and passed **26 tests**. These selections
overlap and are not summed as separate unique test counts.

```sh
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=scripts/performance python3.14 -B -m unittest tests.test_contracts tests.test_diagnostics.PerfCache tests.test_diagnostics.Symbols tests.test_report_diagnostics.RetainedSymbols
```

Schema generation parity, Python 3.11 syntax admission, change-note validation and the new
relative documentation links passed. Python 3.11 runtime is unavailable. The unchanged inventory
generator's `--write`/`--check` passed in an owned source copy without the sandbox's empty,
read-only `.git`; only its two declared inventory outputs were copied back. Direct generation
in the sandbox refused its empty Git metadata. No generator logic changed.

Initial test failures were implementation/test-fixture errors: the archive record initially
omitted `path`; a new CLI fixture named an undeclared case; and a whole-command assertion
expected cleanup prose that an existing later refusal replaces. These were corrected, each
failed case was verified alone or in its affected selection, then the final focused selection
was run. Cleanup authority and diagnostic runtime were not changed to satisfy an assertion.
`cargo +1.88.0` was not supported by the direct Cargo binary, and the rustup wrapper could not
write its read-only home; the direct pinned tool version checks succeeded. Markdownlint was
not installed locally; a hanging `npx --no-install` attempt was stopped without a lint result.
The coordinator's full Markdown gate remains required. No fresh native build was necessary
because binary-affecting inputs stayed unchanged.
Actual coordinator capture/symbolization/flamegraph proof with pinned tools, the unchanged full
kernel/Markdown gate, Terra/Astra reviews and Astra evaluation remain independent obligations.

## Typed finding dispositions

- `bugs-1` (`sha256:f6cba46ae43f10db4f2c7eebc835011005883fded00f4c187f0ae75dda548383`):
  addressed by profiles/6 archive records and bounded collection/report admission, including
  actual writer-to-admission regressions. Independent acceptance remains pending.
- `evaluator` (`sha256:f004c85ade37d0d6855da0b90a8ef165211a28765b24bd4bb3d5d9813c7eeb70`):
  addressed by ADR-0224, this attributed executed-evidence report and the unchanged-input checks.
  This does not inherit the rejected candidate's acceptance or claim a fresh native build.
- The demonstrated #357 blocker is addressed by explicit cache avoidance on both recording
  paths and capability, with positive and hostile-link behavior checked. Actual perf proof is
  still coordinator-owned. Unrelated findings remain follow-ups.
