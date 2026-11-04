# Worker development examples

The Rust implementer and Python reviewer use the same language-neutral
[`af.worker-development/3` contract](../../docs/design/worker-local-development.md). The examples
are captured Task command packages, not model-provider packages or Gate definitions. Their source
fixtures have no external dependencies. Their harnesses keep state between commands in one Attempt.

## Prepare trusted tools

Use a locally installed Docker-compatible runtime and an already available, digest-pinned,
credential-free tool image without declared volumes (which could shadow the source). There is
no implicit pull, build, package installation or network access in a Worker. Build/acquire that image separately under operator authority. The Rust image
needs `sh`, `cargo`, `rustc`, `cc` (the platform linker driver), a complete installed Rust toolchain under
`/af/dependencies/rustup`, and `/usr/bin/sha256sum`. The Python image needs `sh`, `python3` and
the same checksum utility. Runtime image libraries and dependencies are pinned by the image
digest; installed executable bytes are checked separately before each harness.

Create an operator-owned file outside the repository, using the image digest and actual
`sha256sum` results for its tools. This illustrative shape is not a usable binding until its
placeholders are replaced with measured identities:

```toml
schema = "af.worker-development-binding/3"
storage = "tmpfs_volume"
runtime = "/absolute/path/to/docker"
image = "registry.example/toolchain@sha256:IMAGE_DIGEST"
checksum_program = "/usr/bin/sha256sum"
probe_shell = "/bin/sh"

[tools.sh]
program = "/bin/sh"
identity = "sha256:EXECUTABLE_DIGEST"
# Rust: also map cargo, rustc and cc to their actual image programs and measured identities.
# Python: also map python3 to its actual image program and measured identity.

# Explicit grant for the Rust example's bounded, immutable build-cache carry.
[cache_limits]
max_bytes = 268435456
max_entries = 16384
max_depth = 32
max_path_bytes = 4096

[limits]
wall_ms = 120000
output_bytes = 65536
cpu_millicores = 2000
memory_bytes = 1073741824
processes = 128
disk_bytes = 268435456
```

Only a default local daemon endpoint is supported. The runtime client has private configuration;
it cannot borrow a login, credential helper or remote context from the operator's home.
An unsupported runtime or quota refuses dispatch; there is no host-shell fallback.
The runtime must support local-driver tmpfs volumes and volume subpath mounts. The pinned image
also needs GNU `/usr/bin/readlink`, `/usr/bin/stat`, `/bin/tar`, `/bin/cp`, `/bin/mkdir`, `/bin/chmod`, `/bin/chown`
and `/bin/sleep` for fixed host-owned storage helpers. These are backend infrastructure, not
language-specific harness hooks; helpers never run a project build. Explicit `tmpfs_volume`
authority includes the seed helper's private-file ownership transfer with `CHOWN` when the
operator is root. Workers receive no capability and always use a non-root UID.

`disk_bytes` is mandatory for both the machine and Worker. It bounds the aggregate writable
filesystem, including all six roots, HOME/configuration, temporary files, `/run` and `/dev/shm`.
The examples request 256 MiB; initial source/Base/cache copies also consume this capacity.
Tmpfs consumes memory too: allow enough admitted memory for seeded files and compiler processes.
Use a 4096-byte multiple through 8 GiB within machine policy. Every Worker gets its own volume;
no writable host directory is exposed. A full volume stops writes immediately. Export checks
logical bytes too, refusing sparse expansion, links and unsafe archive paths before replacing
source/cache roots. Unavailable export records failed development evidence, never a passing
command or an authoritative cache. Older `/1` and `/2` declarations require a newly reviewed
version 3 binding, companion, package pin and plan.

## Render and pin the package

Copy the selected example package to the consuming repository. Run the renderer against the
reviewed machine binding and that copy's template, with canonical paths (no symlink ancestors):

```sh
python3 -I scripts/worker-development.py \
  --binding /absolute/operator/worker-development.toml \
  --template /absolute/copied/package/development.template.toml \
  --output /absolute/copied/package/development.toml
```

It prints the exact machine binding identity and fills the tool identities. It never executes a
project, container, package manager or model; existing output is refused. Remove the unused
template from the copied package, then use `af catalog digest /absolute/copied/package` and
review/commit its catalog pin through the consuming repository's normal authority process.
Set `AF_WORKER_DEVELOPMENT_BINDING_FILE` to the same machine file for planning and execution.
A later machine-binding change requires a newly rendered companion, package pin and plan.

Use the language fixture as the Task source. The Rust implementer exposes the kernel-owned
candidate port and reports its local tests. The Python reviewer edits only its private experiment
and exposes no candidate. Both are deterministic, zero-token command Workers; a pipeline still
needs its own Gate, independent reviewers and evaluator to establish acceptance. Local development
output never satisfies those obligations by itself.

## Reuse and continuation

The Rust companion declares `caches.objects`: the private build root, immutable admitted output
`cache_out`, optional predecessor `cache_in`, compatibility fingerprints `Cargo.toml`/`Cargo.lock`
and explicit profile/flags/features keys. It requires the machine's cache grant above. A later
Worker on the same slot can bind `cache_in` to the earlier node's `cache_out`. Leave the first
node's optional input unbound. Independent reviewer slots cannot bind that cache. A missing or
incompatible layer runs cold with a recorded reason, and the local Gate remains independent.

The optional `development_warm` input belongs to the kernel: do not wire it in a Pipeline or
include either it or `cache_out` in a harness reply. Selection is recorded before reservation;
a retry receives that same selection. Build Cache outputs are host captures admitted only
with the successful output. They never assert that compilation or tests succeeded.

For explicit checkpoint continuation, use a separate policy with `role = "continuation"`,
`destination = "state"`, `path = "checkpoint"`, `selection = "exact_source"` and distinct typed
input/output ports. HOME and configuration are excluded. Source edits and conversations never
resume from a checkpoint. Bounded knowledge and private Notes use the separate
[`knowledge.toml` companion](../worker-knowledge/README.md); sessions remain unsupported.

An optional dependency policy uses `role = "dependency"`, `destination = "cache"`,
`persistence = "discard"` and `dependency_source = "packages"`, without Worker input/output
ports. The machine binding must name that source under `[dependency_sources]` as an immutable
credential-free Manifest ID in the Task CAS. This is an available-only acquisition policy:
trusted acquisition/import happens separately, and the Worker cannot update the source.
Every declared policy supplies byte/entry/depth/path limits within the machine grant and
`lifecycle = "task"`; immutable bytes follow Task retention and collection, and writable clones
remain Attempt-private.

The Rust template pins `cc` as well as Cargo and Rustc: the declared tools construct the harness
PATH, so an undeclared linker driver is unavailable even if it exists elsewhere in the image.
Its support programs and libraries must already be present in the pinned image. No acquisition
or inherited operator PATH supplies a missing linker.

## Local fixture checks

`python3 -I scripts/test-worker-development.py` exercises the renderer, compiles and tests the
Python fixture, and runs the Rust fixture's Cargo harness twice with the repository's pinned
installed toolchain. It resolves real tools from the active compiler's sysroot and checks the
compiler version against `rust-toolchain.toml`, so the gate's admitted native toolchain needs no
Rustup installation registry. Cargo and Rustup homes remain private, with acquisition disabled.
Fixture build directories are removed after the check, under the checkout's `target/` locally
or the gate target supplied by `scripts/verify.sh` for a read-only source Snapshot.
It requires no Docker daemon, downloaded crates or Provider. The Task invocation tests use a recording
runtime for transport and receipts; real filesystem/network containment is checked by the existing
explicit live container probes. Their aggregate-bound case allocates across all six roots,
`/tmp`, `/run` and `/dev/shm`, requires `ENOSPC`, and checks a concurrent sibling and host marker.
Other probes reject a sparse one-GiB export under a one-MiB bound and kill a storage owner while
its real volume is mounted, then run the production dead-owner sweep. A fixture runtime is not
a containment receipt.

The `review-pipeline::it` test module `task_runtime::development` additionally admits configured
packages and runs real Task plans for both languages under both roles. Its recording runtime
executes the actual pinned local tools through the development mounts and supervisor. It checks
compile/test output, build products, private state retained across child commands, source edits
in reviewer experiments, exact host command receipts and replay after reopening the Task Store.
Its Cargo builds stay in disposable private directories under the checkout's `target/` or the
gate-owned test target. `recording-runtime.py` is shared test infrastructure, never an operator
binding or a containment backend; no Worker may select it as trusted machine authority.

When a continuation producer edits source, wire its candidate through the typed `seal` operation
and pass both the sealed `snapshot` and the admitted cache output to the next Worker on the same
slot. The cache envelope's `subject_snapshot_id` names that output Snapshot; its payload's
`head_snapshot_id` preserves the producer input. Exact-source selection refuses the old source,
raw unadmitted captures and experiments that differ from the sealed candidate.
