# Plugins: every feature that is not the kernel, as an executable af finds, runs and pins

**Status:** proposed 2026-10-19; revision 22 after review rounds 1 to 21 (Codex gpt-6-astra, xhigh).
**Baseline:** kernel `main` bf027de6 (v0.12.0-rc.4).
**Vocabulary:** [`CONTEXT.md`](../../CONTEXT.md). **Values:** [`../values.md`](../values.md).
This note is direction, not shipped behaviour; where it and an accepted ADR disagree, the ADR
wins. The boundaries of §3.1 are proposed as one ADR under the next free number.

The owner's requirement, in their words: af is developing rapidly and must be able to take a
massive amount of features in parallel, some of them removed again after an experiment. The
architecture must be ready for that pace: a plugin architecture in which most af features are
plugins with zero kernel modifications, so that the kernel can be understood once and is not
rewritten feature by feature. Examples: `af analytics`, `af backlog`, `af reviewer`. The design
must be accurate, and the kernel must keep as little API and code as possible.

## 1. Problem

The figures are from this repository and this machine, measured 2026-10-19 on `bf027de6`.

**The pace is real, and it lands in the same files.** 87 pull requests merged and 511 commits
since 2026-09-19; the last 60 commits of `main` touch 403 files with 80,693 insertions. The
workspace is 294,000 lines of Rust in 16 crates; `crates/af`, the binary, is 108,000 of them.
The files every feature touches, with their commits since 2026-09-19:

| File | Lines | Commits in 30 days | What a feature adds there |
|---|---:|---:|---|
| `crates/af/src/main.rs` | 4,293 | 39 | an arm in the command `match` |
| `crates/af/src/cli.rs` | 1,565 | 32 | a clap variant per command and flag |
| `crates/af/src/tui/mod.rs` (TUI: 14,489 lines) | 1,774 | 32 | a pane, a folder, keys |
| `crates/af/src/topics.rs` | 231 | 15 | a help topic |
| `crates/af/src/selfmgmt.rs` | 2,019 | 15 | a dispatch exemption |
| `crates/review-core/src/event.rs` | 1,078 | 13 | an `EventType` variant (42 today, closed by design) |
| `crates/af/src/storage.rs` | 2,141 | 9 | a storage kind under the budget |
| `crates/review-core/src/task/pipeline.rs` | 471 | 7 | a `TaskOperatorV1` variant (28 today) and its host arms in five files |
| `schemas/` | 184 files | 69 | a `kind@version` document, schema and parity test |

Add a config table in `config.rs` (and the `MACHINE_ONLY` list), a `.af/` entry in
`review_config::layout::LAYOUT` with its drift test (ADR-0115), and a `--json` document for the
TUI to project. That is the checklist a feature pays today, whether it is a kernel invariant or
a dashboard.

**The two designs in flight show it.** The Project note adds an `af project` namespace of eight
subcommands, two config tables, a Store kind, six record types and a root default; the telemetry
note adds `af telemetry` with twenty subcommands, a leaf crate, a config table, a storage kind and
a TUI pane. Both are right to be built. Both land in every file in the table above, and both are
the kind of feature the owner wants to add in parallel and keep or drop after an experiment.

**The values already decided the shape, and nothing implements it.** Value 6 says adding a
provider, environment, tool, Worker or store backend "needs no kernel change: executables on
`PATH` speaking a versioned JSON contract (`af-<kind>-<name>`, the `git-*`/`cargo-*`
convention)". Value 8 says "Plugins are executables on `PATH`; the TUI is one of them, not the
kernel". Today `cli.rs` declares no external subcommand, so `af foo` is a usage error; nothing
looks for an `af-*` executable; the TUI is inside `crates/af`; and the second-implementation
rule is met only in-process (`WorkerModelAdapter` has the Claude and Codex adapters,
`TaskSource` has local and Jira, `ReleaseSource` has GitHub and a directory).

**Six things already work exactly the way a plugin should, and are not named as such.**

1. A Worker package is a digest-pinned directory; `[runner] kind = "command"` runs any program
   with the typed `ReviewerInputs` or Worker input on stdin and a typed result on stdout, in a
   sandbox, with no Provider and no tokens (`fixtures/self-optimizer/catalog/*/worker.toml`).
2. `af catalog sync` captures packages from an exact Git commit by digest, and a project
   activates them with one committed edit (ADR-0053).
3. The TUI's rule: a pane is a projection of what a CLI document prints, and every mutation goes
   through the CLI's own path (`docs/design/tui.md` §1, ADR-0119, ADR-0123).
4. `af self` installs versions into `$XDG_DATA_HOME/af/versions/<v>/` with a receipt, verifies
   signed checksums, and `af` execs the release a project's lock pins (ADR-0044, ADR-0045).
5. `af self refresh-check` is a detached child that does one bounded thing and exits; the
   kernel runs no daemon and polls nothing.
6. Every reading command prints one `kind@version` document under `--json`, and a payload shape
   change bumps the version (`af help json`, ADR-0145).

The plugin architecture is these six things, named and completed by the one seam that is
missing: a command af does not know is a plugin af runs.

## 2. Terms

Proposed `CONTEXT.md` entries. Existing terms keep their meanings.

**Kernel**:
The `af` binary and the crates behind it: the only code that holds authority. It captures
Snapshots, keeps the Store, reserves budgets, fences Attempts, materializes sandboxes, compiles
plans, runs acceptance operators, admits Providers, manages its own versions and pins, and prints
the documents everything else reads.
_Avoid_: "af" for the kernel alone — `af backlog` is af to the person typing it, and a plugin.

**Plugin**:
An installed executable af invokes by name as a child process, with the operator's privileges,
that extends af through the seams of §3.2 and nothing else. It reads the kernel only through
`af` documents, writes into it only through a Task file or a file-bound input, keeps its own
state in its own directory, and never decides anything the kernel decides.
_Avoid_: a Worker package, which runs inside a sandbox under a budget; a Provider, which is an
authentication context; a crate, which is kernel code.

**Runner adapter**:
A plugin whose manifest declares `runner = "<kind>"` and that the kernel hands one Attempt's
sandbox and exact inputs, under the rules the native adapters run under (§3.14). It is the one
plugin that touches a sandbox, and it touches only that one.
_Avoid_: a Provider, which is the authentication context a runner adapter is admitted under.

**Plugin manifest**:
The `plugin.toml` (`af.plugin/1`) beside a plugin's executable: its name, version, summary,
protocol, minimum af release, and the seams it uses (hooks, a config table, state, a pane,
shipped packages). The kernel reads nothing else about a plugin.

**Hook**:
A lifecycle point after which the kernel runs every enabled plugin that declared it, handing it
one `af/plugin-hook@1` document naming what finished. A hook runs after the result is durable
and printed, can change no outcome, and is one stderr line when it fails.
_Avoid_: a git hook, which runs before and can refuse; nothing here can.

**Plugin state**:
`$XDG_STATE_HOME/af/plugins/<name>/`: the one directory a plugin writes its durable records to,
counted by the Storage Budget and never evicted by the kernel. Its cache,
`$XDG_CACHE_HOME/af/plugins/<name>/`, is evicted whole like any other cache kind.

**Conformance plugin**:
The credential-free fixture plugin the kernel ships (`fixtures/plugins/echo/`) that exercises
every seam; `af plugin check DIR` runs the same checks against any plugin directory.

## 3. Design

### 3.1 Boundaries

The clauses proposed for the ADR. Each is pinned by a test named in §5.

1. **A plugin is a separate executable.** The kernel never links, loads or calls plugin code in
   its own process: no dynamic library, no embedded interpreter, no Cargo feature, no trait
   registry. A plugin is found only in af's own plugin directory or on `PATH`, and no
   configuration key, in any layer, ever names an executable. The trust gate ADR-0044 declared
   for executable-bearing keys stays unneeded: the ladder carries none.
2. **A plugin reads the kernel only through `af` and its declared exports.** Its reader is the
   command line's versioned `--json` documents, the `af store` read commands of §3.6, and the
   kernel exports of §3.6: directories the kernel writes in a versioned, documented format for
   readers and never consults for an execution, replay, acceptance, admission or Worker-input
   decision, reading them only to recover their integrity and for the Storage Budget's
   housekeeping (§3.6), listed one by one in the surface table. It
   never opens `events.sqlite`, the `attempt_wall` sidecar, the CAS, a Warm Workspace or a
   Worker sandbox. The runner adapter
   of §3.14 is the one exception: it receives exactly one Attempt's sandbox and exact inputs from
   the kernel, under the access class, admission, cancellation and accounting rules the native
   adapters run under, and nothing else.
3. **A plugin writes into the kernel through exactly two forms**, both captured and pinned:
   a Task file it starts (`af task start --file`), and a file it binds to a declared root input
   port (§3.7). Everything else a plugin writes is its own state.
4. **A plugin never decides.** It cannot refuse, approve, suspend or alter a Task, a Round, a
   plan, a budget, a sandbox, a Worker input or an exit code. A pipeline that wires a
   plugin-bound input gives a Worker data, never authority: such an artifact can satisfy no
   coverage obligation and reach no `accept` or `review_accept` node.
5. **Replay never needs a plugin.** A plugin-bound input is captured like any other input;
   Task replay never invokes, locates or requires an installed plugin, and a removed plugin
   leaves every recorded Task replayable. Kernel housekeeping may name one: the sweep
   observation a Task records (ADR-0144) names the kind and path of every entry it evicted,
   plugin kinds included, under a new version of that record (P4); and the execution binding
   of a Task that ran a Worker through a runner adapter names that adapter exactly (§3.14),
   because captured authority must name what executed (ADR-0046), and replay validates that
   record without the executable. No other event, artifact or sidecar row names a plugin.
6. **Every contract is versioned under ADR-0145.** The invocation protocol has a number; the
   manifest, the hook document, the pane document, the store documents and the lock table have
   `kind@version` shapes. A changed shape is a new version; the old one is refused by name on
   both sides.
7. **Nothing a repository commits installs, enables or runs a plugin.** `.af/` may require a
   plugin by name, pin its bytes, configure it and carry its declarations; a machine installs
   it. Machine-only tables stay machine-only, and a plugin may declare its own table so.
8. **Plugin bytes are bounded and visible.** Installed versions, state and cache are three
   storage kinds `af storage` shows per plugin; cache is evicted whole, least recently used, by
   the kernel; state is reduced only by the plugin's own `storage.sweep` hook or by the
   operator.
9. **The surface is one table.** Everything the kernel offers a plugin is rendered by
   `af help plugins` from one declared table in the kernel's source, and a test asserts the code
   that implements the surface carries nothing the table does not name, as the `.af/` layout is
   tested (ADR-0115).

### 3.2 The seams

| Seam | What a plugin supplies | How the kernel finds it | Kernel code | Package |
|---|---|---|---|---|
| Command | `af <name> …` | the manifest's `bin`, or `af-<name>` on `PATH` | `plugins::invoke` | P1 |
| Help | `af help <name>`, a line under `Plugins:` in `af --help` | the manifest's `summary`; `--help` is the plugin's | `plugins::manifests` | P1 |
| Config | `[plugin.<name>]` in the ladder | opaque table, deep-merged like every other | one passthrough | P1 |
| Authority | `.af/plugins/<name>/` | one `LAYOUT` entry | one entry | P1 |
| Worker packages | `packages/` in the plugin, pinned by the project | `task-catalog.toml`, `af.lock`, as today | none | exists |
| Reading | `af … --json`, `af store …` | the documents | `af store` | P2 |
| Task files | `af task start --file` | as today | none | exists |
| Bound inputs | `inputs.<port> = { "file": …, "type": … }` | the Task-file adapter | one binding form | P3 |
| Hooks | `hooks = [...]` in the manifest | `plugins::after_command` | one call site | P4 |
| State and cache | its own directories | three storage kinds | storage kinds | P4 |
| Pins | `af.lock [plugins.<name>]` | dispatch to the pinned version | `plugins::dispatch` | P5 |
| Pane | `af <name> pane --json` | one generic renderer | `tui/panes/plugin.rs` | P6 |
| Runner | `af-runner-<kind>` | the Provider registry's `kind` | one adapter | P7 |

Everything in the "Kernel code" column lives in new files; the existing files gain call sites,
listed in §3.16.

### 3.3 The command seam

`af <word> …` where `<word>` is no kernel namespace resolves, in this order:

1. The installed plugin `<word>`: `$XDG_DATA_HOME/af/plugins/<word>/current/`, whose
   `plugin.toml` names the executable (`bin`, relative to the directory).
2. An executable `af-<word>` on `PATH`: a development plugin. It is run as found and reported as
   `dev` by `af plugin list`; it never satisfies a pin (§3.13) and ships no manifest unless a
   `plugin.toml` sits beside it. `[plugins] path = false` in a machine layer turns this off.
3. Otherwise clap's usage error, exit 2, as today.

A foreground invocation replaces the `af` process with the plugin's: a Unix `exec`, after the
overlay is set and the shared locks of §3.9 are taken on descriptors without close-on-exec,
so the locks live exactly as long as the plugin. There is no launcher left for a signal to miss:
`SIGINT` and `SIGTERM` from the terminal reach the plugin and its children directly (`af
backlog` forwards them to its Task child, §3.17), and the shell sees the plugin's own exit
status (128 plus the signal number for a signal). The rest of the command line are its
arguments, stdin, stdout and stderr are inherited, and the working directory is the caller's.
The outer background invocations, a hook, a pane load and the runner, are supervised children
in groups of their own (§3.8, §3.12, §3.14); a plugin one of them dispatches through `$AF_BIN`
is `exec`ed inside that same group (§3.8). `af <word> --help` is the plugin's help; `af help
<word>` runs `<plugin> --help`. Clap gains one `external_subcommand` variant; nothing else in
`cli.rs` changes for any later plugin.

A plugin inherits the operator's environment, so `HOME`, the XDG variables, `PATH` and every
`AF_*` override, the `AF_PLUGIN__<NAME>__<KEY>` overrides of §3.10 included, reach it and the
`af` children it runs. The invocation protocol is the overlay the kernel adds, after removing
exactly the six invocation-owned variables (`AF_PLUGIN_PROTOCOL`, `AF_PLUGIN_NAME`,
`AF_PLUGIN_DIR`, `AF_PLUGIN_STATE`, `AF_PLUGIN_CACHE`, `AF_REPO`) an enclosing invocation may
have left, so that a plugin which leaves its repository and calls another never hands it a
stale `AF_REPO`; `AF_REPO` is set again only from the new invocation's own resolution. That
resolution happens once, by a plugin-context resolver of its own: the kernel-owned `--repo`
before the plugin name, canonicalized, else the working directory, either taken to its Git
toplevel with `config::git_toplevel` as `load_rooted` and `selfmgmt::pinned` do, so that
`/repo/subdir` names `/repo` on both routes (`task_execution::state_path` only canonicalizes
the directory it is given and registers a Task Store, which plugin resolution never does);
and its one result
selects the bundle (§3.13), the `enabled` filter (§3.10), the configuration the plugin then
reads with `--repo "$AF_REPO"` (`load_rooted` loads the repository it is handed) and `AF_REPO`
itself, so a plugin is never selected by one repository and configured by another.
Nothing else is removed, and `AF_PLUGIN_BACKGROUND` in particular is inherited and never
cleared (§3.8):

| Variable | Holds |
|---|---|
| `AF_PLUGIN_PROTOCOL` | `1`; a plugin whose manifest declares another protocol is refused before it runs |
| `AF_PLUGIN_NAME` | the plugin's name |
| `AF_PLUGIN_DIR` | the directory holding its manifest and executable |
| `AF_PLUGIN_STATE` | `$XDG_STATE_HOME/af/plugins/<name>/data`, the payload under the plugin's control directory (§3.9), created on first use when the manifest declares `storage = "state"` |
| `AF_PLUGIN_CACHE` | `$XDG_CACHE_HOME/af/plugins/<name>/data`, likewise for `"cache"` or `"state"`; the entry the kernel evicts whole (§3.9) |
| `AF_BIN` | the absolute path of the running `af`: the plugin calls this, never `af` from `PATH` (ADR-0123) |
| `AF_VERSION` | the running release |
| `AF_REPO` | the repository the invocation resolved: the kernel-owned `--repo` before the plugin name, else the git toplevel the command was run in; absent outside a repository when no `--repo` was given |
| `AF_DISPATCHED_FROM` | set as the browser's hand-off sets it, so the plugin's own `af` calls run this binary and never dispatch again |
| `AF_PLUGIN_BACKGROUND` | only for a background invocation: `hook:<name>` for a hook, `pane` for a pane load; inherited by every child and never cleared, and while it is set the kernel runs only its read-only table (§3.8, §3.12) |

The plugin reads its configuration with `"$AF_BIN" config show --json`, adding
`--repo "$AF_REPO"` only when `AF_REPO` is set, and takes `plugin.<name>`; origins come with
`--origin`. The kernel passes no configuration itself, so a plugin's keys are its own and
`af config show` prints them like every other table. The manifest that governs a plugin's
table (`machine_only`, §3.10) is the one in the bundle the selection rule of §3.13 picks for
the repository in question, by the one resolver every use shares, commands, `config show`,
hooks and panes: `AF_PLUGIN_VERSION_<NAME>` first, as `AF_VERSION` precedes the project pin in
`maybe_dispatch`, then the pin, then `current`, then the `PATH` development fallback where it
is enabled;
`config show` run under an invocation's overlay (`AF_PLUGIN_NAME` and `AF_PLUGIN_DIR` set) for
the repository `AF_REPO` names does not run the selection again, because another process can
change it in between (a pin moved to an installed version with another `machine_only` after
the plugin started; `load_rooted` and `selfmgmt::pinned` re-read the filesystem, and the
plugin locks do not cover the lockfile): it filters `plugin.<name>` with the manifest at
`AF_PLUGIN_DIR`, the bundle already running, re-read with the bounded read, and fails closed,
treating the table as machine-only, when that manifest is gone, names another plugin or
cannot be read. A `config show` with no overlay, or for another repository, resolves the
current selectors. So one invocation never sees values another version's manifest would have
filtered.

Names are two to thirty-two characters: a letter first, then alphanumeric segments joined
by single hyphens, no hyphen at either end and none doubled (`[a-z][a-z0-9]*(-[a-z0-9]+)*`),
so that the environment encoding of §3.10 is unambiguous. Reserved: the kernel's command names (`review`, `task`,
`catalog`, `provider`, `onboard`, `config`, `self`, `storage`, `completions`, `help`, the
`plugin` and `store` this note adds) and every help topic (`config`, `layers`, `environment`,
`exit-codes`, `json`, `self`, `storage`, and the `plugins` topic this note adds), derived from
the clap and `TOPICS` declarations rather than listed by hand, so a kernel word can never be
shadowed by a plugin on either discovery route: a manifest naming one is refused at install,
and an `af-<word>` on `PATH` with a reserved word is ignored and reported by `af plugin list`.

`af --help` lists installed plugins under `Plugins:` with their summaries, read from the
manifests at `$XDG_DATA_HOME/af/plugins/*/current/plugin.toml`; the filesystem is the record,
as it is for `af self`, and there is no registry file. A manifest is read the way a Task file
is read (§3.7): a non-blocking open, a check that the opened object is a regular file, and a
bounded read with a ceiling of 64 KiB, at install, at `--link`, and at every discovery, so a
FIFO behind a development link or an oversized file can hang nothing. A manifest that fails
that read or does not parse makes `af --help` list the plugin as `invalid` on one line and
skip it; help never fails for a plugin's sake. `af --help` keeps its 50 ms bound (value
3) with twenty plugins installed, and P1 measures it against the baseline before it ships (§5).
`PATH` is never scanned for help.

Dispatch gains one classification rule, because an external command owns its argument tail:
today `exempt_from_dispatch` scans the whole argv for `--help`, `-h`, `--version` and `-V`,
and `repo_from_argv` scans it for `--repo`, so `af echo --help` would skip the project's pin
and a plugin-owned `--repo` would select another repository's release and hand that binary on
through `AF_BIN`. Once the external command name is reached the remaining words are opaque to
the browser; only kernel-owned selectors, `--repo` before the plugin name and `AF_VERSION`,
choose the repository and the release, and the repository they choose is the one `AF_REPO`
carries. With that, `af <plugin>` inside a pinned project first
execs the release the lock pins, which then runs the plugin (ADR-0044). The machine-management commands `af plugin
install|remove|list|show|check|hook-run` are exempt, like the binary-management `self`
commands (ADR-0105); `af plugin pin` writes repository authority and dispatches like
`onboard`, so the lock is written by the release the project pins. When that release predates
this design the dispatching binary refuses `af plugin pin` before the exec, naming the upgrade
(`af onboard --refresh-lock --af <release>`, the pin-upgrade path ADR-0045 defines), rather
than letting the default binary write a shape the pinned release would reject. A pinned release older than this design answers `af <plugin>` with its usage error,
the same way it answers any command it does not have.

### 3.4 The manifest

```toml
schema = "af.plugin/1"
name = "backlog"
version = "0.3.0"
summary = "Order work items and run them as Tasks"
protocol = 1
min_af = "0.13.0"
bin = "bin/af-backlog"            # canonical relative path to a regular file in the bundle

# Optional. Omitted means: no hooks, no table, no storage, no pane, no init, no packages.
hooks = ["task.run.ended", "task.delivered"]
hook_mode = "detached"            # or "sync"
hook_wall_ms = 600000             # detached: default 600000, at most 3600000; sync: default 60000, at most 60000
config = "backlog"                # the [plugin.backlog] table; machine_only = true allowed
storage = "state"                 # none | cache | state (state implies cache)
pane = true                       # `af backlog pane --json` exists
init = true                       # `af backlog init --repo PATH [--apply]` exists (§3.11)
packages = ["packages/*"]         # Worker, Pipeline and kind packages shipped for projects
```

`min_af` is checked against the running release before the plugin runs; `af <name>` under an
older release prints the manifest's requirement and the install command. `bin` is a canonical
relative path (no empty, `.` or `..` component) that must resolve to a regular executable file
inside the installed version directory and never to the root `receipt.toml`, which is the
kernel's and is outside the content digest (§3.15): a manifest naming it is refused at
install, at `--link`, by `af plugin check` and before every private invocation, so the
resolved executable always has an entry in the content manifest. A symlink anywhere in a
bundle, and an archive entry that would land outside it, are refused at install before any
receipt is written, so the bytes a pin verifies are the bytes that run. The schema is `schemas/plugin-v1.json` with the parity
tests every shape gets; unknown keys are refused.

### 3.5 Installing, removing, pinning

```text
$XDG_DATA_HOME/af/plugins/<name>/<version>/plugin.toml      the manifest
$XDG_DATA_HOME/af/plugins/<name>/<version>/bin/af-<name>    the executable
$XDG_DATA_HOME/af/plugins/<name>/<version>/packages/        shipped packages, when any
$XDG_DATA_HOME/af/plugins/<name>/<version>/receipt.toml     from where, verified how
$XDG_DATA_HOME/af/plugins/<name>/origin.toml                the release source and key this machine installs from
$XDG_DATA_HOME/af/plugins/<name>/current -> <version>       the active version; the link is the record
$XDG_STATE_HOME/af/plugins/<name>/                          state (§3.9)
$XDG_CACHE_HOME/af/plugins/<name>/                          cache (§3.9)
```

| Command | Does |
|---|---|
| `af plugin install SOURCE [--version V] [--key FILE]` | installs from a release source: `owner/repo` (GitHub through `gh`, as `af self` does), a local archive, or a release-source directory in `DirSource`'s layout (the versioned per-target archives beside their `SHA256SUMS`, never an unpacked bundle, which is what `--link` is for); verifies the archive the same way whatever the source, so the receipt's digest is the digest a pin names; writes `origin.toml` (`af.plugin-origin/1`: the source and the key) on a first install, and refuses a later install from another source unless `--origin` replaces it; makes it current |
| `af plugin install --link DIR` | a development install: `current` links the directory, no receipt, reported `dev` |
| `af plugin remove NAME [--purge]` | removes every installed version; `--purge` also removes state and cache; refuses a version a project seen on this machine pins unless `--force` |
| `af plugin list [--json]` | installed plugins with version, source and receipt; `dev` plugins on `PATH`; what the project requires and pins |
| `af plugin show NAME [--json]` | the manifest, the receipt with both digests (§3.15), the directories and their bytes |
| `af plugin check [DIR] [--peer DIR]...` | the conformance checks of §3.15 against a directory, before publishing; `--peer` provisions a plugin the subject calls |
| `af plugin pin NAME [--version V]` | writes `[plugins.NAME]` with every published target's digest into `.af/af.lock` (P5) |

A release is `af-<name>-<version>-<target>.tar.gz` per target, beside `SHA256SUMS` and,
when the author signs, `SHA256SUMS.minisig`. Install verifies the archive against `SHA256SUMS`,
and against the signature when a key is known: `--key`, or the key the receipt recorded at the
plugin's first install on this machine, which later installs must match. A plugin without a
signature installs with `verified_by = "sha256sums"` in its receipt and `af plugin list` says
so. Under a project pin the lock's digest is what the bytes must match (ADR-0045 §2), key or no
key. `af self`'s `ReleaseSource`, `install`, receipt and verification code is what runs here,
parameterized by the plugin's repository; `AF_SELF_OFFLINE` and `AF_RELEASE_SOURCE` apply.

Removal is complete by construction: nothing in the kernel references a plugin, no Store
depends on one (§3.1 clause 5), and `.af/plugins/<name>/` is the project's to delete. An experiment ends
with `af plugin remove NAME --purge` and one commit.

### 3.6 Reading the kernel

A plugin reads what the CLI already prints: fifty-odd `af/*@N` documents today, among them
`af/task-inspection@11`, `af/task-list@2`, `af/review-outcome@3`, `af/review-report@4`,
`af/review-campaigns@1`, `af/provider-status@2`, `af/storage@1` and `af/error@1`, each with a
schema under `schemas/` and a parity test. Those are projections, and a plugin uses a projection
wherever one answers its question, so that Ledger, acceptance and accounting rules stay in one
place.

What no projection covers is reachable through one read-only namespace, `af store`, which
interprets nothing:

| Command | Prints | `--json` type |
|---|---|---|
| `af store list` | every Store this machine knows: the default Task and review roots, every Campaign, every registry entry; kind, path, bytes | `af/store-list@1` |
| `af store runs --state DIR` | the run IDs in a Store with event counts, first and last sequence, and whether the log holds a type this release does not know | `af/store-runs@1` |
| `af store events --state DIR --run ID [--from SEQ] [--type T]` | the event rows in sequence order, one line each with every persisted column of the `events` table: `run_id`, `sequence`, `event_id`, `type` (the raw string), `occurred_at`, `node_id`, `attempt_id`, `causation_id`, `correlation_id` (each `null` where the column is), `artifact_refs` and `payload` (the raw JSON values), plus the derived `known` (whether this release's `EventType` names the type); nothing is decoded into a `RunEvent` | `af/store-event@1`, JSONL |
| `af store attempts --state DIR --run ID` | the `attempt_wall` sidecar rows: Attempt, node, round, epoch, start, elapsed, usage | `af/store-attempts@1` |
| `af store artifact --state DIR ID [--payload]` | one artifact by ID, its envelope and payload verified on read, as the CAS reads it | the artifact's own type |
| `af store artifact --state DIR ID --raw` | the digest-verified bytes of one content object that has no envelope (a rendered Worker request, a captured stdout or stderr, anything stored with `Cas::put`), streamed to stdout in bounded buffers, exactly as `Cas::get` verifies them; a corrupted object is refused, and `--raw` conflicts with `--payload` | bytes |

These five commands are the whole generic read API, and they are the reason no later feature
needs a `--json` document added for its own sake: a plugin that needs a fact the projections do
not carry reads the rows and derives it. Printing rows is inspection, not replay: no payload
is decoded, no projection is built, and `RunEvent@1` validation is untouched. ADR-0145 refuses
to *read* another release's state; printing a row's raw bytes with `known = false` is a
narrowing of that refusal the P0 ADR states explicitly as its amendment of ADR-0145, and it is
the only one: replay, projection and every command other than `af store` refuse as today.

**Kernel exports.** Some kernel output is too large or too frequent for a document per call:
the telemetry note's segments and blobs under `$XDG_STATE_HOME/af/telemetry/v1/` are the
first. An export is a directory the kernel writes in a versioned, self-describing format
(`af/TelemetrySegment@1`, `af/TelemetrySpan@1`, `af/TelemetryBlob@1` and the other records that
note versions), documented as a contract, listed by root and format in the surface table, and
never consulted by the kernel for an execution, replay, acceptance, admission or Worker-input
decision (the telemetry note's own rule). The kernel reads it for two maintenance purposes and
no other: the bounded writer recovery below, which inspects an abandoned segment to find its
valid prefix, and the housekeeping of ADR-0144, which reads the structural metadata the
telemetry note enumerates (sizes and times, lock state, `segment` headers, `seal` records, the
`.refs` manifest of a sealed segment and the `blob` references of a live one) to count and to
protect what it keeps, and never a span, an event, a blob or an analytic. A plugin reads
an export directly and strictly read-only: it never seals, recovers, compresses, renames or
indexes inside the export (a sealed `.jsonl.gz` segment is immutable, a live `.jsonl` segment is
the writer's, a `.corrupt` segment is excluded), and it validates every record it reads by its
`kind@version`. Whatever an export's format needs written after a crash is the kernel's: the
telemetry substrate recovers abandoned segments itself, at the start of the next kernel
command that opens the export for writing, bounded (at most eight segments and two seconds per
start) and under the writer's lock; the reader's rebuildable index lives in the plugin's own
cache directory, never in the export. The conformance plugin reads a fixture export, and a
crash-recovery fixture proves the reader wrote nothing there. The
SQLite Store, the sidecar and the CAS are not exports and never become one.

### 3.7 Writing into the kernel

**Task files.** A plugin starts work exactly as a person does: it writes an `af.task-file/2`
outside the checkout and runs `"$AF_BIN" task start --file …`. The file is captured into the
Store at plan time and is disposable afterwards (ADR-0115). Nothing in the Task file can widen
authority: policy, packages and checks come from the committed `.af/` (ADR-0046, ADR-0052).

**Bound inputs.** A Task file may bind any declared root input port to a recorded Task's output
or, for three ports, to an exact artifact (ADR-0117, ADR-0134). This note adds the one form a
plugin needs, a file:

```json
"inputs": {
  "project": { "file": "/path/outside/the/checkout/brief.json", "type": "project/Brief@1" }
}
```

The form changes the persisted Task-file shape, so the Task file becomes `af.task-file/2`;
`/1` is refused by name on read (ADR-0145), and the schema, the `task-input-bindings` document
and every fixture move together in P3.

The `file` form binds only a port whose declared `artifact_type` lies outside the kernel's
namespaces (`af/`, `review.kernel/`): a plugin- or project-owned type such as
`project/Brief@1`. A port declared with a kernel type (a Measurement, a Comparison, a
GateDecision, a Finding, a Snapshot) is refused with the type named, whatever the payload, so
nothing a plugin writes can be presented to a Worker as something the kernel observed
(ADR-0132); Measurements and comparisons stay bindable only as recorded outputs (ADR-0134).
`source`, `history`, `sources`, `requirements`, `base` and `continuation` are refused by name.

A v1 file binding is exactly one artifact: cardinality `one`, admitted only on a root port
whose declared cardinality is `one` and whose `affinity` is `{ kind = "unbound" }`
(`PortAffinityV1`). A `many` port, a `same_as` or `derived_from` port, and a port already
bound by another form are refused by name before capture (ADR-0134 forbids implicit
cardinality conversion, and a file carries no Snapshot to be the same as or derived from). The
admission is decided where ADR-0134 decides port shape, before selection and before capture,
because selection consumes the revision the inputs construct: a Task naming its pipeline must
find the port declared `one` and unbound there; a Task naming none consults every captured
Pipeline accepting its kind, as `DeclaredPorts` does for type and cardinality today, and
`DeclaredPorts` gains the affinity, so that a port declared with one type and cardinality but
differing affinities across those Pipelines refuses the binding with the request to name a
pipeline, rather than selecting the unbound one or deferring to compilation; the refusal
precedes any read of the file and any publication. The
file's top level must be a canonical JSON object, because the artifact envelope admits nothing
else (`ArtifactEnvelope::validate`); an array or a scalar at the top level is refused before
publication, and arrays live inside the object. A list-of-files form is deferred (§6).

The limits are the ones the kernel already enforces, and never by truncation: the serialized
envelope must fit the CAS bound (8 MiB, `MAX_ARTIFACT_ENVELOPE_BYTES`); every file-bound
artifact a Worker consumes must itself fit 1 MiB serialized, envelope included, because the
renderer reads each wired artifact through `Cas::get_bounded(id, MAX_WORKER_BYTES)` before it
renders (`worker_value`, which then drops the producer and content-ID metadata, so an envelope
can exceed the bound while its rendered value fits), and planning makes that check before
publication rather than leaving the renderer to fail it; and the complete rendered request of
every Worker that wires the port, this input included, must fit the Worker request bound
(1 MiB, `MAX_WORKER_BYTES`). Planning checks what it can see, the captured file
with its envelope and the statically known components of each wiring Worker's request, and
refuses with the bound named before anything is published. The complete request can only be
checked when its other inputs exist, an earlier Worker's output or retry feedback among them,
so every complete request is checked before dispatch by the shared renderer
(`WorkerContract::prepare`), exactly as today, and an Attempt whose request the file pushes
over the bound is refused there with the bound and the port named.

The path is absolute, and the read is the one the Task-file reader already performs
(`task_execution::input_file::read`): a non-blocking open, a check that the opened object is a
regular file (a FIFO, or a symlink to one, is refused before it can block planning), and a
bounded read with an explicit raw ceiling of 8 MiB, before any parsing. At plan time the
Task-file adapter reads the file that way, requires canonical JSON, requires `type` to equal
the port's declared `artifact_type`, publishes it
into the Task Store's CAS with the producer `kernel_operation` and the
operation `task_file_input`, pins its artifact ID in the compiled plan, and lists it in the
Attempt's stored context manifest with its bytes and estimated tokens the way every port is
listed, plus `provenance = "external"`: that member is new, so the stored context takes its
next version (`ContextEntry`, ADR-0145). The Worker's request is unchanged: a Worker sees the
port's payload as it sees every port, and what the package's instructions say about the port
is the package author's (value 2). Resume, retry and replay never read the file again
(ADR-0117). The port must be declared by the
selected Pipeline and admitted by the Worker package's `[signature.contract.inputs]` and
`input.schema.json`, so a reviewed, locked pipeline decides which Workers see a plugin's data
and a Worker that does not wire the port sees nothing (value 2). A file-bound artifact can cover
no obligation (§3.1 clause 4). The plan preview and `af task explain` print the port, the type,
the bytes, the provenance and the file's digest.

A Worker package carries one `input.schema.json` over its complete input map, with whatever
cross-port constraints its author wrote, and nothing here extracts a sub-schema from it. The
file's payload is validated as every other input is: when every input of a consuming Worker is
known at plan time, planning validates that Worker's complete map against its captured schema
and refuses with the Worker and the error named; when an input of that Worker is produced
later (an earlier Worker's output, retry feedback), validation of that map happens where it
happens today, in `WorkerContract::prepare` before dispatch, and refuses the Attempt there. A
port several Workers wire is validated once per Worker.

This is the amendment the Project note's Brief (`af/ProjectBrief@1` on a `project` port)
needs to become a plugin's output, and it is the whole of what a plugin can put in front of a
Worker.

### 3.8 Hooks

| Hook | After | The document carries |
|---|---|---|
| `task.run.ended` | `af task run`, `af task start --execute` or `af self optimize --execute` returned, whatever the Task's state now is | `exit_code`, `target` |
| `task.delivered` | `af task deliver` created the worktree | `exit_code`, `target` (a `task`), `worktree`, `branch` |
| `review.run.ended` | `af review run` (or bare `af review`) returned | `exit_code`, `target` |
| `onboard.applied` | `af onboard --apply` wrote `.af/` | `repo` |
| `storage.sweep` | the sweep over the budget, after the kernel evicted what it owns | `target_bytes` |

`target` is a tagged value naming what the invocation recorded: `{ "kind": "task", "store",
"task_id", "rounds_closed" }` for a Task (a review started from a Task file with `review run
--file` is a Task, so its hook carries a `task` target, and a Task that runs no review pipeline
carries an empty list); `{ "kind": "campaign", "store", "campaign", "run_id", "rounds_closed" }`
for a Campaign; and `{ "kind": "none", "reason" }` when the invocation
failed before it recorded anything, a Task file that would not open or parse, a Task ID no
Store holds, a Store that would not open. The `none` variant is delivered, not suppressed, so a
queue plugin learns that its start failed; it carries the failure's reason text and nothing
else. A hook names an invocation that returned, not a state the Task or Round reached; the
plugin reads the state with `"$AF_BIN" task show ID --state DIR --json` for a Task and
`"$AF_BIN" review report --campaign NAME --state DIR --format json` for a Campaign.
`rounds_closed` is `[ { "round": N, "sequence": S }, … ]`: one entry per Round-closure event
this invocation appended to the target's Store, identified by the Round number and the Store
sequence of that event, ordered by sequence, and nothing appended by an earlier invocation,
whatever this one's exit code: an ordinary Campaign invocation closes at most one Round
(`--heavy` included, which runs one Round per invocation), a review pipeline declared in a Task
file may close several, a run that failed after it published a conclusion lists that Round,
and a resumed run never lists a Round twice. An interrupted command runs no hook: the kernel
exits as ADR-0129 requires, and the plugin sees the run next time through `af store runs`. No
command hands its document to the hook site.

A hook runs only what a pane may run. While `AF_PLUGIN_BACKGROUND` is set, the kernel runs
only the commands of its declared read-only table (§3.12) and the dispatch of another plugin
(`af <plugin>`, which inherits the marker and is bound by the same rule, and which under the
marker `exec`s the selected plugin in the inherited process group after validation and the
lock, never in a supervised group of its own: only the outer hook or pane owns a group,
`review-process` kills exactly that one group, and so killing it kills every nested plugin
and descendant with it), and refuses every
other command with the reason before any Store is opened for writing, any source is captured
or any subprocess is created. That covers what starts a Worker (`runs_task_work`'s list: `task
run`, `task start --execute`, `review run`, bare `af review` and `provider doctor`, whose
admission Attempts are paid Workers too, ADR-0129; `self optimize --execute`), what creates or
plans a Task (`task start` in any form, `task plan`, `review plan --file`, `self optimize`),
and the routes that do neither yet write or supervise: `task refresh`, which opens the Store
writable, captures fresh issue data and persists a replacement plan without passing through
`task_execution::start`; `catalog sync`, whose transport runs Git in a group of its own;
`onboard`, `plugin install|remove|pin` and `storage prune --apply`. An allowlist is one
declaration, so the next command the kernel grows is refused in the background until it is
admitted deliberately; `task_execution::start` and `refresh` refuse under the marker as well,
so no route bypasses the classification. The reason is one: a hook is killed at its wall with
its process group, while everything the kernel supervises through `review-process` lives in a
group of its own, Workers, the Git capture `start_kind` runs first and the catalog transport
among them; killing a nested `af` would leave them running without the kernel that settles or
reaps them. Admitting more from a background invocation waits for a nested cancellation that
can stop and reap those children. `AF_PLUGIN_BACKGROUND` is inherited by
every process a hook starts, a plugin called through `$AF_BIN` included, and the kernel never
clears it, so a hook cannot launder Task work through a second plugin. A plugin that starts Tasks does so from a
foreground command of its own that owns, waits for and cancels them (§3.17, `af backlog`).

The document on stdin is `af/plugin-hook@1`: `schema`, `hook`, `af_version`, `repo`, `argv`,
`exit_code`, `occurred_unix_ms`, `target`, and the members the table names. The environment is §3.3's with
`AF_PLUGIN_BACKGROUND=hook:<name>`.

A hook's context is the command's, not the shell's: the repository the command resolved
(`--repo`, or the working directory's toplevel, by the plugin-context resolver of §3.3; a
command run from repository A with `--repo /B` is B's) decides `AF_REPO`, the
effective configuration (`enabled`, `[plugin.<name>]`), the pin of §3.13 and therefore the
bundle that runs; the kernel resolves all of that once, before it spawns anything, and
`hook-run` receives the result as arguments, never re-deriving it from a working directory.
A background invocation, a hook or a pane load, uses only a bundle that is already installed
and verified: it never installs on demand, because the release sources have no deadline of
their own (`GhSource`, `require_gh`) and nothing may wait in front of `hook_wall_ms`. A pinned
version that is not installed is the hook's failure line, or the pane's error row, naming
`af plugin install`; on-demand installation belongs to foreground plugin commands only.

Hooks run after the command's result is written and flushed, after the post-command Storage
Budget sweep (`storage.sweep` alone runs inside that sweep, the one exception, §3.9), and
before the update check's detached child, in plugin-name order. In both
modes the plugin is invoked as `<bin> hook <name>`, receives the document on stdin, which is
then closed, and is killed with its process group at `hook_wall_ms`. `detached` (the default)
spawns `$AF_BIN plugin hook-run <name> --wall-ms N --repo PATH --bundle DIR` as a child in its
own process group with
stdout and stderr null and stdin a pipe, into which the kernel writes the one document (at most
64 KiB) and closes it, never blocking on a supervisor that does not read (a write that fails or
stalls is the hook's failure line, and the child is killed); that supervisor reads the document,
spawns the plugin, writes the same bytes to its stdin, closes it, waits up to the wall, kills
and reaps the group, and exits. It is a bounded child that outlives the command only as long as
the plugin does, not a daemon. `sync` does the same in the command's own process with the
plugin's stderr inherited and stdout null, and the command returns only when the plugin has.
Neither mode changes stdout, the exit code or any `--json` document. A hook that cannot start, times out or exits non-zero is
one line: on stderr for `sync` (`af: plugin backlog: task.run.ended hook exited 3`), and for
`detached` one line appended to `$XDG_STATE_HOME/af/plugins/hooks.log` (kernel-owned, 1 MiB,
rotated once), which `af plugin list` summarises as each plugin's last hook failure. A plugin
with `enabled = false` in the effective configuration runs no hook and no command.

Hooks are notifications, not an event bus: there is no subscription, no replay and no
persistence in the kernel. A plugin that wants to see what it missed reads `af store runs`.

### 3.9 State, cache and the budget

A plugin declaring `storage = "state"` owns the payload directories
`$XDG_STATE_HOME/af/plugins/<name>/data/` and `$XDG_CACHE_HOME/af/plugins/<name>/data/`, each
beneath a control directory, `$XDG_STATE_HOME/af/plugins/<name>/` and
`$XDG_CACHE_HOME/af/plugins/<name>/`, that the kernel keeps for its lock (below); `"cache"`
owns the second only; `"none"` neither, and a
plugin writing there anyway is a bug `af plugin check` catches with a fixture run. The kernel
counts three storage kinds per plugin, `plugin-install:<name>`, `plugin-state:<name>` and
`plugin-cache:<name>`, in `af storage` and the budget (ADR-0144). The kinds are new values in
closed enums (`schemas/storage-v1.json`, `schemas/storage-prune-v1.json`), so the documents
that carry them are versioned with them (ADR-0145, clause 6 of §3.1): P1 introduces
`af/storage@2` and `af/storage-prune@2` beside the sweep record's next version, producers,
readers, schemas and parity fixtures together, and the `@1` documents stay as they are. Cache
payloads (`data/`) are entries the kernel evicts whole, least recently used first, never
within an hour of use. State is never
evicted by the kernel. The `storage.sweep` hook is the one timing exception to §3.8: it runs
inside the sweep, after the kernel's own eviction and before the sweep finalizes its report and
the `storage_sweep` observation, so that `total_after` and the stop reason describe the state
after the plugins ran. It runs only from the two sweeps a command finishes with, the
post-command sweep, whose command result is already written and stays unchanged, and `af
storage prune --apply`, where `prune` runs `sweep` before it prints `af/storage-prune@2`, so
the document is emitted afterwards and carries what the plugins freed; and only when the
budget is still exceeded after the kernel's own eviction. It runs synchronously, each enabled
plugin in name order with the remaining `target_bytes` remeasured after the previous plugin,
under its own bound of 60 seconds whatever the manifest's `hook_wall_ms` says, and the sweep
reports what it could not free. The
sweeps the kernel runs during execution, at the free-disk floor (`ensure_free_disk`) and
before a new warm key (`before_new_warm_key`), never invoke a plugin: nothing a plugin does can
sit inside admission or an Attempt. Whatever remains is the operator's with
`af plugin remove --purge`. Installed versions other than
`current` and the pinned ones are evicted like other installed versions.

Nothing in use is evicted (ADR-0144). Every plugin process the kernel starts, a command, a hook
supervisor, a pane load or a runner adapter, holds shared locks on
`.lock` files in the control directories beside what they protect, one per root the plugin
has, `$XDG_DATA_HOME/af/plugins/<name>/.lock` beside the versions,
`$XDG_STATE_HOME/af/plugins/<name>/.lock` and `$XDG_CACHE_HOME/af/plugins/<name>/.lock` beside
their `data/` payloads, for its whole lifetime, as `hold_running` anchors a Task's protection
to the Store directory it protects: every process that sees the same
directory sees the same lock object, whatever else its environment says, so two invocations
with different data roots and one cache root contend on that cache's lock, and no runtime
directory is involved. The control directories and their lock files are created at install, at `--link` and at
the first start of a `PATH` plugin, and no sweep removes them: the sweep evicts the `data/`
payload beneath, never the control directory, so an open lock and its path never part. A
start takes the shared lock, checks with `fstat` that the file it holds is still linked at
its path (a link count above zero), retrying once and refusing otherwise, and creates `data/`
when it is absent; a lock that cannot be taken refuses the operation with the reason rather
than running unlocked; the hook supervisor holds them while the plugin's process group lives.
The sweep takes the exclusive lock of the root it is about to evict, without blocking, skips
it and says so when it cannot, re-reads the payload's identity (device and inode) and
protection under that lock before removing it, as `campaign_in_use` guards a Campaign today,
and releases it before any start proceeds to recreate the payload. `remove --purge` takes
every exclusive lock, removes the payloads and the versions, and removes the control
directories last while still holding the descriptors, so a start racing it finds an unlinked
lock and refuses. Every start records the plugin's last use.

A plugin never reads or writes another plugin's directories; composition between plugins is
through their command lines, like everything else.

### 3.10 Configuration

`[plugin.<name>]` is one table per plugin in the ladder, deep-merged across layers, reported
with origins, and otherwise opaque to the kernel. One key is reserved: `enabled` (default
`true`), and it is machine-only for every plugin whatever the manifest says: a directory,
project or local layer carrying it is reported and ignored, so a repository can configure a
plugin the machine enabled and can never enable one the machine disabled. A manifest with
`machine_only = true` makes the whole table machine-only, joining `[self]` and `[storage]` in
the `MACHINE_ONLY` list for that name. A kernel table is never a plugin table: the telemetry
note's `[telemetry]`, which configures capture the kernel performs, stays a built-in, typed,
machine-only kernel table beside `[self]` and `[storage]`, known before any repository layer
is merged whether or not a reader plugin is installed, so that removing, disabling or replacing
`af-telemetry` can never let a repository set a capture level; the reader's own settings live
under `[plugin.telemetry]`.

The environment form is `AF_PLUGIN__<NAME>__<KEY>`, where `<NAME>` is the plugin name in upper
case with each `-` written as `_`: the name grammar of §3.3 admits no `_`, no doubled and no
trailing hyphen, so the encoded name never contains `__` and the first `__` after `AF_PLUGIN__`
separates the key, whatever the key starts with (`_enabled` is a key, as the existing parser
allows). An explicit plugin branch parses it ahead of the one-split `AF_<TABLE>__<KEY>` rule,
which would otherwise read it as a table named `plugin_backlog`. Origins report the variable.

`review_config`'s `ProjectFile` is `deny_unknown_fields`, so `.af/af.toml` gains one optional
opaque field, `plugin`, and one optional table:

```toml
[plugins]
required = ["backlog", "analytics"]   # af plugin list and af onboard say what is missing
```

`required` is advice to a machine (a warning line, `af plugin doctor`), never an installer.

### 3.11 Authority in `.af/`

`LAYOUT` gains one directory entry: `plugins/`, writer human, versioned, "plugin declarations
and the packages they ship, one directory per plugin". A plugin keeps whatever it declares for
a project under `.af/plugins/<name>/`, and writes there only the way `af onboard` writes:
absent-only, previewed, never credentials, never state (ADR-0032). A plugin that writes there
declares `init = true` and implements one verb with `onboard`'s shape: `<bin> init --repo PATH`
previews, writes nothing, and prints `af/plugin-init@1`, a closed document: `schema`,
`plugin`, `repo`, and `files`, an array of `{ path, mode, content }` sorted by the bytes of
`path`, where `path` is a canonical relative path beneath `.af/plugins/<name>/` (`/`
separators, UTF-8 in NFC, no empty, `.` or `..` component), `mode` is `"x"` or `"-"` as the
content manifest of §3.15 writes it, and `content` is the file's bytes in base64; at most 64
files, 256 KiB per file decoded, 1 MiB for the document, and a duplicate or invalid path
refuses the preview before anything is applied. `<bin> init --repo PATH --apply` creates
exactly those files, absent-only, refusing when any exists, within 60 seconds. The kernel
cannot enforce that on a plugin; `af plugin check` runs the preview in a fixture repository
and fails on any write, then the apply with only the absent destination writable, and
compares the complete manifest of `.af/plugins/<name>/` afterwards, paths, bytes and modes,
with the preview, failing on a missing, extra or changed file, on any other write under `.af/`
and on any write outside it.

Worker, Pipeline and kind packages a plugin ships are pinned by a project exactly as the
kernel's own are: a `path` under `.af/plugins/<name>/packages/` in `task-catalog.toml` or a
sync from the plugin's repository into `vendor/`, digests in the lock, replacement rules
unchanged (ADR-0052, ADR-0053). The kernel knows nothing about who wrote a package.

### 3.12 A pane in the browser

A plugin with `pane = true` answers `af <name> pane [--item ID] --json` with `af/plugin-pane@1`:
`title`, `items` (the bar entries under the plugin's folder: `id`, `label`, nested `children`),
`rows` (`text`, optional `tone` from the design system's chip set: `run`, `pass`, `fail`),
`legend`, `status` and `actions`, a map of at most 16 entries from a single printable ASCII
key to an argument array without the leading `af` (`{ "s": ["backlog", "start", "--item",
"<id>"] }`), each array at most 32 words of at most 256 bytes. The permitted keys exclude every
key the browser binds globally (`q`, `R`, `Enter`, `o` and the sequence prefixes `g`, `z`,
`]`, `[`, `Z`, read from the keymap rather than listed by hand) and the bar's `r`, `D` and
`d`, so a plugin's key never shadows navigation; a document naming one is malformed and is one
error row. A plugin's keys act only while its pane holds focus and never from the bar, whose
keys stay the browser's; an action's words are validated through the CLI parser, so a
`--confirm` the parser accepts for that command, in either spelling, is caught here and not by
string search, and they are shell-quoted into the prefill. The document is bounded: at
most 1 MiB, 1,024 items, a depth of eight, 4,096 rows, and a load deadline of ten seconds. The
bar gets one folder `plugins/` with a child per such plugin; one generic renderer paints every
plugin pane, loads it on a thread like the other panes, reloads on `R`, and shows nothing a
document does not carry; a superseded load is cancelled and its process reaped on reload, on
closing the pane and on quit, and a malformed, oversized or timed-out document is one error
row. A pane load runs with `AF_PLUGIN_BACKGROUND=pane`, and under that marker, as under a hook's
(§3.8), the kernel runs only the commands of its declared read-only table (`READ_ONLY_COMMANDS` in `main.rs`, beside
`runs_task_work`: `config show`, `task list|show|report|explain`, `review
report|ledger|show|campaigns`, `store *`, `storage` without `prune --apply`, `plugin
list|show`, `help` when it resolves to a kernel topic or a kernel command, `--version`), each
entry a predicate over the parsed command and its flags, not a name: `help <plugin>` runs the
plugin's own executable (§3.3) and is refused before anything spawns; `review plan` is outside the table because it starts a review Task through
`start_review` with `--file` and probes Providers without it, `storage prune --apply` is
outside because it writes, and `provider status` and `provider models` are outside because
they start Provider processes in groups of their own (`run_probe_before`, `claude_catalog`)
whose deadlines live in the `af` that would be killed with the pane or hook. In v1 every
Provider-probing operation is refused at its shared entry point while `AF_PLUGIN_BACKGROUND` is
set; it is admitted once a nested cancellation can stop and reap those groups (ADR-0026). Every entry opens a Store read-only or
none at all; where a reader opens one writable today (`review show` through
`open_campaign_store`), P1 moves it to `open_campaign_store_read_only` and `Cas::open_existing`
before the table is declared; every other command, `task plan` and `task start --file` without `--execute`
among them, is refused before any Store is opened for writing. So a load that is killed on
timeout, reload or close can orphan no Worker, and loading a pane writes no Store (ADR-0119).
The same table bounds a hook (§3.8), is the conformance broker's reader allowlist (§3.15)
and is the reader surface `af help plugins` prints (§3.16): one declaration, four uses. An action prefills the `:` line and never submits it: the operator reads the line and
presses Enter or `Esc`, exactly as the Tasks pane's `r` and `D` work (ADR-0123, Prefills). A
document whose action carries `--confirm` is refused as malformed, so a plugin cannot reduce
delivery's typed Task-ID confirmation to a keypress. The plugin's pane is a projection of its
own `--json` commands, by the TUI's own rule. No plugin can add a key the browser does not have
or paint outside its rows.

### 3.13 Pins

`.af/af.lock` gains `[plugins.<name>]` with `version` and `[plugins.<name>.digests]` per target,
written by `af plugin pin` and by `af onboard --refresh-lock` for every plugin the project
requires. Inside such a project `af <name>` runs the pinned version, installing it on demand
(for a foreground command only, §3.8) from the source this machine's `origin.toml` for that
plugin names, when the archive matches the lock's digest for this target. A plugin never installed on this machine has no origin, and
the pin is then refused with `af plugin install SOURCE` as the next action: a repository pin
locates nothing and authorizes no source, exactly as `af self` fetches only from its configured
release source. The rest follows the rules a pin of af itself follows: a pin without a digest
for this target is never installed on demand,
`AF_SELF_OFFLINE=1` or `[self] install_pins = false` refuses with the command to run, an
installed copy must carry the digest in its receipt, and a `dev` or `PATH` plugin never
satisfies a pin. `AF_PLUGIN_VERSION_<NAME>` overrides for one invocation and precedes the pin, as `AF_VERSION`
precedes af's own (§3.3, one resolver). The pinned version is what a plugin's hooks and panes
run as well. A Task's replay does not depend on any of this (clause 5);
pins exist so that a team runs the same plugin, which is what sharability asks.

### 3.14 The runner seam

The second implementation a Provider kind still lacks is an external one. A runner adapter is a
plugin whose manifest declares `runner = "gemini"` (one kind per plugin) and whose executable
speaks `af.runner/1` through four verbs, so the Provider registry's `kind` may name it. A kind
resolves to exactly one installed plugin: the native kinds (`claude`, `codex`) are reserved, a
second plugin declaring a kind another installed plugin declares is refused at install, and a
`PATH` plugin never supplies a runner; a `--link` runner may, verified by the same content
digest, so a development edit refuses the next Attempt until a plan is approved under the new
bytes. The runner is captured authority, not a lookup: at plan
time the kernel records the selected runner's plugin name, version, protocol, target and
content digest (§3.15, the digest of the bytes that run) in the Task's execution binding beside the Provider binding (ADR-0046), recomputes the content digest of the selected version directory (§3.15) and compares it with
that record before the binding is written and before every private invocation, so an
executable or support file edited in place, a changed mode or a symlink that appeared after
install refuses the Attempt whatever `current` and `receipt.toml` say; runs the verified bundle
while holding the plugin's shared lock (§3.9), which `af plugin remove` and a reinstall of the
same version take exclusively before touching the directory; and refuses substitution on
resume; a plan
approved under one runner never runs under another, and replay validates the record without
the executable. It is
the exception §3.1 clause 2 names: the kernel hands it one Attempt's materialized sandbox and
the exact input bytes it composed, under the Attempt's access class, and nothing else; it never
sees a Store, another sandbox or another Attempt. P0's ADR states the two kinds of plugin and
this bound in the same clause.

The four verbs are not public commands. The command seam of §3.3 refuses `af <name>
identity|status|models|invoke` for a plugin whose manifest declares `runner`, with the reason,
and forwards every other word as usual; the kernel speaks the verbs only over its private
runner transport, pipes it owns for stdin and stdout, never the inherited terminal, and sets
`AF_RUNNER_AUTH_DIR` on that transport alone. What the public sees of a runner is what it sees
of a native adapter: `af provider status` and `af provider models`, the kernel's own sanitized
projections (ADR-0112), which consume `identity` privately as `observe_identity` does today.

Every verb runs under one Provider context: the kernel passes `AF_RUNNER_AUTH_DIR`, the
machine-local authentication directory the Provider registry binds for that Provider, exactly
as the native adapters receive the selected directory (`providers/task.rs`,
`TaskProviderIdentity.auth_dir`), so two Providers of one runner kind never fall through to
one ambient account (ADR-0042). The kernel derives the Provider's canonical principal and
authentication method from `identity`, records only the derived principal, and rechecks
`identity` before it delivers a private input, refusing the Attempt when the account changed
(`CurrentTaskProviderAdapter` does the same for the native adapters today).

| Verb | Request | Answer |
|---|---|---|
| `identity --json` | — | `af/RunnerIdentity@1`: the account's stable identifier and authentication method under `AF_RUNNER_AUTH_DIR`; private: reachable only over the kernel's runner transport, never through `af <name>`, never printed, never stored |
| `status --json` | — | authenticated or not, account label, the one command a human runs at a terminal to log in (ADR-0112: the plugin never starts a login) |
| `models --json` | — | the model catalog with effort levels, for ADR-0148's validation |
| `invoke` | `af/RunnerRequest@1` on stdin: the exact input bytes the kernel composed, the sandbox directory, the access class (`ReadOnly`, `ExecuteChecks`, `WriteSource`, `WriteSourceWithShell`), model, effort, deadline, the sandbox-local environment | framed stdout: `af/RunnerUsage@1` frames (cumulative, `TaskUsageObservationV1`'s shape: the reported counters in `af/TaskTokenUsage@3`'s shape and `charge_complete`), then one terminal `af/RunnerResult@1` with the output bytes, the model that answered and a failure class from the kernel's own list |

`invoke`'s stdout is framed, one JSON document per line: zero or more `af/RunnerUsage@1`
frames, each a cumulative usage observation in `TaskUsageObservationV1`'s shape, then one
terminal `af/RunnerResult@1`. The kernel validates each frame as it arrives and folds it into one
accumulator with `merge_previous` semantics (`review-core/src/task/usage.rs`): a counter never
decreases below a floor an earlier frame set, and `charge_complete = false` once seen stays
false, so a later complete frame cannot erase earlier uncertainty and a decreasing cumulative
counter cannot lower the charge; a run that is cancelled, times out or ends with a missing or
malformed terminal frame settles from that accumulator, which is what the adapter returns as
its one `ModelWorkerReturn`.

The transport's bounds are frozen here, enforced while reading rather than after collection
(`drain_async` grows without a limit today, `run_supervised_duplex_inner` sends stderr
through it and collects the reader's result only after the leader exits, and `validate_reply`
checks `MAX_WORKER_BYTES` only afterwards), and they are enforced where ADR-0026 puts output
collection, in `review-process`: P1 extends the leaf crate, for the conformance harness of
§3.15, with a retention limit per stream, an overflow policy per stream (one policy in both
uses: the first excess byte on any stream cancels) and a cancellation the reader raises the
moment a frame breaks the protocol, while group killing, draining and reaping stay there as
they are, and P7 reuses it; the adapter interprets frames and nothing more: a frame is at
most 2 MiB, the terminal frame's output bytes at most `MAX_WORKER_BYTES` (1 MiB) as any Worker
reply, aggregate stdout at most 16 MiB, stderr at most 1 MiB, a hard limit like the others,
and `identity`, `status` and `models` answer within 256 KiB and 15 seconds each.
Crossing any bound, an unterminated frame included, kills and reaps the process group, refuses
the Attempt's business output, and settles the usage floors observed so far. A frame that
is present as `af/RunnerUsage@1` but fails validation is malformed billing, not absence, and is treated as
`add_usage` treats a malformed native report (ADR-0088): the counters it carries unambiguously
are retained, the observation is recorded with `charge_complete = false`, cumulative floors are
kept, and the Attempt's business output is refused. Only an invocation that wrote no usage
frame at all is the wholly absent case, settled at zero and marked unknown (ADR-0143). A
malformed terminal `af/RunnerResult@1` frame is malformed output, not malformed billing
(ADR-0088 leaves billing completeness untouched by malformed non-billing metadata, as the
Codex adapter's `add_usage` does): the business output is refused and the usage settles from
the accumulator as it stands, counters and completeness unchanged, so a complete usage frame
followed by an invalid result keeps its known charge, and an invalid result with no usage
frame before it settles at ADR-0143's zero marked unknown.

The kernel keeps what it keeps for the native adapters: it composes the input, materializes the
sandbox, reserves the budget, supervises the process through `review-process` with the deadline
and cancellation, redacts and stores stdout and stderr as raw artifacts, validates the usage,
settles the charge and admits the Provider. An external runner is always `trusted_unsafe`
(ADR-0042) and can never authorize `auto_apply`. The Claude and Codex adapters stay in-process
and unchanged; the first external adapter is the proof the second-implementation rule asks for.
The protocol's exact fields are decided in P7's own ADR, against the adapter trait as it stands
then (`crates/review-runner/src/task.rs`, `WorkerModelAdapter`).

### 3.15 Conformance

`fixtures/plugins/echo/` is a Python plugin that uses every seam: it prints its environment and
arguments as `echo/Invocation@1`, answers `pane --json`, records every hook document it receives
under its state directory, writes one file under its cache, and ships one command Worker package.
`crates/af/tests/it/plugins.rs` installs it with `--link`, runs it through `af echo`, through a
hook after a fixture Task, through the pane and through a pin, and asserts the invariants of
§3.1 against the Store; the exact environment delivered and the exact hook bytes received are
asserted there, through the fixture's own `echo/Invocation@1` output and state files, and only
there. `af plugin check DIR` checks any plugin against what the contracts make observable:
the manifest is valid; each declared verb is invocable and bounded (`--help` exits 0 within
100 ms; `hook <name>` with a sample document exits within its wall; `pane --json` answers
within the bounds of §3.12 when declared; `init` runs when declared); and the filesystem
boundary of the audit below. It cannot tell a hook that parsed its document from one that
ignored stdin and exited 0, and does not claim to; an acknowledgement protocol would be a
contract of its own and is not in v1.

A bundle has two digests. The archive digest is the SHA-256 of the release archive, what
`SHA256SUMS` lists and a pin names per target (§3.13), and an unpacked directory cannot
recover it. The content digest (`af.plugin-digest/1`) is execution authority (§3.14), so its
bytes are frozen: the manifest is a JSON array of entries `[path, mode, sha256]`, one per
regular file in the unpacked bundle, where `path` is the relative path with `/` separators,
UTF-8 in NFC, no `.` or `..` component, `mode` is `"x"` when the owner's executable bit is set
and `"-"` otherwise, and `sha256` is the lower-case hex SHA-256 of the file's bytes; entries
are sorted by the bytes of `path`; directories are not entries and an empty one is ignored;
the root `receipt.toml` is excluded, and since `bin` may never name it (§3.4) the executable
always has an entry; a symlink, FIFO, device or other non-regular entry is refused. The
array is serialized as canonical JSON (the kernel's own rules, no whitespace, sorted keys
being moot for arrays) and the digest is the SHA-256 of the bytes `af.plugin-digest/1\n`
followed by that serialization; fixed expected-digest fixtures pin path escaping, a Unicode
path, an executable-bit change, one changed byte in the executable, and two archives with
different bytes and the same files,
which share one content digest. Install records both in the
receipt; `af plugin check` prints the content digest and the target, and a conformance result
is bound to that pair; the runner binding of §3.14 names the content digest. The boundary check runs the bundle inside a Linux container, so it checks a bundle
the container can execute: a Linux target matching the container's architecture, or a bundle
whose `bin` is an interpreter script the image provides (the echo fixture is one). A native
Darwin bundle is reported `not_run` with the reason, `--require-boundary` then fails, and
`af plugin check DIR --target <triple>` checks that target's bundle and claims nothing for any
other; a native-target fixture pins the refusal.

The boundary check cannot be proven by file permissions: a plugin runs as the operator, and
whatever it cannot read, its legitimate `af` child cannot read either. So `af plugin check` runs
the plugin inside the kernel's container sandbox (`review-sandbox`'s probed container provider),
with no Store mounted at all and mounts that follow the seams: the plugin's declared state and
cache directories writable; the fixture kernel export (§3.6) read-only; the fixture repository
read-only, except that for the plugin's `init` the absent `.af/plugins/<name>/` destination
alone is writable (§3.11); `AF_BIN` inside the container is a broker shim that sends each
command line, framed, over a FIFO that exists only inside the container to a relay process in
the container, which forwards it on the container's own stdout and reads the answer on its
stdin: the harness drives the container runtime's attached stdin and stdout, supervised
through `review-process` with the stream limits P1 adds there (§3.14) and bounds of its own, a
broker frame at most 64 KiB, a broker reply at most the CAS bound of 8 MiB plus its header,
the container's aggregate stdout at most 64 MiB, its stderr at most 1 MiB, and the trace, which
`strace -o` writes to a pipe the relay forwards as a second framed stream on the container's
stdout, so that the harness counts its bytes before retaining them as on every other channel,
at most 256 MiB, with the size of the trace file the harness keeps as no more than a second
check; the first excess byte on any of them cancels the check, which confirms the container's removal
(§3.15 below) and fails with the channel named, and a truncated trace is never evidence. The
harness runs the real `af` against the fixture Store outside, and
answers with stdout and the exit code. A mounted FIFO would not do: on macOS the container is
a separate Linux virtual machine and a FIFO's endpoints do not cross that boundary. The
container is the provider's, as it is for an Attempt: P1 adds to `review-sandbox` a duplex
execution path beside the collecting one, keeping the provider's named-container ownership,
its bounded forced removal on timeout and cancellation (`remove_container`) and its cleanup
confirmation, so that stopping the attached client never leaves the daemon-owned container
writing its mounts; when removal is not confirmed the check is refused with the reason and the
fixture directories are preserved for inspection rather than removed under a live container. The harness lives only for the check, records every brokered call, and executes none without
checking it first against a typed allowlist: the kernel's declared read-only table (§3.12,
`task report` and `task explain` among its entries, `help` only for kernel topics and
commands) with its flags, each only against the fixture's own Store and repository, and
nothing that writes: the check exercises only the declared verbs (`--help`, `hook`, `pane`,
`init`), none of which may create, plan or run a Task (§3.8), so the broker admits no
`task start` in any form. The brokered `af` runs with the fixture's `HOME` and XDG tree, and a
selector that escapes it (`--repo`, `--state`) is rejected before execution. A nested plugin
call, `"$AF_BIN" <peer> ...` (§3.8, the composition §3.9 allows), never runs on the host:
`af plugin check DIR --peer PEER_DIR` provisions each named peer bundle, a Linux bundle like
the subject, inside the container, and when the shim's command names a provisioned peer the
harness answers with the peer's path and overlay and the shim execs it there, under the same
tracer, the same bounds and the inherited background marker, the peer's own `af` calls
returning through the broker; a nested call naming a peer that is not provisioned is reported
`not_run` with the reason, and `--require-boundary` then fails. Any other
command, `task start --file`, `help <plugin>` and `onboard --refresh-lock` among them, is
rejected before anything spawns and recorded as a boundary violation, since the plugin's
syscall trace cannot see what a host-side `af` reads or writes. Denial alone proves nothing, because a plugin can swallow the error and exit 0, so the
harness also audits: inside the container the plugin runs under a file-syscall tracer
(`strace -f -y -s 4096 -o <the relay's trace pipe> -e
trace=%file,fchdir,dup,dup2,dup3,fcntl,close,unshare,%process`, the
container is Linux on every host, with the ptrace capability the harness requests for this
check only), and the trace, not the plugin's output, is the record. The auditor resolves every
access to an absolute path before classifying it: a descriptor-relative call through the path
`-y` decodes for the descriptor, a relative path against the process's working directory,
which the auditor models as the kernel does, as filesystem contexts rather than per-PID
values: every task belongs to one context holding a directory, which starts as the
container's initial directory; `clone` and `clone3` with `CLONE_FS` (every thread, since
`pthread_create` passes it) join the child to the parent's context, any other `clone`, `fork`
or `vfork` gives the child a copy, `unshare` with `CLONE_FS` splits off a copy, and `chdir` and
`fchdir` update the context of the task that called them, which is every task sharing it; a
successful `rename`, `renameat` or `renameat2` whose source is a prefix of a context's
directory rewrites that prefix, since directory identity follows the rename as the kernel's
does; and `chroot`, `pivot_root`, `mount`, `umount2` or a flag strace leaves undecoded makes the
check `unresolved`. The result is normalized
lexically, which is sound because the bundle carries no symlink (§3.4), the mounts start
without one, and a successful `symlink`, `symlinkat`, `link` or `linkat` anywhere is an
immediate violation in the trace whether or not the link survives (a link made, used and
removed before exit would otherwise alias the cache to the export and read lexically as a
cache write), with a scan of the writable directories after the run as a second check. An access the auditor cannot resolve (an
undecoded descriptor, a truncated string, a PID it never saw created), a `strace:` diagnostic,
or a process the trace does not see exit makes the check `unresolved`, which fails like a
violation: the record is complete or the check claims nothing. Any path-naming syscall outside the plugin's bundle, its declared directories, the
mounted export (reads only), the fixture repository (reads, and `init`'s one destination), the
broker FIFO, the system paths the image provides and a private scratch directory is a
violation whatever its result, `ENOENT` and `EACCES` included. This harness policy is the
check's own and shares nothing with the Worker sandbox policy. Without a container provider,
or when the provider cannot grant the tracer or attach the relay's stdio, the boundary check is
reported `not_run` and
`af plugin check` claims no conformance; `--require-boundary` turns that into a failure. The
echo fixture's tests include the positives (a brokered `AF_BIN` read, an export read, an
`init` write under `.af/plugins/echo/`) and the negatives (a direct Store open, an export
write, a write under a neighbouring `.af/` path, and a denial caught and exited 0). A plugin template repository carries the
fixture's structure for a new plugin.

### 3.16 What stays in the kernel, and the whole surface on one page

The kernel is the code that holds authority, and the list is short enough to state. Identity and
capture (`review-source-git`); the Store, its CAS, log and Ledger (`review-store`); budgets and
fencing (`review-attempt`); the graph and scheduler (`review-graph`); the plan compiler, the
Task host and every operator that carries acceptance or lineage (`seal`, `accept`,
`review_accept`, `check`, `verify`, `measure`, `compare`, `call`, `select`, the document, report
and optimization operators) (`review-pipeline`); sandboxes (`review-sandbox`); process
supervision (`review-process`); the native model adapters until P7 and the Provider registry
and admission; self-management, dispatch and pins; the Storage Budget; the configuration
ladder; the `.af/` layout and `af onboard`; and the CLI with its documents. Instrumentation is
kernel too: the telemetry note's capture packages (the span substrate, process resources, the
stream observer) instrument kernel phases and can only live there.

Everything else is a plugin, including what the two notes in flight planned as kernel commands:

| Feature in flight | Kernel part | Plugin part |
|---|---|---|
| Telemetry | T1's substrate and segment writer with abandoned-segment recovery on the writer's side (§3.6), T2, T3's observer and taps, T5's probe and receiver, the `[telemetry]` table as a built-in machine-only kernel table (§3.10), the segment directory declared as a kernel export | `af-telemetry`: T4's analytics, exports and sinks, T6's pane, the rebuildable index under the plugin's cache, all reading the export through the seam of §3.6 and never writing it or the Store |
| Project | the Project ID derivation (`repository_id`), printed by the Task and review inspections | `af-project`: Workspaces and their registry, resolving a Workspace label into explicit `--state`, `--bindings` and file paths before it invokes the kernel (no kernel `--workspace`); Surveys; the Surveyor as a `task.run.ended` hook; Briefs rendered to a file and bound to the `project` port by whoever starts the Task; `activity` over `af store` |

The whole kernel surface toward plugins, which `af help plugins` renders:

| Direction | Contract | Shape |
|---|---|---|
| kernel to plugin | the invocation environment of §3.3 | protocol `1` |
| kernel to plugin | the hook document of §3.8 | `af/plugin-hook@1` |
| kernel declares | the read-only command table of §3.12: pane mode, hook mode, the conformance broker and the printed reader surface | one table in `main.rs` |
| kernel reads | the manifest of §3.4 | `af.plugin/1` |
| kernel keeps | the origin of §3.5, the receipts, `hooks.log` | `af.plugin-origin/1`, `receipt.toml`, one line per failure |
| plugin reads | every `--json` document the CLI prints | each its own `kind@version` |
| plugin reads | `af store list`, `runs`, `events`, `attempts`, `artifact` | `af/store-*@1` |
| plugin reads | the kernel exports of §3.6: `$XDG_STATE_HOME/af/telemetry/v1/` (`segments/`, `blobs/`) | the telemetry note's `af/Telemetry*@1` records; one row per export |
| plugin writes | a Task file | `af.task-file/2` |
| plugin writes | a file-bound root input | the `file` form of `inputs` |
| plugin answers | a pane | `af/plugin-pane@1` |
| plugin answers | an `init` preview | `af/plugin-init@1` |
| project declares | `[plugins] required`, `[plugin.<name>]`, `.af/plugins/<name>/`, `[plugins.<name>]` in the lock | TOML, as every declaration |

Call sites in existing files, the complete list: `cli.rs` (the `external_subcommand` variant,
the `plugin` and `store` namespaces); `main.rs` (one arm each for the two namespaces, one call
to `plugins::after_command`); `topics.rs` (one topic, rendered from the surface table);
`layout.rs` (one entry); `config.rs` (the `[plugin.*]` passthrough, manifest-declared
machine-only names); `project.rs` (one opaque field, one table); `storage.rs` (three kinds, `af/storage@2`, `af/storage-prune@2`);
`selfmgmt.rs` (the machine-management `plugin` commands in `exempt_from_dispatch`, the stop
at the external command name in `exempt_from_dispatch` and `repo_from_argv`, and the pre-exec
refusal of `plugin pin` under a release that predates plugins); `lock.rs` (one table);
`tui/mod.rs` (one folder, one pane); the Task-file adapter (one binding form, `af.task-file/2`, and the affinity kept in
`DeclaredPorts`);
`storage_sweep.rs` (the sweep record's next version with plugin kinds); `review-sandbox` (a duplex execution path with the provider's container lifecycle, for P1);
`review-process` (a retention limit and an overflow policy per stream, a reader-raised
cancellation, for P1, reused by P7). New code, by
package, is estimated at 3,500 lines including tests, most of it `plugins/` reusing
`selfmgmt`'s install, receipt and verification code. After P4 a feature like `af backlog` is
zero lines in this repository.

### 3.17 The three examples

**`af analytics`.** A `command` plugin with `storage = "state"`, hooks `task.run.ended`,
`review.run.ended` and `storage.sweep`, a machine-only table `[plugin.analytics]`, and a
pane. On each hook it reads the finished Task or Campaign through `af task show --json`,
`af review report --campaign NAME --state DIR --format json` and `af store attempts`, and
folds them into its own index under its
state directory; `af analytics spend --since 30d`, `failures`, `flakes`, `workers` and
`projects` answer from the index; `af analytics pane --json` shows the last runs. It enumerates
Stores through `af store list`, so work spread over many Stores and Workspaces is one answer.
Nothing it records is read by the kernel; `af plugin remove analytics --purge` leaves no trace.
Once the telemetry capture lands in the kernel, the same plugin reads the telemetry export
too (§3.6) and becomes the `af-telemetry` reader side.

**`af backlog`.** A `command` plugin with `storage = "state"` and the hook `task.run.ended`. It
holds an ordered queue of work items from the sources it is told about: GitHub issues through
`gh`, a Jira filter, a directory of Markdown files, a hand-written list. `af backlog add`, `rank`,
`next`, `status`. `af backlog run --parallel 2` is a foreground command: it writes one
`af.task-file/2` per item under its state directory. A Jira item, or a JSON or TOML issue file
committed in the repository, is named through the Task file's `issue` selector, the kernel's own
issue sources (ADR-0061, `IssueSource`); every other source, a GitHub issue read through `gh`,
a Markdown file, a hand-written item, is normalized by the plugin into the Task file's `goal`
and `requirements` fields, because the kernel's issue sources read nothing under plugin state
and `requirements` is not a bindable port; a GitHub-shaped fixture proves it writes nothing to
the checkout. Any Brief or evidence goes through the `file` form. It starts each Task with
`"$AF_BIN" task start --file --execute` as a child it owns, waits for, and cancels by forwarding
`SIGINT` and `SIGTERM` to the child `af`, which stops its Workers as ADR-0129 requires; when
one child returns it starts the next until the queue is empty or the operator interrupts, and a
cancellation fixture proves it leaves no Worker alive. Each start is an ordinary Task with its
own plan, budget and approval. The `task.run.ended` hook only moves the item to done, failed or
needs-human; it starts nothing (§3.8). It never approves a plan (ADR-0056), never delivers,
never pushes. Its `pane` lists the queue with the state chips.

**`af reviewer`.** A plugin that ships reviewer Worker packages under `packages/` (a
`worker.toml` with `[runner] kind = "model"` or `"command"`, instructions, schemas), and a
command `af reviewer run --pr N` that composes a review Task file for the pull request and
starts it, reading the outcome with `af task show --json` and the Task's recorded outputs, since a review
started from a Task file is a Task and has no Campaign report. A project adopts the packages by
pinning them (§3.11), so the reviewer runs under the project's policy, lock and budgets like the
kernel's own `kernel/correctness`. A reviewer for a model whose CLI af does not drive adds a
runner plugin (§3.14). Zero kernel code for the first two forms; the third is one external
adapter behind one protocol.

## 4. Values check

- **Wise token consumption.** No seam spends a token. A plugin's data reaches a model only
  through a declared, locked, byte-bounded port that the context manifest lists; a Task a
  plugin starts is reserved and bounded like any other.
- **As little information in Worker context as possible.** A Worker that does not wire the
  port sees nothing; a file-bound input is one named manifest entry with its bytes; plugins
  never touch prompts, Warm Sets or Notes.
- **Performance.** `af --help` reads small manifests and never `PATH`; `af <plugin>` costs one
  exec; hooks run after the result and are detached by default; no daemon, no polling, no
  in-process plugin code to load.
- **Fast.** A feature is a repository with a template, `af plugin install --link .` and
  `af plugin check`; the kernel's `make check` is not in its loop. Removal is one command.
- **Simple and clear.** One manifest, one environment, one hook document, one read namespace,
  one binding form; entities are not added to the kernel for features that are not authority,
  and the Project and telemetry notes lose entities rather than gain them.
- **Extendable.** Executables speaking versioned JSON, as value 6 states; the runner seam
  gives Provider kinds their second implementation.
- **Deterministic.** Plugin data enters a Task only as a pinned artifact of a non-kernel type
  with its provenance recorded; replay never invokes, locates or requires a plugin.
- **Unix-native.** `af-<name>` on `PATH`, stdio and exit codes passed through, `--json` on both
  sides, XDG directories per plugin, `$AF_BIN` for children.
- **Sharability, batteries, pluggability.** A project pins the plugins it requires; the
  conformance plugin and the template are the battery; a Provider, source or Worker is
  replaced by name.

## 5. Packages

Each package is one af Task, delivered, verified and merged before the next starts, with its
`af task report` in the pull request (ADR-0142), its schemas and parity tests, a
`changelog.d/` note, and the ADR or amendment named. Acceptance is staged with the
deliverables: a test names only what its own and earlier packages ship, so the conformance
harness's core is P1's, its Store-reader coverage P2's and its Task-creation refusal P3's;
hooks are tested unpinned in P4, under a pin in P5, and with the pane in P6.

### P0 — The decision

Deliverables: the ADR with the clauses of §3.1; `CONTEXT.md` entries of §2; this note indexed;
`docs/values.md` §6 and §8 cross-referenced rather than restated. Acceptance: the ADR's
considered options name every option of §7 with its reason.

### P1 — Command seam, manifest, install, help, config, authority

Deliverables: `crates/af/src/plugins/` (manifest, directory layout, `bin` validation,
resolution, invoke with the inherited environment and overlay); clap's `external_subcommand`;
`af plugin install|remove|list|show|check` for a directory, a local archive and `--link`, with
`origin.toml`; the `Plugins:` section of `af --help`; `af help <plugin>`; `af help plugins`
rendered from the surface table; `[plugin.*]` passthrough, the machine-only `enabled` key,
manifest-declared machine-only tables and the `AF_PLUGIN__<NAME>__<KEY>` parser; the `plugins/`
`LAYOUT` entry; `ProjectFile`'s field and `[plugins] required`; the three storage kinds of
§3.9 with `af/storage@2`, `af/storage-prune@2` and the sweep record's next version naming
them, and the locks of §3.9, so that no plugin
byte is outside the budget from the first install (`storage::Roots` enumerates explicit roots
and discovers nothing, ADR-0144); the dispatch classification of §3.3; the duplex container execution path of §3.15 in
`review-sandbox` and the stream limits and reader-raised cancellation of §3.14 in
`review-process`; `fixtures/plugins/echo/` and the boundary harness. ADR: P0's, and the amendment to ADR-0144 for the kinds and the record
version.

The help-latency bound is measured, not asserted: P1 compares `af --help` on the pinned
baseline with the implementation on a release build with twenty representative manifests
installed, 100 repetitions each, warm and cold filesystem, reporting p50 and p99 for the whole
command and for manifest discovery alone, with the hardware named; the package is not done
while p99 exceeds 50 ms.

Acceptance: `plugin_inherits_the_environment_with_the_overlay_and_passes_exit_codes` (0, 3,
and a signal, as the shell sees them after the `exec`; `HOME` and `XDG_CONFIG_HOME` reach the plugin's `af` child; only the six
invocation-owned variables are removed, `AF_REPO` among them, and a nested invocation that
left the repository sees no `AF_REPO`; a non-default `AF_PLUGIN__ECHO__COLOUR` override and
its environment origin survive into the plugin's `af config show --origin --json`; user scope
without `AF_REPO`); `plugin_on_path_runs_as_dev_and_off_under_path_false`;
`unknown_word_is_still_exit_2`; `help_lists_installed_plugins_without_scanning_path` (`PATH`
set to a directory that would fail if read); `help_latency_with_twenty_manifests` (the protocol
above); `reserved_names_are_refused` (the set is derived from the command and topic declarations;
neither an installed manifest nor `PATH` can shadow a help topic); `manifest_protocol_and_min_af_are_checked_before_exec`;
`manifest_reads_are_bounded_and_never_hang` (a 64 KiB-plus manifest and a FIFO behind a
`--link`, each listed `invalid` by `af --help` within its bound);
`bin_escaping_the_bundle_or_a_symlink_is_refused_at_install` (and `bin = "receipt.toml"`, at
install, `--link`, `check` and invocation);
`foreground_plugin_replaces_af_and_owns_its_signals` (`SIGINT`, then `SIGTERM`, sent to the
original `af` PID while the echo fixture owns a command-backed Task started through `$AF_BIN`:
the plugin is that PID, it cancels the child, no Worker and no descendant stays alive, and the
lock is released; a hook and a pane load remain children);
`plugin_locks_are_anchored_to_the_protected_roots` (identical roots, `XDG_RUNTIME_DIR` set
in one process and unset in the other: a running invocation blocks a concurrent `remove`, a
reinstall of the same version and the sweep's eviction of its aged cache; two invocations with
different `XDG_DATA_HOME` and one `XDG_CACHE_HOME`: the aged shared cache of the running
plugin is not evicted by the other's sweep, which skips it and says so; a `PATH` plugin gets
its lock files at first start; a start racing the eviction and recreation of its cache
payload keeps one lock object and recreates `data/` once; a `remove --purge` racing a start
leaves the start refused on the unlinked lock);
`plugin_context_resolves_the_git_toplevel` (invoked from `/repo/subdir`, and with
`--repo /repo/subdir`: the bundle, `enabled`, the configuration and `AF_REPO` are `/repo`'s,
and no Task Store is created or registered);
`plugin_table_merges_with_origins_and_machine_only_is_honoured`;
`existing_machine_only_tables_are_preserved` (`[self]` and `[storage]` stay machine-only with
plugins installed, beside plugin-declared filtering; the `[telemetry]` variant, the table with
its reader absent, disabled and replaced by a manifest without `machine_only`, is telemetry
T1's acceptance, which ships that table as built-in machine-only configuration (§3.10), with
P1 as its prerequisite);
`project_and_local_enabled_cannot_override_a_machine_disable`;
`env_override_reaches_plugin_backlog_enabled_with_its_origin`;
`layout_declares_plugins_directory_and_drift_test_passes`;
`surface_table_matches_the_code` (every `AF_PLUGIN_*` variable in `plugins/` is in the table
and nothing else is set); `plugin_check_verifies_the_manifest_and_each_declared_verb_within_its_bound`;
`plugin_check_runs_init_preview_then_apply_inside_the_destination_only` (the applied manifest
equals the preview in paths, bytes and modes; a file the preview did not name, a missing file,
a changed byte, a changed mode, a write under a neighbouring `.af/` path, a duplicate path, a
path with `..`, a 65th file and a 257 KiB file are each failures);
`plugin_check_broker_admits_every_declared_reader_and_rejects_the_rest` (`task report` and
`task explain` pass, `af store` coverage joins in P2; `onboard --refresh-lock`, `task plan`, and an escaping `--repo` through
`AF_BIN` are recorded as violations; `task start --file` is refused for every invocation; `help <plugin>` is refused before the
plugin's executable starts, proven by a fixture plugin whose help writes a file that never
appears, through the broker and in pane mode);
`plugin_check_runs_a_provisioned_peer_inside_the_container` (the echo fixture provisioned as
its own peer: a hook of the subject calling the peer's `--json` read command through `$AF_BIN`
passes, one trace covering both; the peer calling `task start --file` is refused and recorded;
a nested call to a peer that is not provisioned reports `not_run` and fails
`--require-boundary`);
`config_filtering_uses_the_selected_bundles_manifest` (two installed versions whose
`machine_only` differ: `current` governs, and an unreadable manifest fails closed; a pin moved
between the plugin's start and its `config show` leaves the running bundle's manifest in
force; the pinned and override variants are P5's); `plugin_arguments_never_steer_dispatch` (under a
plugin-capable pinned release, `af echo --help`, `af echo -V` and `af echo --repo /elsewhere`
each dispatch to the pinned release, which hands the words to the plugin untouched);
`a_release_predating_plugins_receives_argv_unchanged` (the cross-release fixture: `af echo
--help` under a pinned release older than this design dispatches to that release with the
argv unchanged, which answers with its usage error and starts no plugin);
`kernel_owned_repo_selects_the_invocation_repository` (from repository A and from outside
Git, `af --repo /B echo` resolves B for the bundle, `enabled`, the configuration and
`AF_REPO`, and a stale inherited `AF_REPO` is replaced);
`plugin_cache_is_evicted_whole_and_state_never_by_the_kernel`;
`an_aged_running_plugin_is_not_evicted_by_a_concurrent_sweep`;
`plugin_bytes_are_counted_from_the_first_install` (`af storage --json` prints `af/storage@2`
listing the three kinds for a freshly installed plugin, `af storage prune --json` prints
`af/storage-prune@2`, the `@1` parity fixtures are byte-identical, and the sweep record's
next version names the kinds);
`read_only_table_matches_the_code` (every command in the table opens no Store for writing;
`review plan --file` and `storage prune --apply` are outside it, and a refused `review plan
--file` under `pane` creates no Task and no CAS object; `review show` reads through
`open_campaign_store_read_only`);
`directory_install_is_a_release_source_directory` (an unpacked bundle is refused with
`--link` named; a `DirSource` directory installs with a receipt whose digest equals the
archive's pin digest);
`plugin_check_boundary_records_a_direct_store_open_and_an_outside_write`;
`plugin_check_audit_resolves_relative_and_descriptor_relative_accesses` (an `openat` through a
numeric dirfd into a permitted directory and into a forbidden one, a relative open after
`fchdir` into each, a symlink created in the cache, used and unlinked before exit 0, a write
attempted through such a link to the read-only export, and a denied outside write followed by
exit 0: the permitted ones pass and every other is a recorded violation; a trace with a lost
process is `unresolved` and fails; a working directory renamed under the process and then
left through `..`, classified by the renamed path; a thread that changes the directory and
the parent's relative open after it, classified by the shared context; an `unshare` child
whose `chdir` leaves the parent's resolution alone; a `chroot` call, `unresolved`);
`plugin_check_bounds_every_channel` (a plugin flooding each channel in turn, a broker frame
over 64 KiB, a reply over its bound, container stdout over 64 MiB, stderr over 1 MiB and a
trace over 256 MiB, crossed in a burst inside the first second: each cancels the check with
the channel named, the container's removal is confirmed, the retained trace bytes stay within
the bound, and no truncated trace is reported as evidence);
`plugin_check_without_a_container_reports_not_run`;
`plugin_check_binds_its_result_to_the_content_digest_and_target` (a native Darwin bundle is
`not_run`, `--target` checks the Linux bundle and claims nothing for the other; two archives
with different bytes and identical files share one content digest, the fixed expected-digest
fixtures of §3.15 hold, and the receipt records both digests); `broker_round_trip_from_a_macos_host_into_the_linux_container` (a brokered
`config show` answered through the relay's stdio; a provider that cannot attach stdio reports
`not_run` and fails `--require-boundary`);
`plugin_check_timeout_and_cancel_remove_the_container_before_the_fixture` (a plugin that
sleeps past the wall, and a cancelled check: the container and its descendants are gone,
confirmed, before any fixture directory is removed; a provider whose removal cannot be
confirmed refuses the check and keeps the directories).

### P2 — `af store`

Deliverables: `list`, `runs`, `events`, `attempts`, `artifact` with their documents,
`af/store-event@1` among them. ADR: the clause of P0's ADR that narrows ADR-0145 for `af store`
alone (§3.6).

Acceptance: `store_events_prints_every_row_in_sequence_with_every_column_verbatim` (compared to the
SQLite rows; two events with identical payloads that differ only in `occurred_at` and
`causation_id` stay distinguishable; null columns print as `null`); `store_events_marks_an_unknown_type_known_false_and_decodes_nothing`;
`run_event_v1_validation_is_unchanged`;
`store_artifact_verifies_envelope_and_payload_identity` (a corrupted object is refused);
`plugin_check_broker_admits_the_store_readers` (every `af store` command through the broker
against the fixture Store); `store_artifact_raw_streams_verified_bytes` (a recorded rendered request and a non-JSON
stderr object read back byte-identical; a corrupted object refused; `--raw` with `--payload`
refused);
`store_list_names_every_store_the_sweep_would_visit`; `store_reads_never_take_a_writer_lease`
(a running Task's Store stays writable); `store_read_latency_under_200ms_on_10k_events`;
`export_reader_writes_nothing` (an immutable fixture export with an abandoned segment: the
conformance plugin reads it and the export's bytes are unchanged). The writer's side, recovery
of that segment by the next kernel writer within its bound, is telemetry T1's acceptance, with
P2 as its prerequisite, together with `export_reads_by_the_kernel_are_maintenance_only` (a
fixture export under a file trace: the recovery of an abandoned segment and the sweep's
structural reads are the only kernel opens, and a Task's planning, execution, replay and
acceptance open none).

### P3 — File-bound inputs

Deliverables: `af.task-file/2` with the `file` form, `/1` refused by name; the
`task-input-bindings` document, schemas and fixtures moved together; capture, namespace check,
cardinality and affinity check, the envelope and rendered-request bounds, type check, schema
check, publication with provenance, pinning, manifest entry, preview and explain lines. ADR: an
amendment to ADR-0134.

Acceptance: `task_file_v1_is_refused_by_name`;
`file_bound_port_reaches_only_the_worker_that_wires_it`;
`file_bound_type_must_equal_the_port_type`;
`file_claiming_a_kernel_type_is_refused_even_with_a_valid_payload` (`af/Measurement@1` on a
report port); `stored_context_records_external_provenance_and_the_worker_request_is_unchanged`
(bytes delivered on first execution, retry and replay compared with a recorded-output binding of
the same payload);
`file_bound_payload_fails_the_package_schema_at_plan_time_when_the_map_is_complete`;
`file_bound_payload_fails_the_package_schema_before_dispatch_when_an_input_comes_later` (a
cross-port constraint, two consuming Workers);
`file_bound_input_replays_after_the_file_is_deleted`;
`file_bound_input_cannot_cover_an_obligation` (a pipeline wiring it into `coverage` is refused
at load); `refused_ports_stay_refused` (`requirements`, `base`, `continuation`);
`file_binding_affinity_is_admitted_before_selection` (two Pipelines accepting the kind
declare the port with one type and cardinality, one unbound and one `same_as`: an unnamed Task
is refused with the request to name a pipeline before the file is read; naming the unbound one
admits, naming the other refuses by name);
`file_bound_many_same_as_and_derived_from_ports_are_refused_by_name` (declared with
`PortAffinityV1`); `file_with_an_array_or_scalar_top_level_is_refused_before_publication`;
`plugin_check_broker_rejects_task_creation` (a brokered `task start --file`, with and without
`--execute`, whose Task file binds an outside sentinel file: rejected before any host read, the
sentinel unread); `file_bound_envelope_over_the_renderer_bound_is_refused_at_planning` (an envelope over 1 MiB
serialized whose rendered value fits: refused with the bound named, nothing published);
`file_binding_requires_an_absolute_path_and_a_regular_file` (a relative path, an oversized
file at the 8 MiB raw ceiling, a FIFO and a symlink to a FIFO, none of which blocks planning);
`file_bound_envelope_over_8mib_is_refused_before_publication` (payload plus envelope overhead
at the bound); `file_bound_static_request_over_1mib_is_refused_at_plan_time`;
`file_bound_request_pushed_over_1mib_by_a_later_output_is_refused_before_dispatch` (an earlier
Worker's output, then retry feedback, each tipping the bound).

### P4 — Hooks, state, cache, budget

Deliverables: `plugins::after_command` and its one call site; the five hooks and
`af/plugin-hook@1`; `af plugin hook-run`, the detached supervisor; both modes with their wall
bounds; `hooks.log`; the `storage.sweep` hook over P1's kinds; the background allowlist of §3.8 as the
table's second use. ADR: none new; P1's amendment to ADR-0144 gains the hook.

Acceptance: `hook_runs_after_the_result_and_changes_no_output` (stdout, `--json` document and
exit code byte-identical with and without a failing hook); `interrupted_command_runs_no_hook`;
`review_run_ended_lists_the_rounds_the_run_closed` (one for a Campaign `--heavy` invocation,
several for a Task-file review pipeline, the published Round for a run that failed after its
conclusion, none twice on resume; each entry `{ round, sequence }` matching the Store's
closure events, for a Campaign target and for a `review run --file` Task target); `task_work_is_refused_inside_a_hook` (every `runs_task_work` command under `AF_PLUGIN_BACKGROUND`,
`provider doctor` with a credential-free admission fixture and `self optimize --execute` among
them, refused before any Attempt starts, and at `task_execution::start` itself);
`task_creation_is_refused_inside_a_hook_before_capture` (`task start --file` without
`--execute`, `task plan` and `review plan --file` refused before source capture; a hanging-Git
fixture under a hook timeout leaves no nested process alive);
`hooks_run_only_the_read_only_table` (a command-only Task's `task refresh` under a hook,
refused before the Store opens writable and the plan unchanged; `catalog sync` with a hanging
Git transport, refused before the subprocess exists, a hook timeout leaving nothing alive; a
hook calling a second plugin that calls `task refresh`, refused);
`a_hook_calling_a_second_plugin_cannot_start_task_work` (the marker survives `$AF_BIN`);
`nested_plugins_die_with_the_outer_group` (a hook whose nested plugin and its grandchild sleep
past the wall: none survives the kill);
`hook_run_receives_the_exact_document_on_a_pipe` (byte-identical transfer, malformed input, a
supervisor that never reads); `hook_target_names_what_the_invocation_recorded` (a Task, a
Campaign, a `review run --file` Task, and the `none` variant for a malformed Task file, an
unknown Task ID and an unopenable Store); `hook_context_is_the_commands_repository` (from
repository A and from outside Git with `--repo B`, B's configuration and `enabled` are used
and `AF_REPO` is B; the pinned variant is P5's);
`provider_probes_are_refused_in_the_background` (a credential-free Provider fixture that
hangs: `provider status`, `provider models` and `review plan` under `AF_PLUGIN_BACKGROUND` are
refused at their entry points, and a hook timeout leaves no Provider process alive);
`storage_sweep_hook_never_runs_at_the_free_disk_floor_or_before_a_warm_key`;
`hooks_run_in_name_order_and_a_sync_hook_is_killed_at_its_wall`;
`detached_hook_receives_the_document_is_killed_at_its_wall_and_is_reaped` (a plugin that never
reads stdin, one that forks a grandchild, one that exits 3; the failure line in `hooks.log`);
`disabled_plugin_runs_nothing`; `hook_leaves_the_event_log_byte_identical` (the log with hooks
on and off differs only by the sweep observation, when one is recorded);
`storage_sweep_hook_receives_target_bytes_and_cannot_block_the_sweep` (the report's
`total_after` and stop reason describe the state after the plugins ran, `target_bytes` is
remeasured between plugins, and `af storage prune --apply --json` prints its document after
the hooks with what they freed).

### P5 — Pins and release sources

Deliverables: `[plugins.<name>]` in the lock; `af plugin pin`; `af onboard --refresh-lock` for
required plugins; GitHub and directory release sources; receipts with `verified_by`; the key
rule; install on demand; `AF_PLUGIN_VERSION_<NAME>`. ADR: an amendment to ADR-0045.

Acceptance: `pinned_plugin_runs_the_pinned_version_and_installs_on_demand_by_digest` (from
the recorded origin); `pin_without_an_origin_refuses_with_the_install_command`;
`install_from_another_source_is_refused_without_origin_replacement`;
`pin_without_a_digest_for_this_target_is_never_installed`; `dev_plugin_never_satisfies_a_pin`;
`offline_refuses_with_the_command_to_run`; `signature_mismatch_refuses_install`;
`remove_refuses_a_version_a_seen_project_pins`; `plugin_pin_dispatches_to_the_pinned_release`
(a cross-release fixture: the lock written by the pinned release, and the pre-exec refusal
whose printed `af onboard --refresh-lock --af <release>` parses and advances the pin);
`hook_context_uses_the_projects_pin` (from repository A with `--repo B`, B's pinned bundle
runs); `override_precedes_the_pin_for_every_use` (a pin and `AF_PLUGIN_VERSION_<NAME>` both
present, their manifests differing in `machine_only`: the override's bundle runs and governs
`config show`, a hook and a pane alike); `background_resolution_never_invokes_the_release_source` (a pinned, uninstalled plugin:
the hook fails with the install command in `hooks.log`, and a release source that would fail if
touched is never touched).

### P6 — The pane

Deliverables: `af/plugin-pane@1` with its bounds; the `plugins/` folder; the generic renderer;
`R`, `Enter`, actions as prefills; cancellation and reaping of superseded loads. No new ADR; an
amendment to ADR-0119 and ADR-0123.

Acceptance: `plugin_pane_paints_only_what_the_document_carries`;
`plugin_pane_loads_off_the_key_loop_and_a_slow_plugin_shows_the_spinner`;
`oversized_malformed_and_timed_out_pane_documents_are_one_error_row`;
`reload_cancels_and_reaps_the_superseded_load` (a nested plugin and its grandchild included);
`pane_action_prefills_and_never_submits` (a word with a space and a quote round-trips through
the prefill); `pane_action_with_confirm_is_refused` (`--confirm X` and `--confirm=X`);
`pane_action_keys_never_shadow_the_browser` (a document binding `q`, `R`, `g` or `r` is one
error row; a permitted key acts only with the pane focused, and from the bar the browser's
binding wins);
`pane_load_runs_only_the_read_only_table` (a pane calling `task start --execute` is refused
before any Attempt and its cancellation leaves no Worker; a pane calling `task start --file`
without `--execute` or `task plan` is refused and the Store's event log is byte-identical);
`pane_load_of_an_uninstalled_pinned_plugin_is_one_error_row` (naming `af plugin install`, the
release source untouched); `pane_reload_and_timeout_leave_no_provider_process` (the hanging
Provider fixture of P4); the existing
`terminal_data_is_printable_ascii` and the TUI's timing tests unchanged.

### P7 — The runner seam

Deliverables: `af.runner/1`, `af/RunnerRequest@1`, `af/RunnerResult@1`, `af/RunnerIdentity@1`;
`AF_RUNNER_AUTH_DIR`; the external adapter behind `WorkerModelAdapter`, reusing P1's stream
limits and reader-raised cancellation in `review-process` (§3.14); the
Provider registry accepting a runner plugin's kind; `identity`, `status`, `models` and admission through the
plugin; one external adapter as the proof. ADR: its own.

Acceptance: `two_providers_of_one_runner_kind_use_their_own_auth_directories`;
`an_account_change_is_refused_before_the_private_input_is_delivered`;
`runner_identity_is_never_printed_or_stored` (only the derived principal is recorded);
`external_runner_is_supervised_cancelled_and_deadlined_by_the_kernel`;
`external_runner_output_is_redacted_and_stored_as_raw_artifacts_before_parsing`;
`external_runner_usage_frames_are_settled_like_a_native_observation` (known zero; no frame at
all, settled at zero and marked unknown, ADR-0143; a valid frame followed by one with
`charge_complete = false`, retaining the counters and the floor, ADR-0088; a malformed first
frame carrying one valid counter, retained with `charge_complete = false` and the output
refused; a valid frame then a truncated terminal frame; a valid frame then cancellation;
decreasing cumulative counters that keep the earlier floor; `charge_complete = false` followed
by `true` that stays incomplete; a complete usage frame then a malformed terminal result, the
charge kept complete and the output refused; a malformed terminal result with no usage frame,
zero marked unknown);
`runner_verbs_are_refused_on_the_public_seam` (`af <runner> identity --json` executes nothing
and prints no identity; `af provider status` shows the sanitized projection);
`runner_transport_bounds_are_enforced_while_reading` (an oversized unterminated frame, a
stderr flood, a stalled `status`: each killed and reaped at its bound by `review-process` the
moment the reader refuses, not after the leader exits, output refused, earlier usage floors
kept; the native adapters' collection byte-identical);
`runner_binding_is_captured_verified_and_never_substituted` (a second plugin declaring the
kind is refused at install; `current` moved to other bytes refuses the Attempt; the
executable, then a support file, edited in place with `current` and `receipt.toml` untouched
refuses the Attempt, as do a mode change and a symlink that appeared after install; a
`remove` racing an invocation waits on the lock of §3.9; resume under a changed bundle is
refused; replay validates the record with the plugin removed);
`external_runner_is_trusted_unsafe_and_cannot_auto_apply`;
`native_adapters_are_byte_identical_with_the_seam_present`; the Task fixture run through the
external adapter with a command-backed fake model.

After P4 the first-party plugins start, each in its own repository from the template:
`af-backlog` and `af-analytics` first, because they need nothing beyond P4; `af-project` and
`af-telemetry` as their notes are re-planned under §3.16; `af-reviewer` with P7 when a new
Provider kind is wanted.

## 6. Not in v1

- **A list-of-files binding.** A v1 file binding is one artifact on a `one` port; a form that
  gathers several files into a `many` port is a later decision with its own fixtures.
- **Plugin-to-plugin dependencies.** A plugin calls another's command line; the manifest
  declares no dependency graph.
- **Completion of a plugin's own arguments.** `af completions` completes plugin names; a
  plugin's flags complete when a later package defines `__complete`.
- **Plugins inside a plan.** A deterministic program that must run inside a Task is a command
  Worker package, digest-pinned and sandboxed, as today. No `op = "plugin"`.
- **Plugin-appended events.** The event vocabulary stays closed (`EventType`).
- **Sandboxing plugins.** A plugin runs as the operator's own tool, like `gh` or `git`. What is
  sandboxed is a Worker, including a plugin-shipped one.
- **Moving the TUI out of the kernel.** P6 makes the browser host plugin panes; `af-tui` as a
  plugin that bare `af` runs is a later decision this design enables and does not take.
- **Windows.** As for af itself.

## 7. Considered options

- **Dynamic libraries with a plugin ABI.** The workspace forbids `unsafe_code`, Rust has no
  stable ABI, and a plugin's panic takes `af` down with a Task's writer lease. Rejected.
- **WebAssembly components.** Sandboxed and portable, which is attractive for pure data
  operators; but the plugins the owner named need `git`, `gh`, SQLite, the network and child
  processes, and a component runtime brings an async stack into a synchronous kernel. Rejected
  for v1; a later host for sandboxed in-plan operators would be a new decision.
- **Crates in this workspace behind Cargo features.** Still kernel code: the same `cli.rs`,
  `main.rs`, CI, release train and review gate, which is what the owner wants to stop paying.
  Rejected.
- **An embedded scripting language.** A second language without the ecosystem the plugins use.
  Rejected.
- **An in-process registry (`inventory`, `linkme`).** Needs a rebuild of `af` for every plugin.
  Rejected.
- **`PATH` discovery only, no install directory.** The `git-*` convention alone gives no pins,
  no receipts, no `af --help` listing without scanning `PATH`, and no removal. Kept as the
  development loop beside the install directory.
- **Plugins reading SQLite and the CAS directly.** Ties every plugin to tables that are not a
  contract and bypasses verification on read. Rejected for `af store`.
- **Handing each command's `--json` document to the hook.** Every command would return its
  document to `main` instead of printing it. Rejected for an identifier the plugin resolves
  with one `af` call.
- **A daemon or an event bus for hooks.** Rejected by values 3 and 8; detached children with a
  bound, as `self refresh-check`.
- **Long-lived plugin processes over JSON-RPC.** A protocol with session state for
  invocations that have none. Rejected.
- **An `op = "plugin"` Task operator.** Replay would depend on an executable the Store does not
  pin; the command Worker already runs a pinned program in a sandbox. Rejected.
- **Tasks started from hooks.** A hook is killed with its process group at its wall; a Task's
  Workers are in their own groups and would outlive the kernel that settles them. Refused by
  the kernel while `AF_PLUGIN_BACKGROUND` is set.
- **A hook on interruption.** The interrupted path exits as ADR-0129 requires; a plugin that
  must know reads `af store runs` next time. Rejected.
- **Proving the conformance boundary with file permissions.** The plugin and its `af` child run
  as the same user; whatever one cannot read, neither can. Rejected for the container harness.
- **A plugin monorepo.** One release train and one PR queue again. One repository per plugin
  from a template is recommended; the owner decides (§8).

## 8. Open decisions for the owner

1. **The namespace name.** `af plugin` here; `af ext` is shorter and reads less like an
   implementation detail. The note keeps `plugin`, the word the values use.
2. **`PATH` fallback on by default.** On here, for the development loop and value 8; off would
   mean `af plugin install --link .` is the only way to run an unreleased plugin.
3. **Hook default mode.** `detached` here. `sync` by default makes a backlog's next start
   visible before the prompt returns, at the cost of up to `hook_wall_ms` per run.
4. **Re-planning the two notes.** Whether `af project` and `af telemetry` are built as in §3.16
   now, with their kernel parts first, or shipped as designed and extracted later. The note
   recommends now: both are the first proof that the seams are enough.
5. **Automatic Briefs.** PJ3's root default rendered a Brief at plan time without a Task file
   naming it; under this design the starter binds a file. Whether one kernel root default is
   wanted for Briefs is the owner's call; the note recommends the explicit binding.
6. **Repositories.** One per plugin from a template (recommended), or a plugins workspace.
7. **Signatures.** Digest-only installs allowed with a visible receipt and keys recorded at
   first install (here, mirroring `af self`, whose receipts are `lock`, `minisign` or
   `sha256sums`), or a signature required for every release install.
