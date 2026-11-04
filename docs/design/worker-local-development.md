# Worker local development

This design separates local development from the project Gate. Step 1 introduces a pinned
Worker package companion and host-recorded command evidence. Step 2 adds explicit immutable
cache carry. Step 3 adds bounded knowledge retrieval and private Notes. Step 1 is governed by
[ADR-2026-11-03-worker-development-environments](../adr/2026-11-03-worker-development-environments.md),
with aggregate storage enforced by
[ADR-2026-11-04-bound-worker-storage-in-one-private-tmpfs](../adr/2026-11-04-bound-worker-storage-in-one-private-tmpfs.md).

## Step 1: an explicit environment for one Attempt

A captured Task Worker may include `development.toml`, schema `af.worker-development/3`, beside
`worker.toml`. It is part of the existing package digest and Execution Binding's package pin.
The `af.worker/1` manifest, Task context and effective binding shapes do not change. The new
companion has its own schema, strict reader and validation; unknown versions and fields fail
closed. A package without the companion follows its existing execution path.

The companion declares:

- A symbolic harness and tools, each with the plain SHA-256 of its installed image executable.
  The machine binding also pins the complete image by digest, including tool libraries and
  dependencies. Cargo is an example configuration, never a runtime field or implicit default.
- Environment variables: literals or one symbolic path with canonical relative components.
  `${source}`, `${state}`, `${scratch}`, `${build}`, `${cache}`, `${knowledge}` and
  `${dependencies}` are the only path roots. Absolute host paths, parent traversal, alternate
  separators, interpolation, reserved environment overrides and disabled-state references are
  refused. Source symlinks are refused before tool execution.
- Writable roots chosen from `source`, `state`, `scratch`, `build`, `cache` and `knowledge`.
  The remaining roots are read-only. State, cache and knowledge are either `attempt_private`
  or `disabled`; absent a cache policy no root is carried between Attempts. Cache continuation
  requires explicit same-slot admission; independent reviewers never share writable roots.
- `dependencies = "available_only"` and `network = "deny"`. Dependencies are present in the
  admitted image, source or approved immutable Cache Snapshots; a missing required object fails
  the offline command. Nothing implicitly downloads
  an image, resolves credentials or opens the network. Acquisition requires a separate trusted
  operator action outside this Attempt.
- Mandatory wall, per-stream output, CPU millicores, memory bytes, aggregate `disk_bytes`
  and process limits, within the Attempt reservation and machine ceilings. Disk limits are
  exact 4096-byte multiples from 4096 through 8 GiB. Missing limits and older development
  contracts fail closed; a machine binding must explicitly authorize `storage = "tmpfs_volume"`.

The complete TOML examples and setup procedure are in
[`fixtures/worker-development/`](../../fixtures/worker-development/README.md). The JSON views are
[`worker-development-v3.json`](../../schemas/worker-development-v3.json),
[`worker-development-binding-v3.json`](../../schemas/worker-development-binding-v3.json) and
[`worker-command-evidence-v1.json`](../../schemas/worker-command-evidence-v1.json).

### Trusted authority and admission

The operator sets `AF_WORKER_DEVELOPMENT_BINDING_FILE` to an absolute, regular machine file,
schema `af.worker-development-binding/3`. Descriptor-relative no-follow reads reject symlink
leaves and ancestors and cap it at 64 KiB. It is not a package path or a project policy file.
The binding contains a local container-runtime executable, digest-pinned credential-free tool
image, trusted checksum program, POSIX image-probe shell, symbolic tool mappings and machine
ceilings and `storage = "tmpfs_volume"`. That explicit backend authorization permits private
local-driver tmpfs volume operations and fixed image-only seed/export helpers. When af runs as
root, only the seed helper receives `CHOWN`, solely to transfer its private volume's files to
UID 65534; its host inputs are read-only. Workers always run without capabilities as a non-root
UID (the operator UID, or 65534 for a root operator). It contains no credential grant or
host-cache mapping. Optional cache ceilings and approved
dependency Manifest IDs grant only the immutable reuse described in step 2.

The companion's `binding_identity` must equal the canonical content identity of that complete
binding. Planning checks the exact binding, and the host checks it again before preparation.
A replacement machine file cannot change a resumed Task's environment. Worker requests can
select only mapped tools with exact identities and ceilings no greater than machine policy.
Changing the companion requires a new package digest and the normal plan approval.

Supported Workers use `runner.kind = "command"`, `runner.command.program = "@tool/HARNESS"`,
`read-source` and `execute-checks`; they either write source or have the `review` role. A
symbolic tool without a companion is refused. Native Claude, Codex and Pi model adapters are
explicitly unsupported for this companion: none exposes a common enforceable command/evidence
protocol here. Their declarations fail package admission instead of being ignored. Existing
undeclared model Workers retain their current behavior. A source environment that cannot
materialize under its own isolation policy still refuses; the companion never downgrades it.
Other installed environment preparation that adds host variables cannot be combined with this
backend.

### Execution and isolation

After reservation and context binding, the installed local runtime is probed under the
remaining absolute Attempt deadline. Admission requires the Engine
[capability flags](https://github.com/moby/moby/blob/master/api/types/system/info.go)
`MemoryLimit`, `SwapLimit`, `CpuCfsQuota` and `PidsLimit` to be true; an exit-zero client alone
is not proof that the runtime enforces the requested quotas. Missing or unsupported metadata
is refused. Bounded local image metadata admission refuses declared
volumes, which could otherwise shadow source subtrees or the private roots with unrelated bytes.
A subsequent image-only invocation of the machine-bound `probe_shell` checks fixed mount
components for symlinks and non-directories before any source is mounted, including auxiliary
and helper mount points. GNU `readlink` resolves helper and declared tool programs in the image;
programs resolving into dynamic mounts are refused before input copies or harness dispatch.
It executes only a kernel-owned test script against the immutable image; an unavailable shell or unsafe layout
is refused. The existing container provider and `review-process` supervisor
run the harness; there is no new scheduler, retry loop or process executor.

Each invocation uses a new private source clone and the existing `CheckRuntime` lifecycle for
its source staging, state, scratch, cache, build, knowledge, package and client directories.
Before harness dispatch a fixed helper copies these exact initial bytes into one new private
local-driver tmpfs volume. The helper verifies filesystem type, 4096-byte block size and exact
capacity; a missing driver, unsupported volume subpath mount, unsafe image layout or failed
copy refuses dispatch. The filesystem has a fixed 16,384-inode ceiling. Initial source, Base
and cache bytes also consume its capacity: an overfull initial environment is refused.
Tmpfs data also consumes memory; the seed and workload containers retain their admitted memory
ceilings. Operators must leave room for initial files and compiler processes, and memory pressure
may stop allocation before the disk ceiling is reached.

Only subdirectories of that same filesystem are mounted at `/work`, `/af/state`, `/af/scratch`,
`/af/build`, `/af/cache`, `/af/knowledge`, `/tmp`, `/run` and `/dev/shm`. Undeclared roots and
Base are mounted read-only, without a second writable alias. The package is a read-only host
bind; no Worker receives a writable host bind or a mount of the volume's parent. `/dev` is
root-owned and unwritable by the non-root Worker; `/dev/mqueue` is covered by an empty read-only
subdirectory and IPC sharing is disabled. The image root and Engine-generated configuration
files are read-only. Daemon logging is disabled, so stdout cannot grow a log outside the bound.
Regular device streams such as `/dev/null` do not allocate filesystem storage. Filling any
writable root exhausts the same filesystem, immediately returning `ENOSPC` on further
allocations; wall deadlines and later capture scans are not the storage enforcement.

The container has
no network, no capabilities, no privilege gain, a private PID namespace, an init, a process
ceiling, CPU and memory ceilings, a read-only root filesystem and a read-only package mount.
It receives only the explicit environment plus the pinned image's own environment. Its
entrypoint is replaced with the exact selected program, and image health checks are disabled.
Image pulls are disabled. The runtime forwards the supervised stdin pipe with `--interactive`,
without a TTY, so the harness receives its exact declared context and stdout/stderr stay separate.
The runtime client gets a cleared environment and private configuration, not the operator's credentials or
remote context. Only a default local daemon endpoint is supported.

Before the harness runs, a bounded command in the same pinned image hashes each requested
installed tool. This probe mounts empty read-only source/package roots and no runtime or Base roots, so
image symlinks and interpreter startup settings cannot execute project bytes. Its receipt
identifies that empty input Manifest; the source Snapshot is the Attempt association, not a
claim that the probe read source. It runs the machine-authorized checksum utility, never
project code or a project build. A missing image, unavailable tool, changed digest, failed runtime or failed
probe refuses dispatch. That probe's actual output is retained with the command receipt.

After the workload and all its container descendants stop, a fixed helper mounts the volume
read-only and exports only declared writable roots. The shared process supervisor streams the
archive under the same Attempt deadline and cancellation signal. The host accepts only regular
files and directories in a private staging directory, with aggregate logical file bytes at most
`disk_bytes`, at most 16,384 entries, depth 64 and bounded canonical paths. GNU tar framing supports bounded long-name records through 4096 bytes and preserves the
filesystem bytes that the Manifest scanner spells with `review_core::encode_path`. Links, devices, malformed/duplicate paths and oversized sparse
files fail closed before publication. No host archive extractor follows Worker-controlled paths.
All roots validate before any source/cache root is replaced, and a failed replacement cannot
admit the Attempt. The final Manifest and cache captures describe the exported bytes. Export
failure records a failed command and unavailable final tree; confirmed daemon cleanup remains
separate from unavailable filesystem evidence. The exact stdout/stderr returned by the harness
remain the command receipt's bytes, independently of export success.

A durable `af.worker-storage/1` intent in CheckRuntime is synced before any daemon resource is
created. A bounded keeper holds the tmpfs mounted across seed, workload and export, and expires
at the Attempt deadline. Ordinary cleanup removes exactly that intent's workload/helper/keeper
names and volume. The existing dead-owner sandbox sweep reads the versioned intent, confirms
cleanup in the same private local daemon context, then removes the runtime. Live owners and
explicitly preserved roots remain untouched; unknown or unconfirmed recovery keeps the intent.
CheckRuntime drop also refuses to remove a runtime whose resource cleanup is unconfirmed.
No Worker can alter this marker, daemon configuration or cleanup authority. Retained exports
and immutable cache objects still use existing Task CAS leases, reachability and Storage Budget
accounting. The tmpfs is Attempt-private execution storage, never a persisted cache index.

A command harness may run several compile/test commands; they share only that Attempt's
private roots. The Rust example points Cargo's target, cache and offline configuration at
these declared roots; the Python example compiles bytecode into the declared build root.
The runtime contains no language-specific environment variables or path rules.

An implementer writes its private source clone, which still goes through the existing candidate
seal and acceptance path. A reviewer receives a second experiment clone of the exact source,
including declared Review input files, and the attached Base sibling is mounted read-only at
`/base`. Experiments may edit source and create reproducers. Only the untouched original clone
reaches the source environment's finish operation, so an experiment cannot become a candidate
or Proposal. It is discarded after its evidence is recorded.

A failed or cancelled runtime command uses the existing bounded daemon cleanup. If cleanup
cannot be confirmed, the source and runtime roots are marked preserved before any fallible
receipt write. They are neither sealed nor removed while daemon cleanup is unconfirmed;
the recovery intent retains the private volume's exact identity. The existing crash sweep leaves preserved roots for the operator.

### What command evidence means

The tool identity probe and each dispatched harness contribute an
`af.worker-command-evidence/1` raw record to the
Attempt's existing retained-evidence list, with purpose `tool_probe` or `harness`. Failed probes
retain their bounded output even when no harness is dispatched. Each record contains:

- Attempt and context identity, the admitted source Snapshot, actual initial tree digest and
  Manifest reference, whether this was an experiment, and the final tree digest and Manifest
  reference when they can be read after confirmed cleanup. Comparing the two Manifests gives
  the experiment's actual delta, using the existing canonical Manifest path alphabet. A final
  experiment Manifest records paths and content digests; it is not a reusable source Snapshot,
  and its edited file bytes are not retained. Implementer candidates use the ordinary complete
  source capture separately.
- Actual program/arguments, actual container cwd `/work`, exact allowed-environment identity, exact
  binding identity and the tool identity probe's output reference. Referenced binding,
  environment, Manifests and output objects are retained beside the receipt.
- Start time, elapsed command-and-cleanup time, exit/failure and the supervisor's available resource
  counters. These counters describe the runtime client, explicitly marked
  `resources_are_runtime_client_only`; they are not invented container CPU or memory samples.
- Bounded stdout/stderr references, original stream totals and the applied cap. A truncated
  stream refuses the result. Credential-shaped output is withheld before CAS storage and is
  marked `redacted`. Redaction does not rewrite original byte counts into measurements.
- `returned_output_id`: the exact bounded stdout delivered by the command transport to its
  reply parser, absent for a failed, truncated or redacted invocation. Invalid business JSON
  remains inspectable and cannot become an admitted output.

The record is for the outer harness command, not every child it launched. The omissions
`internal_commands_unobserved` and `internal_model_output_unobserved` are explicit. A command
harness may itself use a model, but this host cannot claim its internal prompts, tool calls or
output delivered to that model. There is no invented transcript or reconstructed excerpt.
A missing final Manifest is marked `final_tree_unavailable`. Final evidence is omitted when
its pre-walk exceeds 16,384 entries, 256 MiB, depth 64, path length 4,096 bytes or the remaining
Attempt deadline, or when the shared scanner cannot read it; it never becomes a partial
Manifest. Worker-built outputs and caches remain development evidence, never a Gate receipt or independent acceptance.

### Checks and cost

`review-core`'s `worker_development` schema-parity tests check versions, strict fields,
unsupported quotas, paths and authority widening. `review-config` tests no-follow machine
reads, optional companions and real package admission, including model-adapter refusal.
`review-pipeline`'s `task::development::tests` exercises the actual invocation, supervision,
state, experiment isolation and receipts with a recording runtime. That runtime is a transport
fixture, not proof of container isolation.

`task_runtime::development::admitted_implementer_and_reviewer_tasks_compile_test_rust_and_python_with_private_state`
captures and admits configured command packages, compiles their plans and runs four real Tasks:
Rust and Python, each as implementer and reviewer. The shared recording runtime translates the
private mount paths and executes the installed, hash-pinned tools; compiler/test results come
from real offline Cargo tests and Python bytecode compilation/unittest, not fabricated output.
The harnesses edit source before testing, retain private state across child commands, and check
actual build products. The test checks admitted outputs against exact host command receipts,
Attempt/context/source identities and initial/final Manifests, and reopens the Store for replay.
Implementer edits reach only its captured candidate; reviewer edits remain in its experiment.
The fixture's Rust build roots live under the checkout's disposable `target/` (or the gate-owned
test target), with fresh HOME/cache directories and no dependency acquisition. These tests prove
Task/toolchain integration; the recording runtime supplies no OS containment guarantee.

`scripts/test-worker-development.py` runs the actual companion renderer, the Python example
harness and the Rust example's Cargo harness twice, offline and without external dependencies.
The existing live `a_host_marker_is_out_of_reach` probe additionally runs concurrent Worker slots
against the real daemon. Live probes require their existing explicit container-probe invocation;
a recording runtime does not discharge them.

Undeclared environments pay bounded package-file lookups and no extra processes or source scan.
Declared development runs add runtime and bounded local image metadata probes, an image-only
mount-layout command, an image-only tool hash command, per-Attempt runtime setup, initial/final
tree scans with a bounded final pre-walk and, for reviewers, a private experiment clone. Keep
build products outside source to keep those scans small. No paid dogfood comparison is claimed.

## Step 2: immutable caches and explicit continuation

The binding decision is
[ADR-2026-11-04-carry-declared-worker-caches-in-exact-warm-sets](../adr/2026-11-04-carry-declared-worker-caches-in-exact-warm-sets.md).
The version-2 companion adds a `caches` table, at most one policy per private build, cache or
state root. A policy declares:

- `role`: `dependency`, `build` or `continuation`; `destination` and canonical relative `path`.
  A build uses the build root, an approved dependency uses the cache root, and continuation
  uses an explicit state subdirectory outside HOME and configuration. No host path is accepted.
- `selection`: `compatible` or `exact_source`. Compatibility includes the pinned request,
  machine binding/image/tool identities, host OS/architecture, declared `keys` for flags,
  profiles/features and up to 64 `fingerprints` naming source files in canonical Manifest
  spelling. Missing files differ from empty files. Capture uses final producer fingerprints.
- `lifecycle = "task"`: immutable layers follow existing Task retention, leases, Storage Budget
  and collection; every writable clone remains Attempt-private. Machine-wide or immortal
  lifecycle requests are refused.
- `limits`: bytes, traversed entries, depth and path length, within the existing Build Cache
  hard ceilings and the machine's explicit `cache_limits`. No grant means no cache capability.
- An optional typed `input` and `persistence = "discard"` or `"admitted_output"`. Persistence
  requires an optional typed `output`. These ports are `review.kernel/BuildCache@2`, cardinality
  one, unbound; the output is filled only by the host, never the Worker reply. Pipelines wire
  an admitted predecessor's output explicitly to a later Worker on the same effective slot.
  Cross-slot edges are refused even when their types agree.
- A dependency instead names `dependency_source`, a key in the pinned machine binding's
  `dependency_sources` map of CAS Manifest IDs. It has no Worker input or output. Acquisition
  stays `available_only`: a trusted operator imports credential-free bytes separately. Missing
  dependencies do not authorize a fetch, and Worker writes cannot populate that source.

A cache-enabled Worker declares an optional unbound `development_warm` input of
`review.kernel/WarmSet@3`. The kernel selects it before recording the invocation, before any
Attempt reservation. For each policy the selection names exact artifact, Manifest and
compatibility identities or `no_input`, `incompatible`, `wrong_lineage`, `missing`, `corrupt`
or `over_bound`. The Warm Set and each selected filesystem layer appear in the context
Manifest; filesystem clones have zero rendered prompt bytes. No cache lookup runs during
retry or replay. A recorded selected layer that becomes unavailable refuses restoration with
its explicit reason instead of silently selecting another input.

Every invocation still receives a fresh writable copy or copy-on-write clone. Files remain
warm between commands inside that Attempt. A failed sibling's source, private cache and state
are discarded. A later node may use only an admitted same-Task, same-slot predecessor output.
Continuation additionally checks the exact source Snapshot and actual final tree digest, so
checkpoint diagnostics cannot resume source edits. HOME, configuration and conversations stay
fresh. There is no ambient cross-Task cache index or session restoration.

`BuildCache@2` records producer Attempt, Task/slot, initial Snapshot, actual initial/final tree
digests and compatibility. Its historical `gate_node`/`gate_attempt_id` fields identify the
producer on the Worker branch. It remains candidate-built data. Gate-to-reviewer captures
retain Gate-only events, trusted-local admission and same-Round producer checks. Worker
captures never enter those events or the Gate's private warm-check directories. Reusing bytes
never supplies test success or independent acceptance.

Capture uses the existing bounded no-follow Build Cache traversal and immutable CAS publication.
It admits regular files only, strips metadata and enforces all limits. Deadline/cancellation
checks run during traversal and streaming. A complete capture gets an
[`af.worker-cache-capture/1`](../../schemas/worker-cache-capture-v1.json) receipt and a host-built
cache artifact; refusal, failed command, cancellation, exhausted deadline and unconfirmed
cleanup have explicit omission receipts. Optional final source evidence must be available to
publish producer provenance. A malformed reply or later source/output refusal cannot admit
its capture. Unconfirmed container cleanup preserves the runtime and every exposed source root
before any fallible receipt write.

All retained bytes are in the existing Task CAS. Source/cache Manifests and Warm Set references
participate in ordinary reachability and Storage Budget accounting. Live Task writer leases
prevent collection, including capture objects not yet attached to a settled Attempt. CAS
publication barriers, fenced settlement and tombstone-then-sweep recovery remain the owners;
there is no new shared directory, lease or GC loop. Partial clone destinations are removed and
never reused; crashed CheckRuntime directories use the existing process-aware crash sweep.
The companion and machine binding are version 3 with mandatory aggregate disk authority;
Build Cache remains version 2 and Warm Set/selection version 3. Older versions are refused; no migration is provided.

Tests in `task::development::tests` exercise compatible reuse without a build fallback,
parallel writable clones, tool/config/dependency/profile invalidation, same-slot provenance,
exact checkpoint continuation, failed sibling isolation, approved dependency clones, missing
and corrupt selections, cancellation and incomplete capture. Schema parity covers positive
and negative forms. The real Task publication/recovery test proves selection precedes
reservation and is not repeated after a lost acknowledgement. Shared capture/clone and Task
collection tests cover interrupted work, active leases, byte accounting and resumed GC.
`build_cache::tests::crash::mid_capture_and_clone_crashes_recover_private_roots_without_admitting_partial_caches`
uses child processes and test-build-only barriers in the real capture and clone paths. Capture
stops after publishing an object but before publishing its complete Manifest; clone stops after
copying its files but before required private metadata normalization. The parent waits for the
barrier, kills exactly that child, reopens the Task Store and runs the production sandbox sweep.
It checks that no interrupted capture Manifest or admitted output appears, an unfinished clone
is refused as an existing destination, live owners and writer leases protect their state, and
reachable cache bytes survive collection. Dead private roots are swept, explicitly preserved
roots remain for the operator, unreferenced capture bytes become collectible, and a fresh clone
of the recorded complete layer succeeds. The existing collection tombstone test separately
covers restart during GC. No product crash switch, polling sleep or timing-based success is used.
The fixtures are transport/protocol tests, not live container containment or paid model
measurements; no new model calls or dogfood savings are claimed.

## Step 3: bounded knowledge retrieval and private Notes

The decision is
[ADR-2026-11-04-select-bounded-worker-knowledge-through-typed-ports](../adr/2026-11-04-select-bounded-worker-knowledge-through-typed-ports.md).
A separate pinned `knowledge.toml` companion (`af.worker-knowledge/1`) works on both command
and model Workers, including command Workers using the step-1 environment. It does not grant
native model adapters a development environment. Complete language-independent examples and
an operator procedure are in [`fixtures/worker-knowledge/`](../../fixtures/worker-knowledge/README.md).

### Declaration and selection

Declare up to 64 named source ranges using `${source}/canonical-path`, their byte offsets and
lengths, and additional path dependencies. Names and paths are explicit; there are no globs,
repository-wide queries or archive ingestion. The source is the invocation's typed `source`
Snapshot, never a host directory. `read-source` and the typed source input are required.
Both source content and dependency identities come from that Snapshot's Manifest. File kind,
absence and bytes matter; unchanged filenames do not establish freshness.

Declare `manifest_bytes`, `context_bytes`, `retrieval_bytes`, `retrieval_items`, `source_bytes`
and `notes_bytes`. Hard ceilings are 256 KiB per manifest, context or source file, 64 KiB per
retrieval or private Notes payload and 64 retrieved items. `retention = "task"` uses existing
Task retention and collection. Private writable `${knowledge}` roots from step 1 are still
Attempt-private scratch; no filesystem directory becomes admitted memory.

The compiler reserves the optional unbound `knowledge` input of `af/KnowledgeContext@1` for
the kernel. Before reservation the host builds a bounded `af/KnowledgeIndex@1` from explicit
source records. The initial context contains metadata and omissions, with no source excerpts
or private prose. Entries identify kind, visibility, freshness, source artifact and Snapshot,
path/range, dependencies, exact excerpt blob and supporting evidence. `fact` means a captured
source excerpt, never a conclusion a Worker asserted. All indexed text is labeled data, not
instructions or acceptance evidence.

A discovery Worker emits an optional `knowledge_query` output of `af/KnowledgeQuery@1`, naming
that index and selected entry IDs. Bind it to the next node's optional `knowledge_query` input.
The next invocation receives only bounded selected excerpts in its `knowledge` context. This
uses existing typed pipeline nodes: there is no live retrieval callback, automatic extra model
call or new scheduler. Every node uses its ordinary Attempt and token reservation. Worker
query outputs are checked against the manifest actually delivered and retain its index.

### Private Notes, freshness and independence

With `private_notes = "same_slot"`, existing optional `af/WorkerNotes@1` ports and same-slot
compiler wiring supply an admitted predecessor's inspection map. The kernel checks its actual
Task/Attempt producer and effective slot. Failed or quarantined raw evidence is never searched.
The kernel resolves referenced paths against the producing Snapshot; a Worker cannot assert
freshness by writing an inspected digest. The selected **Warm Set** refers to Notes and the
kernel's existing **Head Delta**. A versioned `af.private-knowledge/1` wrapper contains
Notes and that delta. Retrieved Notes include the delta, remain `hypothesis` and
`same_slot`, and never become a reviewer verdict or source fact.

A private Notes entry's query identity binds the admitted Notes artifact, Task, effective slot
and producing Snapshot's path dependencies. It does not bind the rendered Head Delta: an
unrelated source change preserves the query identity while retrieval recomputes the delta to the
current Snapshot. The excerpt's separate `content_id` and byte count identify the newly delivered
Notes/delta bytes. Same-slot authority, freshness and all bounds are checked again before delivery;
a query never authorizes another producer's Notes or reuse of an old delta. Credential screening
exempts the already validated, producer-bound Attempt identity from prose heuristics; every
Worker-authored Notes field is still screened, and the delivered identity bytes remain exact.

`freshness = "mark"` labels changed Notes `revalidate`; `omit_stale` omits them. Pathless Notes
are current only on their exact source Snapshot. Source facts are revalidated from exact
content/dependency identities; unrelated source changes keep them eligible. A query for an
invalidated fact records `stale`. Notes and raw queries stay in invocation provenance, while
context rendering suppresses their raw input payloads. Without an explicit query no private
prose is delivered. Independent slots and other Tasks never receive private Notes.

`cross_task = "disabled"` is the normal declaration. Explicit `source_facts` permits a wired
imported query from another Task or slot only for public source facts in a kernel-produced
index, revalidated against the receiving Worker's current declared source. It authorizes no
ambient search or raw session/private Notes access. For cross-Task use, bind a recorded admitted
Task output through the existing typed Task input mechanism; a query alone grants no trust.

### Evidence, bounds and recovery

The immutable index and exact excerpt blobs live in the existing Task CAS. The context input
retains its index, source artifacts, excerpt blobs, query and optional Warm Set. Context manifest
entries identify each delivered excerpt blob and its exact UTF-8 byte count; the rendered
request records JSON escaping and wrappers in the total context size. A step-1 command receipt
already identifies that exact Task context. No mutable shared index or separate GC owner exists.

Index overflow omits entries in reverse name order. Retrieval considers sorted entry IDs, applies
item and byte ceilings, then omits excerpts from the end if the total knowledge context exceeds
its separate ceiling. Each omission is recorded. Missing source paths, oversized files, symlinks,
non-text or credential-shaped content, wrong lineage, stale entries and evicted/corrupt artifacts
have explicit reasons. If metadata and omission records alone cannot fit a requested bound,
preparation refuses. Complete manifests are never silently replaced with partial evidence.

Indexes are rebuildable projections of captured source records and declared Notes, with stable
content identities. The common Task runtime pins selection before reservation; retry and restart
reuse the recorded invocation. A selected context or exact excerpt unavailable during composition
refuses composition; it never changes its query. Existing writer leases, CAS barriers, Storage
Budget and Task collection govern retention. No failed sibling's private filesystem state is
reused. Sessions remain unsupported under ADR-0145.

The host observes the exact outer command/model request. A command harness may run its own
model, but its internal prompts, calls and file reads remain unavailable; neither an excerpt blob
nor an outer receipt proves that an internal model consumed it. Worker commands, caches, Notes
and knowledge remain development evidence, never local Gate or independent acceptance receipts.

### Validation and cost

Schema-parity tests cover all five new versioned contracts and refuse unsupported shapes.
`task::knowledge::tests` covers source changes, path dependencies, exact excerpts, restart,
bounds, eviction, private Notes, Head Delta and both command and native Codex adapters with a
fake CLI. `task_runtime::knowledge` runs captured command/model packages through real Task
selection, retry, durable admission and replay; its rejected producer contributes no private
memory. Its same-slot Notes regression runs three captured Workers: one produces Notes, one
queries the delivered private manifest, and one receives the admitted query against a Snapshot
with only unrelated bytes changed. It checks query provenance, unchanged Notes dependencies,
the current Head Delta and exact delivered context evidence. Existing Notes compiler tests
preserve independent slots. These fixtures use no paid model and claim no measured savings
or live containment.

Workers without the companion pay no knowledge selection, source reads or additional processes.
Selection uses the existing Snapshot reader's integrity validation, which hashes captured
objects as normal source preparation does. It indexes at most 64 explicitly declared bounded
files, and optionally one Notes artifact and its existing head-to-head diff. That diff admits at most 16,384 combined input entries
and 256 MiB across its two Snapshots; an over-bound input or output explicitly omits private
knowledge. Stored immutable artifacts follow the Task's machine budget.
