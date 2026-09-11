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
`imports = ["catalogs/workers.toml"]` names other files in that commit. There is no namespace
precedence: ambiguous package names are errors. A shared catalog cannot contain `local/*`
packages. Worker input/output schemas are validated during sync using the runtime's contract
capture, without executing the Worker.

Sync publishes only an absent destination. The imported `catalog.lock.json` records the exact
commit, requested revision, catalog content identities and package digests. Package bytes and
their original catalog files are stored beside it. Machine paths and private remote URLs are
represented by a source identity hash. Import depth is at most four, with 32 catalogs, 128
packages, 4,096 files and 64 MiB of captured source bytes. Each package retains the runtime's
16 MiB bound. These are capture limits; the explicit HTTPS transport also has a 120-second
process bound.

After reviewing the import, add its lock to the committed project `.af/task-catalog.toml`:

```toml
schema = "af.task-catalog/1"
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
verification override cannot weaken its profile. `implementation`, `reviewed_implementation`,
`repair_allowed_implementation` and `review` are executable profiles at this checkpoint. `document` is a recognized package
declaration whose runtime capability belongs to P13. Packages cannot install arbitrary domain
handlers or replace engine acceptance rules.

Tests sync a transitive catalog, remove its original repository, plan an imported Task, modify
the imported Worker and execute the captured plan. Fresh admission rejects the mutated package.
Separate cases reject catalog cycles, missing dependencies, namespace collisions and symlinks.
