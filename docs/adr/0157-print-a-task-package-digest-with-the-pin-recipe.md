# ADR-0157: Print a Task package digest with the pin recipe

**Status:** accepted (2026-10-20).

## Context

[Issue #150](https://github.com/PakhomovAlexander/afactory/issues/150). A Task catalog pins
every package by `digest`, and capture refuses a package whose bytes disagree with its pin.
No command printed that digest. An author who changed a package had to rebuild the recipe
from AF source; the reporter kept a separate script for it. A second recipe drifts when the
canonical encoding or the package-content rules change.

## Decision

`af catalog digest PACKAGE_DIR` prints the digest a pin records for one local Task package
directory: one line, `sha256:` and 64 lowercase hexadecimal digits, and nothing else on stdout.
It sits beside `af catalog init`, `test` and `sync` because pins live in catalogs.

There is one recipe. `review_config::lock::package_files` reads the directory with the walk
`package_digest` makes: every regular file keyed by its `/`-separated relative path, a
symlink, a non-regular file or a non-UTF-8 path refused. `package_digest_from_files` hashes
it. `TaskPlanCompiler::local_package` then validates the bytes as `capture_package` does: safe
paths, capture's bounds of 4,096 files and 16 MiB, exactly one `pipeline.toml`, `worker.toml`
or `kind.toml` at the root, a package name and exact version from that manifest, and the same
`parse_package` call capture makes. A digest is printed only for a package capture would admit
under that pin. Any refusal prints nothing on stdout, names the reason on stderr and exits 1.

The walk never reaches outside the package. It classified each entry with `lstat` and then
reopened it by pathname, so a writer could swap a checked file or directory for a symlink in
between and have bytes from elsewhere digested. Every directory and file below the root is now
opened relative to its parent's descriptor with `O_NOFOLLOW`, and the opened object must have
the device and inode that were classified. A symlink swapped in is refused as a symlink, and
any other replacement, a FIFO included, is refused rather than read. `package_files` also opens
the root itself with `O_NOFOLLOW`, as its last component relative to its parent directory:
`O_NOFOLLOW` binds only a path's final component, and in `LINK/` or `LINK/.` that is no longer
the link, so trailing separators and `.` components are dropped lexically first and nothing is
resolved to make a link acceptable. `LINK`, `LINK/`, `LINK/.` and their repeated forms are all
refused, while `DIR/` and `DIR/.` digest as `DIR`. A reviewer `Registry` opens the package root it
located the same way, and the opened directory must have the device and inode `locate` checked, so a
root renamed away and replaced by a symlink or another directory in between is refused.
`af catalog digest` binds its root the same way: the device and inode of the directory its
no-follow check saw are the ones the opened root must have, so a root replaced after that check
by a different, equally valid package is refused instead of digested. Only
the public `package_digest`, which callers hand a path they chose, still follows a symlinked
root, as before.

The bounds are enforced before bytes are read, not after. `package_files` takes a
`PackageBounds`; the walk refuses the file past the file bound before opening it, compares
each opened file's size with what is left of the byte bound before reading it, and reads at
most one byte past that remainder, so neither a huge sparse file nor one that grows while it is
read is ever held whole in memory. The refusal is `LockError::TooLarge`. The bounds only stop
a walk; they never change which bytes a digest covers.

The command reads the package's files and nothing else that matters to the result. It never
compares against or changes a catalog or pin, writes a file, runs a Worker, contacts a Provider
or installs anything. It has no `--json`: the single line is the whole result. That holds for
af's own state too. `main` parses a `catalog digest` invocation and runs it before every
self-management hook, so a project's `af` pin never dispatches it to (and so never installs)
another release, and no update check is scheduled after it, even with `auto_update = "always"`.
`exempt_from_dispatch` lists `catalog digest` as well, so a malformed invocation that falls
through to the ordinary path dispatches and schedules nothing either.

## Consequences

Every regular file under the directory contributes its relative path and bytes, the manifest
included. File modes, timestamps, empty directories and the directory's name or location do
not. Capture hashes a package's files from the captured Snapshot, so a file the Snapshot leaves
out, such as an ignored one, changes this digest but not the captured one.

The issue's workaround matched ten pins that AF published when it was filed; it did not list
them, and AF's own catalog has since grown. `crates/af/tests/it/catalog_digest.rs` reproduces
every pin in AF's `.af/task-catalog.toml` and in every runtime fixture catalog except
`bound-inputs`, whose placeholder digests its test replaces before planning. It also checks
that changed instructions, manifest bytes, an added file or a renamed file change the digest,
that the command leaves the package unchanged, and that absent, empty, manifest-less,
two-manifest, malformed, floating-version, protocol-invalid, symlinked and non-directory
packages exit 1, a symlinked root spelled with trailing separators or `.` components included,
while the same spellings of a package directory print its digest. Those mutation and refusal cases run on private copies written byte for byte
into a temporary directory, never on the checkout: the checkout may be read-only, and a copy
that kept its source's file modes could not be edited. Unit tests in
`crates/review-config/src/task/catalog/local/tests.rs` pin the same rules on the library
function. `crates/review-config/src/lock/tests.rs` replaces a file and a directory with a
symlink, with an object moved in from outside and with a FIFO, deterministically, between the
walk's check and its open, and proves each is refused and nothing outside is read. It also
swaps a registry's located package root for a symlink and for another directory before the
read opens it, and proves both refused. `local/tests.rs` renames a checked digest root aside and
moves a different valid package into its place before the walk opens it, and proves that refused
with no digest printed, alongside a symlink swapped in as `DIR`, `DIR/` and `DIR/.`. It proves a bounded walk refuses a 2 GiB sparse file
by its size, one file or byte past its bounds and a file grown past them mid-walk, while it
admits exactly its bounds. `the_digest_never_dispatches_installs_or_schedules_an_update_check`
runs the command, and a malformed form of it, in a project pinned to an uninstalled release
with a due check and `auto_update = "always"`, and proves the running af printed the digest,
nothing was installed, activated or scheduled, and af's state, cache and versions are
unchanged.
