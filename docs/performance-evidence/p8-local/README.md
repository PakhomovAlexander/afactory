# p8 closeout: one real local run of the performance workflow

The retained evidence of
[ADR-0187](../../adr/0187-bind-kernel-receipts-to-their-task-results-word-reports-by-what-was-admitted-and-retain-a-real-local-run.md):
the documented laboratory workflow, executed for real on this Task's macOS arm64 host in the
named `macos_arm64_local` environment. The products were built from source, the samples ran
under the authority supervisor, and the oracles judged every sample. Nothing here came from
`synthetic.write_run` or a stand-in binary.

It is functional local evidence of the workflow. It is not a product speed claim, a calibrated
sensitivity, a shipping-budget demonstration, a Linux or contained result, or a hosted run. The
comparison is same-source (A/A): baseline and candidate are one commit and one build.

## What was measured, and when

- **Pre-sealing source.** The run measured this Task's source before it was sealed: every file of
  the sandbox except its build `target/`, committed once into a task-owned Git repository
  (`$WORK/repo`) with a fixed author and date. Commit `acc14e9a77ff69381a521eea1662e80b18b23574`,
  Git tree `3d1c9c780f55ceda85361a916b7dbe229e1b3347`, 1,906 files, protected harness
  `sha256:d6c26e84e05c086e98238a71128c637051e4b38189180e6244622cce01bf8760`.
- **Later differences.** After the run only documentation and this directory changed; no code,
  registry, fixture, schema or harness file did. The sealed Snapshot did not exist when the run
  happened. [`measured-source.json`](measured-source.json) records the measured fingerprints and
  every path whose bytes differ between the measured commit and the tree this directory was
  finalized in (see "Reconciling").
- **Host.** Apple M3 Max, 14 cores, 36 GiB, macOS 27.0.1 (Darwin 27.0.0), Python 3.9.6, rustc
  and cargo 1.88.0 (the repository pin), Git 2.54.0, Chrome 155.0.8059.85. The host was shared
  with other work; load averages were 11 to 29 around the shards (`checks/load-*.txt`).
- **Dependencies.** The locked dependencies were fetched once into the task-owned Cargo home with
  `cargo fetch --locked --target aarch64-apple-darwin`, from a pre-sealing tree whose `Cargo.lock`
  is the measured one (`sha256:7ffe05d928f8f4c1b46f6d92e5e207d30a38768e05a1e7aa25401fc364b2939a`);
  it exited 0 in about 19 s (`console/fetch.err`). Every build then ran `--locked --offline`.

### Earlier attempts

- **A first complete run** measured an earlier pre-sealing source (commit
  `facee803264a9e274a82bd25f8cf5b132c6e540d`, harness
  `sha256:9724a48bc34e3cca60fd413210c6d45d0b2d522ce883004a6eefe00fcac397fd`). Its first plan
  command named the quick tier, which has no macOS profile, and was refused with exit 2 before
  anything ran. The nightly-tier plan then ran every step below with exit 0, apart from its
  rerun's `aggregate`, which exited 4: under load averages near 80 the rerun's CLI case was
  inconclusive (medians 28.81 and 35.69 ms). The report wording was then corrected for a shard
  that ends without completing although every record was admitted, so the code changed. That
  run is superseded and not retained; this directory holds only the run of the final code.
- **A second attempt** was stopped by hand during its build: its snapshot had included the first
  run's evidence directory. Nothing it produced is used.
- **The native kernel integration**
  (`guarded_kernel_experiments_measure_one_judged_sample_and_refuse_every_tampered_harness`) ran
  once on the first run's source and passed in 761.22 s. Its exported
  `af.perf-kernel-receipts/3` document, 11 Tasks that its own schema and reader check, is
  [`kernel-receipts.json`](kernel-receipts.json) (`checks/native-kernel-test.txt`). The mandatory
  gate runs that test again on the sealed source.

## Steps and exits

Every step ran from `$WORK/repo` with a private `HOME`, XDG directories and `TMPDIR` below
`$WORK`, as [`tools/workflow.sh`](tools/workflow.sh) runs it. [`steps.jsonl`](steps.jsonl) holds
each step's exact argv, exit status and wall time; `console/` holds its stdout and stderr. No
step was retried.

| Step | Command | Exit | Seconds |
| --- | --- | --- | --- |
| capabilities | `cli.py capabilities` | 0 | 0.4 |
| list | `cli.py list` | 0 | 0.4 |
| plan | `cli.py plan --trigger manual --scope partial_scope --tier nightly --environment macos_arm64_local --shards 1 --pairs 20 --seed 20261021 --run-id p8_local_aa`, three `--case`, base = head = control = authority = the measured commit | 0 | 0.6 |
| build | `cli.py build --plan run.json --cargo-home $WORK/cargo-home` | 0 | 123.5 |
| run-shard-0 | `cli.py run --shard 0 --trusted-local --scratch $WORK/scratch` | 0 | 9.5 |
| aggregate, compare | `cli.py aggregate`, `cli.py compare` | 0, 0 | 0.4, 0.4 |
| profile-trace | `cli.py profile --recipe macos_arm64_trace --mode trusted_local --case cli_discovery__command_version --case canonical_validate_small__package_small_envelope --parent-build ... --total-minutes 30` | 0 | 146.8 |
| report | `cli.py report --run $WORK/run --diagnostic $WORK/diag` | 0 | 0.5 |
| archive | `cli.py archive --init` (2 GiB), `--import $WORK/run`, `--status` | 0, 0, 0 | 0.4 each |
| history | `cli.py history --inputs $WORK/archive` | 0 | 0.4 |
| reproduce | the report's own recompute, report and rerun commands, placeholders filled (`checks/reproduce.argv`) | 0, 0, 0 | 0.4, 0.4, 0.6 |
| rerun | `cli.py build` and `run` of the reproduced plan, `aggregate`, archive import and status, `history` of both runs | 0 each | 159.3, 9.1, 0.4, 0.4, 0.4, 0.5 |

## Results

**Capabilities.** `macos_arm64_local` and trusted local mode were available, for discovery and
nonspawning library cases. Both Linux environments and `linux_container` were unavailable: this
host is macOS arm64, and no verified disk-bounded scratch filesystem is configured
(`console/capabilities.out`). Linux-only cases, Linux perf captures and contained runs therefore
stay unavailable here. No perf, samply or inferno executable is configured on this host, and none
was installed, so no CPU profile or flamegraph was attempted.

**Plan.** `p8_local_aa`, plan digest
`sha256:8a238df755d3131ba53c590bbed314181d8d519aabd702979a1c1fcb355c1480`: partial scope of the
nightly tier's `macos_arm64_local` profile with recipe `macos_arm64_native_release`, authority
mode `proposed` (no activation exists), one shard, 20 measured and 2 warmup pairs per case, 5
start and 5 end sentinel pairs. Three critical cases the registry declares supported here:
`canonical_validate_small__package_small_envelope`,
`cas_typed_get_small__package_small_envelope` and the CLI case
`cli_discovery__command_version`.

**Build.** Supervisor, `af` and `perf-probe` built offline and locked; the candidate shares the
baseline build because both are one commit. `af`
`sha256:d218192f722cb1f9c933e705c024ffc8996e77445a8f10d7f4aed5c0a17b3831`, probe
`sha256:b9dd767cc401239208856202f5344bfb960e92877372667d1f375e50e3f9d515`, supervisor
`sha256:3fcc448e667ec5dae0fbbeafd6df8639189ebf85c6f4fba65961e15b4db00bcd`, embedded commit
verified (`report/evidence/builds.json`). The build recorded that an ambient Cargo
configuration above the checkout set `build.target-dir` and was overridden by `--target-dir`.

**Shard and verdict.** Shard 0 completed with 152 of 152 planned records and no failed sample
(samples `sha256:cc0bd52f126092c0da8edb9295eb999b16a66fadc6ac6b97cca6db593f07797a`); health was
healthy. The verdict is `pass` (exit 0) with measured evidence. As a partial-scope,
proposed-policy run it does not satisfy the required check, and the report says why.

| Case | Status | Baseline median (ms) | Candidate median (ms) | Interval [L, U] (exact) |
| --- | --- | --- | --- | --- |
| `canonical_validate_small__package_small_envelope` | within tolerance | 0.0256455 | 0.02525 | [-1459/1000000, 2041/1000000] |
| `cas_typed_get_small__package_small_envelope` | within tolerance | 0.0835415 | 0.084521 | [-2813/500000, 23/4000] |
| `cli_discovery__command_version` | within tolerance | 6.224729 | 6.2561245 | [-292833/1000000, 244167/1000000] |

Intervals are in units of each case's tolerance (relative 0.05, absolute 1 ms). Every raw point
in planned order and the shard's host record are in [`report/`](report/report.html) and
`report/evidence/`.

**Diagnostic child run.** `p8_local_trace` (plan digest
`sha256:4d1a99f9a0bdc4add0cfc2629f54519dc34257d853755be174eb7475d693ce3f`, scope `diagnostic`,
recipe `macos_arm64_trace`) built the default-off `perf-trace` products and recorded four phase
traces, both sides of both selected cases, each with complete region coverage, no dropped or open
span and no omission. Against the parent's build, the median paired overheads were 4.5% to 8.9%,
so every trace is labelled `perturbing` (target 2%). On this shared host that is an observation,
not a trace-overhead qualification. The report links the child, and its verdict effect is `none`
(`report/diagnostics/p8_local_trace/`).

**Report, links and browser.** The report's first section is the verdict, with no case listed as
failed or incomplete. Before export, in the work directories, every link of the report, its
diagnostic page and both history pages was a relative path to a regular file below its own
directory, and each of the 7 listed downloads had its recorded SHA-256 (`checks/links.json`, 36
links). That check is kept as recorded; it describes the work directories, not this one. The
first export then left out the original run's history copy, so the retained `history/history.html`
linked to a missing `runs/p8_local_aa/report.html` until ADR-0188 restored it (see "Repair after
publication"). The same check of this directory as retained, `checks/links-exported.json`, finds
the same 36 links resolving, with every bundle manifest matching. Headless Chrome 155, with a fresh profile,
the browser sandbox and the page's CSP, opened the real measured `report/report.html`. Its first
section was the verdict, of 10. The filter showed 190 searchable rows, 0 for an absent text, 190
when cleared and 3 for `cli_discovery`. The 3 disclosures loaded closed and each opened on a
click. No console or CSP message appeared and the report directory did not change. That page
interaction completed. Separately, after `Browser.close` Chrome exited 2 after 10.5 s with
`Teardown watchdog expired`, and no process of that profile remained (`checks/browser.json`).
The shutdown outcome says nothing about the page.

**Archive and history.** The archive admitted `p8_local_aa` with 8 members and reported health
`ok`. History materialized the run from the archive's rehashed objects and drew 1 run in 3
compatible groups.

**Reproduction.** The report's three reproduction commands were run as printed, with only their
placeholders filled (`checks/reproduce.json`). `compare` recomputed a byte-identical verdict
(`sha256:ec1787ecceac6219de9e91e45fa326dd32ad506a23accc1dd32c838e653ade40`, also the
aggregate's and the report's). The regenerated report has the same verdict and the same
`report.json` apart from the linked diagnostic child it was not given, and the same summary
before its diagnostics section. The rerun plan differs from the original only in `run_id`,
`parent_run_id` and `plan_digest`. That plan, `p8_local_aa_rerun` (plan digest
`sha256:ac7ab4382c9f4d299b36130e663b9bd7a706fdf0f061609c564db2bc4c8d27bc`), was then built and
executed: 152 of 152 records, healthy, `pass` with every case within tolerance. The archive
admitted it, and history drew both runs in the same 3 compatible groups (`history/`). The same
experiment reproduced; its timings are its own.

**Hashing observation.** `kernelmeasure.sources_digest` over a `git archive` export of the
measured commit (1,906 files, 21,285,119 bytes) took 107.8 to 146.3 ms in five repetitions,
median 117.4 ms, at a load average near 26 (`checks/sources-digest-timing.json`). It is one
observation of this size on this host and bounds nothing else.

## Contents

- `report/` — the measured report exactly as `cli.py report` wrote it: `report.html`,
  `report.json`, `summary.md`, `verdict.json`, the complete run bundle and its manifests below
  `evidence/` (`run.json`, `builds.json`, `shards/0/samples.jsonl`, `shard.json`), and the
  diagnostic child bundle below `diagnostics/p8_local_trace/`.
- `history/` — the two-run history and both runs' materialized reports and bundles, so the
  history opens both runs directly. The original run's bundle is byte-identical to
  `report/evidence/`.
- `omitted.json` — kept as the first export wrote it: the ten files of the original run's history
  copy that it listed instead of copying, with each file's size and SHA-256. Those ten files are
  now present at those paths with exactly those hashes (`checks/links-exported.json`).
- `kernel-receipts.json` — the native integration's real `af.perf-kernel-receipts/3` export.
- `steps.jsonl`, `console/`, `checks/` — the step records, console output and check results.
  `checks/links-exported.json` and `checks/p9-repair-reconciliation.json` were added by ADR-0188,
  and `checks/p10-repair-reconciliation.json` by ADR-0189. Every other check is as the run
  recorded it.
- `tools/` — the scripts that ran the workflow, the reproduction and link checks, the browser
  check, the fingerprints, this export and the history restoration. `workflow.sh`, `export.py`
  and `checks.py` are the ADR-0188 revisions, and `fingerprint.py` is the ADR-0189 revision (see
  "Repair after publication").
- `measured-source.json` — the fingerprints of the measured commit and the reconciliation.

The bundles are byte-exact, so they keep what the laboratory recorded about this host: the
rustup toolchain and ambient Cargo configuration paths in `builds.json` and the host name in the
child's `profiles.json`. No binary, build target, credential, Cargo home, private Worker log or
Git repository is kept. Console output and checks name host paths as `$WORK` and `$REAL_HOME`.

## Regenerating

On a macOS arm64 host with the pinned toolchain, from a checkout of the measured source
(`SOURCE`) and a new task-owned work directory (`WORK`):

```sh
bash tools/workflow.sh SOURCE WORK snapshot    # copy without .git, commit in a fresh repository
bash tools/workflow.sh SOURCE WORK plan        # capabilities, list, the frozen plan
mkdir -m 700 WORK/cargo-home && CARGO_HOME=WORK/cargo-home cargo fetch --locked --target aarch64-apple-darwin
bash tools/workflow.sh SOURCE WORK build
bash tools/workflow.sh SOURCE WORK run
bash tools/workflow.sh SOURCE WORK analyse     # aggregate and compare
bash tools/workflow.sh SOURCE WORK profile     # the phase-trace child run
bash tools/workflow.sh SOURCE WORK report
bash tools/workflow.sh SOURCE WORK archive     # archive and history
bash tools/workflow.sh SOURCE WORK reproduce   # the report's own reproduction commands
bash tools/workflow.sh SOURCE WORK rerun       # execute the reproduced plan, archive, history
python3 tools/checks.py reproduce WORK/run.json WORK/rerun.json WORK/report WORK/report-repro WORK/verdict-aggregate.json WORK/reproduce.json
python3 tools/checks.py links WORK/report WORK/history-2 WORK/links.json
python3 tools/browser_check.py CHROME WORK/report/report.html WORK/chrome-profile WORK/browser.json
python3 tools/export.py WORK NEW_EVIDENCE_DIR REAL_HOME tools   # then checks the exported directory
```

`WORK/repo` must not exist before `snapshot`. Each stage stops at the first command or step that
fails and exits with its status, after recording that step in `steps.jsonl`; a nonzero verdict
exit ends its stage too. A new run has a new commit identity, other timings and possibly another verdict. Only the frozen
plan's selection is reproducible. `cli.py report --run report/evidence --output NEW_DIR`
regenerates a report from the retained bundle.

## Reconciling

`tools/fingerprint.py` lists every regular file of a tree except a top-level `target/` and
`.git/` as `path\0size\0sha256\n` lines sorted by path. A digest is the SHA-256 of a set of lines.
`measured-source.json` holds the measured commit's digest of all files and of each top-level
entry, its protected-harness digest, the SHA-256 of the registry, golden, lock, toolchain and
probe files, and `reconciliation`: each path whose bytes differ between the measured commit and
the finalized tree, with both sizes and hashes, and the digest of every other file, which must be
identical. This directory itself is new and is not listed. To check a sealed tree, run
`python3 tools/fingerprint.py verify SEALED_TREE measured-source.json`: it exits 0 when every
unlisted file is byte-identical to the measured commit and every listed path holds its finalized
bytes. That describes the tree this directory was first finalized in. The ADR-0188 repair
changed more files afterwards, so a later tree is checked against the chain:
`python3 tools/fingerprint.py verify-repair SEALED_TREE measured-source.json
checks/p9-repair-reconciliation.json` exits 0 when every file that neither record lists is
byte-identical to the measured commit and every path the repair lists holds its repaired bytes.
That describes the P10 tree. The ADR-0189 repair changed the source again, so the current tree
is checked with `python3 tools/fingerprint.py verify-repair SEALED_TREE measured-source.json
checks/p10-repair-reconciliation.json`. That record lists every path both repairs changed
relative to the first finalized tree. `tools/fingerprint.py rerepair` computed it from the exact
P10 tree, which `checks/p9-repair-reconciliation.json` verifies. `measured-source.json` and
`checks/p9-repair-reconciliation.json` are unchanged, and `measured-source.json` still names
only the measured source.

## Repair after publication

[ADR-0188](../../adr/0188-validate-kernel-receipts-as-the-kernel-does-and-restore-the-retained-history.md)
repaired this directory and its tools after publication. It measured nothing again.

- **History restored.** `tools/restore_history.py` imported the two retained run bundles,
  `report/evidence/` and `history/runs/p8_local_aa_rerun/evidence/`, into a new archive and drew
  the history with the laboratory's own `archive` and `history` commands. It regenerated every
  retained history file byte for byte. It also regenerated the ten files `omitted.json` lists,
  each with its listed size and SHA-256, and only then copied them into
  `history/runs/p8_local_aa/`. Nothing retained was rewritten. `checks/links-exported.json` is
  the post-export check of this directory: 36 links, no failure, the 7 downloads, every manifest
  entry and the ten restored files.
- **Tools.** `workflow.sh`'s snapshot stage no longer copies a checkout's `.git` directory or
  worktree file. It refuses an existing `WORK/repo` and stops on any failed copy or Git command,
  and every stage ends at its first failing step. Earlier, a clean checkout's own Git repository
  was copied, the commit had nothing to commit, and the stage still reported the checkout's
  `HEAD`. The measured run's source was a Task sandbox, which holds no `.git`, and every one of
  its steps exited 0 (`steps.jsonl`). `export.py` now keeps every page the history links and checks the exported
  directory itself.
- **Fingerprints.** The measured commit, its tree, harness digest and every measured runtime
  identity are as recorded. Raw run, build, trace and native bundles are byte-exact.
  `checks/p9-repair-reconciliation.json` lists each path the repair changed relative to the first
  finalized tree, with both sizes and hashes, and the digest of every other file. That includes
  the receipts reader, its fixtures and the protected harness, now
  `sha256:69bddb0deca5051b56f9bf63a9ba12243c2b5b220fa322bb11b06a909903dd31`. Of the code the
  run executed, only `scripts/performance/perflab/contracts.py` changed, and only in the kernel
  receipts reader, which no step of the run calls. No registry, golden, probe, supervisor,
  Cargo or toolchain file changed. The repaired reader still admits the receipts in
  `kernel-receipts.json`.
- **Set order (ADR-0189).**
  [ADR-0189](../../adr/0189-read-a-task-result-s-sets-in-the-kernel-s-order-and-reconcile-a-second-repair-through-the-first.md)
  changed only the kernel receipts reader, its fixtures and tests, the protected harness pin and
  documentation. The reader now refuses a Task result set that is not strictly ascending. No
  step of the run calls that reader, so nothing was measured again. This directory changed only
  in `tools/fingerprint.py`, which gained `rerepair`, in this README and in the new
  `checks/p10-repair-reconciliation.json`. The repaired reader still admits the receipts in
  `kernel-receipts.json`.
- **Limit.** The measured commit's Git repository is not retained. Neither are its bytes of the
  two documentation files that changed before finalization; `measured-source.json` keeps only
  their sizes and hashes. The original commit cannot be checked out again. A rerun from a copy
  of any later source measures a new source identity, not the original commit.
