# Warm checks

A Task check is a command from the project's committed code policy that af runs against a
read-only Snapshot (the frozen source tree a Task works on); its results form the gate a Task
must pass. A code policy's `[warm]` table lets those checks reuse build directories that live on
this machine between runs, so a check does not compile the whole workspace every time. This page
is the operator's view of that warm layer; the decisions are
[ADR-0131](../adr/0131-warm-task-checks-through-a-toolchain-keyed-bounded-cache.md) and
[ADR-0207](../adr/0207-key-warm-check-layers-by-content-lock-them-split-the-build-deadline-and-seal-without-generated-files.md).

```toml
[warm]
build_cache = ["cargo_target", "cargo_home", "npm_cache"]
max_bytes = 17179869184      # eviction bound of one key
hard_max_bytes = 34359738368 # the only bound that ends a running check
build_wall_ms = 2400000      # the build phase's own deadline (optional)
clone_when_busy = false      # true: build in a private copy instead of waiting
```

## Where a layer lives

A layer is one directory per project and toolchain:
`$XDG_CACHE_HOME/af/task-build-cache/<project>/<toolchain>/`, with one subdirectory per kind —
`cargo_target` (bound as `CARGO_TARGET_DIR`), `cargo_home` (`CARGO_HOME`) and `npm_cache`
(`npm_config_cache`). `<project>` is a digest of the repository's identity and `<toolchain>` a
digest of the Snapshot's `rust-toolchain.toml`, `rustc -vV`, `cargo -vV` and the check's fixed
environment, so a changed compiler is a new layer. Beside each layer af keeps three small records:
`<toolchain>.sources` (what the layer last built), `<toolchain>.holder` (who holds it now) and
`<toolchain>.evicted` (when the Storage Budget removed it).

## The key rule: content, not modification times

Cargo decides whether a crate is fresh by comparing modification times, and a materialized
Snapshot carries old, fixed ones. So af decides by content. The Snapshot's files fall into source
groups — each Cargo package directory, and the root for files no package holds — and each group
has a digest of its paths and bytes. Before a check, while it holds the layer:

- a group whose digest the layer's sources record holds gets back the time af gave its files when
  the layer built that content, so Cargo keeps that build;
- every other group — changed, new, or every group of a layer with no record — is stamped now,
  after everything in the target, so Cargo rebuilds exactly those crates and what depends on them.

The record is removed before any file is stamped and written again before the check starts, so a
check that fails or is killed never leaves a record describing a build it did not finish. A layer
built from another Snapshot therefore can never be linked into this one (#266). `af task show ID
--checks` prints how many groups were stale and fresh.

## One check at a time per layer

A check holds the layer's lock (`warm.lock`) from preparation until its results are accepted. A
second check that wants the same layer waits until its own deadline, printing every minute on
stderr:

```text
waiting for warm layer <project>/<toolchain> held by af task run 4242 since 2026-10-23 12:00:00 UTC on claw-3; waited 60 s, waiting up to 2340 s more (the check deadline)
```

With `clone_when_busy = true` it does not wait: it copies the layer into its own runtime directory
(`clonefile` on APFS, a reflink on btrfs and XFS, a plain copy elsewhere) and builds there, with
every source group stamped now, leaving the layer untouched. A layer whose deadline ran out before
it was free builds cold and says `busy`.

## Two deadlines: the build, then the tests

With `build_wall_ms` declared, a check has two phases. The build phase runs from its start until
it reports a `build` step through `$AF_CHECK_REPORT` (`scripts/ci-step.py --stage build` does; in
this repository that is `make check`'s `test-build`), and `build_wall_ms` bounds it. The tests
run from there, and `check_wall_ms` bounds them, so a cold build never eats the tests' time. A
check that has reported steps but no build step when its build deadline passes is ended there; one
that reports no step at all gets both deadlines as one. The check Attempt's wall is
`check_wall_ms` plus `build_wall_ms`; a Task file whose verification reserve is smaller is refused
at planning with the limits that fit (`af task plan --fit`). Without `build_wall_ms`, the whole
check has `check_wall_ms`, as before.

## Eviction

A layer is removed, never repaired. The kernel removes one that is over `max_bytes` before or
after a check, and one a check left suspect (a link, a special file, a credential, a widened
directory); a check over `hard_max_bytes` is ended. The Storage Budget (`af storage prune
--apply`, and the sweep a run starts with) evicts only a layer no check holds: it
reads the lock when it lists the layer and takes it before removing anything, so a held layer is
refused and left in place. When a layer nevertheless disappears under a running check, the check
names the cause instead of a bare deadline: `layer evicted during build by storage sweep at
<time>` when the sweep's eviction record is newer than the check, `by another process` otherwise.

## Offline checks

A check declared `offline = true` runs without network: npm reads only the layer's `npm_cache`,
every proxy variable points at the closed port `http://127.0.0.1:9`, and Cargo stays offline. Its
`npm_packages` pin each package by exact version and the SHA-256 of its registry tarball:

```toml
[checks.markdownlint]
name = "markdownlint"
required = true
offline = true

[[checks.markdownlint.npm_packages]]
spec = "markdownlint-cli2@0.22.1"
sha256 = "57f59b92ce95a5d0beb9d484bf5ecede02fde29f913ad1a6cc74b15901650037"
```

Before every run af finds each pinned tarball in the cache and checks its SHA-256. A pin the cache
lacks is filled once, with `npm exec --yes --ignore-scripts --package=<spec> -- true` while the
network is still allowed; after that the check never needs it. Bytes that differ from the pin
refuse the check (`not_run`), naming the package and the pin.

## Sealing leaves generated files out

When a Worker's sandbox is sealed into a candidate, the files it added that the tree's ignore
rules exclude are left out: af's defaults `__pycache__/`, `*.pyc`, `/target/` and `node_modules/`,
then every `.gitignore`, then `.af/seal-ignore`, matched as git matches them. A `!` rule stages a
path back, and a file the parent Snapshot already held is always sealed. What was left out is
listed in an `af.seal-ignored/1` record referenced from the candidate, and on stderr. A FIFO, a
socket or a device file in the tree stops the seal with its path; af never opens one.

## What a check records

Beside its receipt, a warm check node records `af/TaskCheckLayer@1`
([schema](../../schemas/task-check-layer-v1.json)). `af task show ID --checks` prints it under
each check, and `--json` carries it as `layer` in `af/task-checks@2`:

```text
check kernel: passed (exit 0) in 2342000 ms, cargo_target warm 12.0 GiB
  layer 1a2b3c4d5e6f/7a8b9c0d1e2f: warm at start, waited 61 s for af task run 4242 on claw-3; source groups 2 stale, 15 fresh
  phases: build 812000 ms of 2400000 ms, tests 1530000 ms of 3600000 ms
```
