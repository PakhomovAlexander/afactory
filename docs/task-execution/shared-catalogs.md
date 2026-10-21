# Git catalogs and Task-kind packages

`af catalog sync` captures shared Pipeline, Worker and Task-kind packages from one exact Git
commit. A catalog can import other catalog files within that same commit. Each additional Git
repository is an explicit sync. Task planning never downloads dependencies or installs tools.

```sh
af catalog sync --source ../team-pipelines --revision main \
  --manifest catalog.toml --destination .af/catalog/team --json
```

The source can be a local Git checkout, including a private repository already fetched using
the developer's Git tools, or a public HTTPS Git URL. HTTPS sync uses a bounded transport with
hooks, credential helpers, redirects and submodule recursion disabled. Credentials and remote
URLs with query tokens are not accepted in catalog definitions.

The shared file uses `schema = "af.shared-task-catalog/1"`. Its `packages` table has the same
exact `version`, `digest` and repository-relative `path` fields as the project Task catalog.
`imports = ["catalogs/workers.toml"]` names other files in that commit. Optional
`path_base = "manifest"` resolves package and import paths relative to the catalog file; omission
retains repository-relative paths. This allows an [exported bundle](export.md) to move as a unit.
There is no namespace
precedence: ambiguous package names are errors. A shared catalog cannot contain `local/*`
packages. Worker input/output schemas are validated during sync using the runtime's contract
capture, without executing the Worker.

Sync publishes only to an absent destination given as a safe path relative to `--repo` (default
`.`): an absolute path, an empty, `.`, `..` or `.git` segment, or a path through a symlink or
non-directory is refused, and an existing directory is never overwritten. The imported
`catalog.lock.json` records the exact commit, requested revision, catalog content identities and
package digests. Package bytes and their original catalog files are stored beside it. Machine
paths and private remote URLs are represented by a source identity hash. Import depth is at most
four, with 32 catalogs, 128 packages, 4,096 files and 64 MiB of captured source bytes. Each
package retains the runtime's 16 MiB bound. These are capture limits; the explicit HTTPS
transport also has a 120-second process bound.

After reviewing the import, add its lock to the committed project `.af/task-catalog.toml`:

```toml
schema = "af.task-catalog/2"
code_policy = ".af/code-policy.toml"
imports = [".af/catalog/team/catalog.lock.json"]

[packages]
# Additional project packages, if any.
```

Retain the project's existing independence, Review and Provider settings. Importing does not
modify those settings or activate a catalog. Commit the lock, imported files and project
configuration together. A mutated package, catalog provenance file or duplicate namespace
fails admission. An existing Task uses its captured closure even after an import is edited or
the original Git repository becomes unavailable.

## Pin a package

`af catalog digest PACKAGE_DIR` prints the `digest` a pin records for one local Task package
directory, the directory its pin's `path` names. It is the recipe and validation Task capture
uses ([ADR-0157](../adr/0157-print-a-task-package-digest-with-the-pin-recipe.md)), so a script
needs no copy of either:

```sh
af catalog digest .af/task-packages/team/implementer
# sha256:<64 lowercase hexadecimal digits>
```

Stdout is that one line and nothing else. Every regular file under the directory contributes:
its path relative to the directory, with `/` separators, and its bytes. That includes the
manifest, `instructions.md`, schemas, `outputs/` and any other file, however deep. File modes,
timestamps, empty directories and the directory's own name or location do not contribute.
Capture hashes the package's files in the captured Snapshot, so a file the Snapshot leaves out,
such as an ignored one, changes the printed digest but not the captured one.

The package must be one capture admits: exactly one `pipeline.toml`, `worker.toml` or
`kind.toml` at its root, whose `name` is a package name, whose `version` is one exact
`major.minor.patch` release and whose full manifest parses; regular files only, with no symlink
and no path a catalog cannot hold; at most 4,096 files and 16 MiB, checked against each file's
size before it is read, so an oversized package is refused without being loaded. Every entry is
opened without following a symlink, so one swapped in while the package is read is refused, not
followed, and the directory read must be the very one checked, so a package directory replaced
meanwhile, even by another valid package, is refused rather than digested. A symlink named as the package is refused however it is spelled, `LINK/` and `LINK/.`
included. An absent directory, a symlink, or an incomplete or malformed package prints nothing
on stdout, says why on stderr and exits 1. The command never compares against or changes a
catalog or pin, writes a file, runs a Worker, contacts a Provider or installs anything. It
always runs as the `af` you invoke: a project's `af` pin never dispatches it to another
release, and it schedules no update check, so it never installs or activates a release. Put the
printed digest and the manifest's version in the package's `[packages."NAME"]` entry and review
the change like any other pin.

## Task-kind packages

Task-kind packages add reusable business names for installed acceptance profiles. For example,
`kind.toml` can declare:

```toml
schema = "af.task-kind/1"
name = "team/feature-kind"
version = "1.0.0"
kind = "team/feature"
profile = "reviewed_implementation"
```

Pin that package and add `[kinds] "team/feature" = "team/feature-kind"` to the project catalog.
Tasks with `kind = "team/feature"` then require embedded Review acceptance; their selected
Pipeline must accept that kind. The kind package becomes an exact plan dependency. A Task-file
verification override cannot weaken its profile.

The installed profiles are exactly `implementation`, `reviewed_implementation`,
`repair_allowed_implementation`, `review`, `document`, `report`, `optimization_analysis` and
`optimization_candidate`; a `kind.toml` naming any other `profile` is refused. A package only
gives a business name to one of these engine-owned profiles: it cannot install an arbitrary
domain handler or replace the engine's acceptance rules. These are not the starter names that
`af catalog init --profile` takes ([starters](starters.md)).

Tests sync a transitive catalog, remove its original repository, plan an imported Task, modify
the imported Worker and execute the captured plan. Fresh admission rejects the mutated package.
Separate cases reject catalog cycles, missing dependencies, namespace collisions and symlinks.
