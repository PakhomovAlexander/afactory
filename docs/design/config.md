# Afactory — configuration (TOML-first)

**Status:** v2 · part of [`overview.md`](overview.md). Everything a human writes
is TOML; everything the kernel writes goes to the Store ([`store.md`](store.md)) — with one
exception, `.af/af.lock`, which is machine-written TOML like `Cargo.lock`. Git versions the
declarations under `.af/`; it holds no state.

## 1. One precedence ladder

Lowest to highest. Tables deep-merge; scalars last-wins; **arrays replace** (an `extend_*` key
exists wherever an additive list is wanted); `enabled = false` is how a higher layer switches an
entity off, because TOML has no null.

| Layer | Where | Holds |
|---|---|---|
| built-in defaults | the binary | everything has a default; an empty `.af/af.toml` works |
| system | `/etc/af/config.toml` | fleet ceilings (optional) |
| user | `$XDG_CONFIG_HOME/af/config.toml` + `conf.d/*.toml` (XDG on macOS too, like jj and mise) | providers with auth references, the Store connection, personal defaults, trust, ui, `[self]` |
| directory | `.af/af.toml` in every ancestor **above** the git toplevel (or above cwd outside a repository), nearer wins; may carry `workers/` shared by the tree | a team's or a workspace's defaults: providers, shared packages, envs — trust-gated like a project |
| project | `.af/af.toml` at the git toplevel (found by walking up; no in-repo cascade — a monorepo uses `extend`) | envs, tools, defaults |
| project-local | `.af/af.local.toml`, gitignored (`af init` adds it to the excludes) | personal overrides for this checkout |
| profile | `--profile <name>` / `AF_PROFILE` selects `[profile.<name>.*]` overlays from any layer | cheap / ci / offline variants |
| environment | `AF_<TABLE>__<KEY>` (`AF_DEFAULTS__ENV=ci`) | CI knobs |
| command line | `-c 'env.default.network = "none"'` (a TOML fragment; string fallback), repeatable; `--isolated` ignores every file | one-off overrides |

`af config show --origin` prints every effective value with the file and line it came from, and
the directory file that supplied it. The directory layer is recorded in
[ADR-0044](../adr/0044-af-manages-itself-and-dispatches-to-the-pinned-release.md); branch-specific
configuration is git-managed — the project layer on a branch is what that branch commits.

## 2. `.af/af.toml` — the project

Annotated sketch; every table is optional.

```toml
#:schema https://afactory.dev/schema/af-1.json      # taplo / editor validation
version = 1
extend = "../.af/af.toml"                           # monorepo inheritance, one level

[project]
name = "my-project"
min_af = "1.0"                                      # soft gate; the hard pin is in af.lock

[defaults]
env = "default"
pipeline = "review"                                 # what `af review` runs
profile = "local"

[provider.claude]                                   # requirement only: kind, defaults — no auth here
kind = "claude-code"
model = "opus"

[provider.codex]
kind = "codex"

[env.default]
isolation = "host"
network = "none"
writable = ["."]
read_only = [".git"]
protected = [".git/hooks", ".git/config", ".af", ".claude", ".codex"]
caches = ["cargo"]
limits = { cpu = 4, memory = "8g", wall = "30m" }

[cache.cargo]                                       # a Cache Snapshot source, admin-approved
path = "~/.cargo/registry"
max_size = "4g"

[tool.shell]
kind = "shell"
rules = [
  { prefix = "cargo", decision = "allow" },
  { prefix = "git diff", decision = "allow" },
  { prefix = "git push", decision = "deny" },
  { prefix = "*", decision = "ask" },
]

[tool.lint]
kind = "command"
program = "npx"
args = [{ value = "--yes" }, { value = "markdownlint-cli2@0.22.1" }, { value = "**/*.md" }]

[tool.verify]
kind = "command"
program = "bash"
args = [{ value = "scripts/verify.sh" }]

[profile.ci.env.default]                            # overlay: deep-merged when --profile ci
isolation = "container"
image = "ghcr.io/org/dev@sha256:…"
```

Rules of the file: named tables for entities (`[tool.x]`, never `[[tool]]` — arrays do not
merge by key); at most three levels deep; a `kind` on every entity; references by name; a
comment on every non-obvious key. `af config set env.default.network none` edits through
`toml_edit`, preserving comments and order — prefer it to hand edits in scripts.

## 3. `~/.config/af/config.toml` — the person and the machine

```toml
version = 1
default_profile = "local"

[store]
backend = "sqlite"                 # the only backend for now; a shared backend comes later
path = "~/.local/state/af/store.db"          # sqlite; XDG state, never the repository
# url_env = "AF_STORE_URL"         # a future shared backend: its connection string comes from the environment, never this file

[provider.claude]
kind = "claude-code"
auth = "login"                     # use the machine's Claude Code login, read in place
home = "~/.claude"

[provider.codex]
kind = "codex"
auth = "login"

[provider.anthropic]
kind = "anthropic"
auth = "env:ANTHROPIC_API_KEY"     # a reference; never the key

[provider.gateway]
kind = "openai-compatible"
base_url = "https://llm.example.com/v1"
auth = "keychain:af/gateway-token"   # a secret reference, re-resolved on TTL or 401; never the token

[trust]
paths = ["~/src/**"]               # projects whose executable-bearing config is trusted

[ui]
color = "auto"                     # respects NO_COLOR
pager = "auto"                     # $PAGER

[self]                             # the binary managing itself (ADR-0044)
update_check = true                # a detached, rate-limited check after the result is written
check_every = "24h"
auto_update = "notify"             # notify | always | never; a project's af.lock pin is never affected
channel = "stable"                 # stable | rc
install_pins = true                # install a project's pinned version on demand
keep_versions = 3
```

Auth resolution per provider, in order: an explicit `env:` reference → a `helper:` command → the
OS keychain (`keychain`) → the harness's own login (`login`: Claude Code's keychain entry,
Codex's `auth.json`), read in place and never copied. Subscription logins are the user's own
credential: passed through untouched on the user's machine, never minted, stored, exported, or
used in CI paths.

## 4. `.af/af.lock` — what is pinned

Machine-written TOML, committed, like `Cargo.lock`:

```toml
version = 1
af = { version = "1.0.0", sha256 = { "aarch64-apple-darwin" = "…", "x86_64-unknown-linux-gnu" = "…" } }

[workers.architecture]
version = "2.0.0"
digest = "sha256:…"                # over every regular file in the package, symlinks refused

[pipelines.review]
digest = "sha256:…"

[extensions.af-env-e2b]
version = "0.2.0"
sha256 = "…"
```

Editing a package or pipeline without re-locking fails at load with the path and the command to
run (`af lock`).

## 5. Substituting an entity — the four ways

1. **Partially redefine it in a higher layer** — `.af/af.local.toml`: `[env.default]
   network = "ambient"`.
2. **Switch it off** — `[tool.github] enabled = false` (the no-null answer).
3. **Re-point a reference** — `[defaults] env = "ci"`, or a Task's `strategy.workers`.
4. **Patch locally, never commit** — `[patch.provider.claude] home = "/tmp/claude-test"`; the
   Cargo `[patch]` idea: applies transitively, refused in committed layers by the trust gate.

Adding an implementation of a kind is never a config change: it is a built-in adapter or an
`af-<kind>-<name>` executable.

## 6. Unix-native conventions

- Directories: `$XDG_CONFIG_HOME/af` (config), `$XDG_STATE_HOME/af` (embedded Store, trust,
  activation history), `$XDG_DATA_HOME/af/versions/<v>/` (installed binaries + receipts; the
  default is the `~/.local/bin/af` symlink), `$XDG_CACHE_HOME/af` (materialized envs, cache
  snapshots, blobs, the release check), `$XDG_RUNTIME_DIR/af` (locks, sockets). The same on
  macOS; no `~/Library` surprises.
- Environment: `AF_*` overrides, `AF_PROFILE`, `AF_STORE_URL` (secret-bearing connection
  strings only ever via the environment), `NO_COLOR`, `$EDITOR`, `$PAGER`, `CLICOLOR_FORCE`.
- Streams: stdout is the result, stderr is progress; `--json` (one document) and `--jsonl`
  (streams) on every read; stdin accepted for a prompt or a patch; `-` means stdin.
- Signals: `SIGINT` ends the turn gracefully (attempts are fenced and charged), `SIGTERM`
  kills; children run in their own process groups.
- Plugins: `af-<name>` on `PATH`; built-ins win; `af help <name>` delegates; `af-tui` is one.
- Self-management: `af self status|update|rollback|install|remove|prune|setup-shell|uninstall`;
  `af completions <shell>` (values completed live from the binary); `af help <topic>`; a
  project's `af.lock` pin is honored by dispatch — any `af` on `PATH` execs the pinned version.

## 7. Pitfalls this design absorbs

- No null in TOML → every optional has an explicit off-value; layering can always unset.
- Array semantics differ across tools (Cargo concatenates, Figment replaces) → replace, with
  `extend_*` where additive; documented once, enforced by the loader.
- `[[array-of-tables]]` entities cannot be overridden by key → named tables only.
- Deep nesting is TOML's weak spot → three levels, flat catalogs, dotted keys in overlays.
- Rewriting loses comments unless `toml_edit` → `af config set`/`af lock` are the only writers.
- Environments-as-tables with non-uniform inheritance (Wrangler, Netlify) → profiles are plain
  overlays with one rule: deep-merge, arrays replace.
- Eager loading is the startup cost, not argument parsing → the loader is lazy; a command opens
  only the layers and the Store connection it needs.
