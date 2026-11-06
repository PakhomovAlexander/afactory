# Issue #697 native capture investigation

## What I saw

Native macOS FSEvents can deliver setup and prior-attempt activity during a stable capture
retry. The original traced execution had equal index and worktree passes on attempt two,
but its watcher still reported a change, forcing an incorrect third attempt. Command-sandbox
passes were misleading because native events were not delivered there.

The two-second observer reproducer and original settled/untracked/index assertions are
unchanged. Production still allows at most three attempts with a 250 ms boundary wait.

## Cause and correction

Inspection of the locally installed notify 8.2.0 `src/fsevent.rs` established that it starts
with `kFSEventStreamEventIdSinceNow`, latency zero and `NoDefer`, discards native event IDs,
and exposes neither its stream handle nor a flush operation. `SinceNow` does not establish
an attempt-local delivery barrier for delayed/coalesced filesystem activity. Retaining one
monitor and draining delivered batches previously failed under Rust 1.88 and was reverted.

Apple documents a synchronous delivery barrier in
[`FSEventStreamFlushSync`](https://developer.apple.com/documentation/coreservices/1445629-fseventstreamflushsync),
but it requires the stream handle. The correction now uses one attempt-local root kqueue
vnode watch through nix's safe API, with checked registration receipts, and complete before/after
metadata observations of descendants (including ignored paths and Git administration).

The rejected per-path candidate retained one descriptor for every descendant and recursed with
path-based `read_dir`. Its descriptor requirement therefore exceeded ordinary process limits,
and `O_SYMLINK` on a child did not protect intermediate components during recursive path lookup.
The replacement keeps a relative-directory stack, reopens each component from the held root
with `openat(O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`, enumerates through a held parent descriptor,
and promptly drops each event-only child descriptor after reading metadata. Live descriptor
count is constant even with deep trees. The root itself is opened no-follow and watched for
replacement. Full path-keyed observations compare change stamps, mode, ownership, link count
and size. Access time is excluded; native index reads can update atime without a mutation.
True resource exhaustion and syscall errors still fail closed.

Every initial metadata read precedes both content scans and index checks; every final metadata
read follows them. Each path's compared interval therefore contains the whole content/index
read interval. Descendant stamps detect content/metadata ABA, directory stamps detect transient
names, and the root watch detects replacement of the held root. The unchanged content scans
and index fingerprints remain independent checks. No event stream synchronization prototype,
extra attempt, raised descriptor limit or longer boundary wait is introduced.

See [the ADR](adr/2026-11-06-use-vnode-boundaries-for-macos-capture.md) for the interval argument
and traversal safety. Deterministic native race hooks substitute an external symlink after a
directory is opened and while an intermediate component is being reopened. They assert actual
original-directory enumeration, no external-only entries and a refused replaced-component
lookup; the ordinary monitor also invalidates the replacement.

## Current bounded-handle candidate

The old logs below describe earlier candidates, not this source. The lost Task4 implementation
and its results are informational only. This Task reconstructs and independently executes the
bounded-handle implementation. Current execution evidence is recorded in
[the bounded-handle log](issue-triage-evidence/issue697-bounded-native.log), SHA-256
`d5d6077080e99723dd4d7dd0e90bdf8484d75646f83f4016e9b4897bc69b6a1b`.

Fresh native execution outside the command sandbox, built with Rust 1.88.0 and a private
Cargo home/TMPDIR under this checkout's target, passed 46 library tests and 41 integration
tests. The one existing manual performance measurement remains ignored; no test was newly
skipped or quarantined. Three additional exact delayed-event runs passed, each retaining the
two-second observer, exact two-attempt assertion and settled-content proof. The log records
commands, source and executable digests, complete output and all exit statuses. This includes
the unchanged untracked-creation/index/ABA assertions and the new low-descriptor capture and
deterministic traversal races. The low-descriptor child asserts its inherited limit is still
64 after capturing 256 source files, 256 ignored directories and an 80-level ignored tree.
True exhaustion is tested both before root open and after root/queue allocation; both fail
closed, and partial startup releases its descriptors.

Final pinned Clippy (`--all-targets --offline -- -D warnings`), formatting, changelog validation,
ADR discovery and markdownlint-cli2 0.22.1 on the three Markdown files passed. All nine Python
kernelguard `Bootstrap`/`Staged` tests passed with `-I -B`. An initial compilation failed with
E0599 because nix does not name macOS `O_EVTONLY`/`O_SYMLINK` constants; the final implementation
passes those libc flag bits through nix's safe `OFlag::from_bits_retain` interface. No native
test failed in this Task.

The original catalog was restored byte-for-byte with SHA-256
`a2144f92644d30100a13ea19d65dc25cd7399923e44233b74aafd8984ac75645`, and the temporary bootstrap
was removed. Harness pins regenerated to
`sha256:1c539ab523d06a81e4132c492279830f4e062a55b7441d7d676a746486800322`; a second regeneration
was byte-identical and `harness-digest --check` passed. After all executable tests finished,
this Task removed its `target/issue697-tmp`. Full no-follow/lstat scans of target before and
after cleanup found no FIFO, socket, device or other special files. No external sandbox was
modified. No release version, workflow or publication changed.

The required complete local fast gate and independent bugs/correctness/evaluator decisions
remain for the sealed-candidate pipeline. The implementer has not duplicated that gate or
claimed the independent build/private staging and native correctness run on its behalf.

## Historical per-path candidate execution evidence

The historical per-path candidate tests were built with `rustc 1.88.0 (6b00bc388 2025-06-23)`, a private
`CARGO_HOME=target/issue697-cargo`, the checkout's own target, and private
`TMPDIR=target/issue697-tmp`. Test executables were run through the authorized native execution
capability, outside the command sandbox, with actual macOS vnode/FSEvents delivery.

- Before changing production code, native `it-1cc9f140965b3a39 capture:: --test-threads=1
  --nocapture` produced 20 passes, one failure and one existing ignored manual measurement.
  `an_index_change_alone_invalidates_a_pass` reported three attempts instead of two. The delayed
  and untracked tests passed in that particular run; earlier native runs failed them as below.
  [The baseline log](issue-triage-evidence/issue697-capture-native-before.log) has SHA-256
  `679bb77a1f2731b4fecf628cf3785f4c9a5e42ffda8d8a2cbed3c75fcda68e02`.
- Final `cargo test -p review-source-git --no-run --offline` exited zero. Running both emitted
  executables with `--test-threads=4 --nocapture` natively exited zero: 42 library tests passed;
  40 integration tests passed, with the existing release measurement ignored. This includes
  the unchanged delayed-event, settled, untracked, index-only, mutation/ABA, hostile-Git,
  symlink, content and refusal tests. The six new direct monitor tests include transient
  creation across registration receipts, metadata ABA, read access, dangling/external links,
  missing roots, and descriptor exhaustion/cleanup in a child limited to 64 descriptors.
- Three additional exact native runs of
  `capture::a_settled_worktree_does_not_replay_a_previous_attempts_events` all exited zero,
  retaining the two-second observer, exact two-attempt assertion and settled-content proof.
  [The final log](issue-triage-evidence/issue697-capture-native-after.log) records commands,
  executable digests and statuses; its SHA-256 is
  `144308627cc2879311a005a88a17ef98111f211e6bc365e87c4ce74b7914ab4a`.
- Pinned `cargo clippy -p review-source-git --all-targets --offline -- -D warnings` exited zero.
  The first invocation selected Homebrew's incompatible clippy driver and failed with E0514;
  putting the complete 1.88.0 toolchain `bin` first on PATH corrected that invocation.
- Python 3.15 kernelguard `Bootstrap` and `Staged` selections passed all nine tests. The first
  run omitted `-B`, generated bytecode and correctly failed the CLI's trusted-source refusal.
  Removing only those generated caches and rerunning with `-I -B` passed. No refusal changed.
- The original catalog was restored byte-for-byte (SHA-256
  `a2144f92644d30100a13ea19d65dc25cd7399923e44233b74aafd8984ac75645`) and the entire temporary
  `devrec-impl-bootstrap` removed. Protected-harness pins were regenerated and checked; the
  second regeneration was byte-identical and the final check exited zero. The digest is
  `sha256:1c539ab523d06a81e4132c492279830f4e062a55b7441d7d676a746486800322`.
- Rust 1.88 `cargo fmt --all -- --check`, changelog validation, ADR discovery and the cached
  markdownlint-cli2 0.22.1 executable on the three changed Markdown files all exited zero
  (markdownlint: three files, zero errors).

An initial native run of the new backend failed 15 of 41 integration tests: conflicting
`O_NOFOLLOW | O_SYMLINK` flags refused links, and index read-atime notifications invalidated
stable attempts. Both implementation mistakes were corrected before the final passing runs.
The final implementation contains no temporary event tracing.

## Earlier evidence and pipeline acceptance

The original trace showed `changed=true`, equal index fingerprints and unequal passes on
attempt one; `changed=true` with equal index and worktree fingerprints on attempt two; and
`changed=false` with equal fingerprints on attempt three. Its diagnostic log SHA-256 was
`288c3f87ab0d13933c76b63b0c4f98a68691f42c4f90d2845d343767bbbeac93`.

The earlier single-monitor/batch-drain experiment initially passed 26 native tests, then
failed the delayed regression under pinned 1.88 (25 passed, one failed). After its reversion,
the previous native selection passed 21 and failed the delayed/untracked exact-two assertions.
The previous full fast gate stopped in CI-disk (three failures, three errors) because its
TMPDIR overlapped protected source/target; e2e/uix did not run.

Per this implementation Task's instructions, the complete kernel/markdownlint fast gate runs
after seal; it has not been duplicated here. Its check-runtime TMPDIR must remain outside
protected source/target. Independent bugs/correctness/evaluator acceptance, including an
independent build/private stage and native executable tests of the sealed candidate, remains
for the unchanged required pipeline. No remote gate or publication was performed.

## Repro

Put the entire installed Rust 1.88.0 toolchain `bin` first on PATH (including cargo-clippy),
use a private writable Cargo home and TMPDIR, and build in this checkout's target:

```sh
cargo test -p review-source-git --no-run --offline
cargo clippy -p review-source-git --all-targets --offline -- -D warnings
```

Execute the emitted library and integration binaries natively with `--test-threads=4
--nocapture`. Also run the emitted integration binary with `--exact
capture::a_settled_worktree_does_not_replay_a_previous_attempts_events --nocapture`.
Alternatively, use a native Rust 1.88 nextest invocation:

```sh
cargo nextest run --profile ci -p review-source-git \
  -E 'test(capture::a_settled_worktree_does_not_replay_a_previous_attempts_events)' --no-capture
```

A command-sandbox-only pass does not establish native watcher correctness.
