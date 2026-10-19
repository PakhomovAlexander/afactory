# Plugins: every feature that is not the kernel, as an executable af finds, runs and pins

**Status:** proposed 2026-10-19; revision 8 after review rounds 1 to 7 (Codex gpt-6-astra, xhigh).
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
   readers and never reads back for any decision, listed one by one in the surface table. It
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

The kernel execs the plugin with the rest of the command line as its arguments, stdin, stdout
and stderr inherited, in the caller's working directory, and exits with the plugin's exit code
(128 plus the signal number for a signal). `af <word> --help` is the plugin's help; `af help
<word>` runs `<plugin> --help`. Clap gains one `external_subcommand` variant; nothing else in
`cli.rs` changes for any later plugin.

A plugin inherits the operator's environment, so `HOME`, the XDG variables, `PATH` and every
`AF_*` override, the `AF_PLUGIN__<NAME>__<KEY>` overrides of §3.10 included, reach it and the
`af` children it runs. The invocation protocol is the overlay the kernel adds, after removing
exactly the six invocation-owned variables (`AF_PLUGIN_PROTOCOL`, `AF_PLUGIN_NAME`,
`AF_PLUGIN_DIR`, `AF_PLUGIN_STATE`, `AF_PLUGIN_CACHE`, `AF_REPO`) an enclosing invocation may
have left, so that a plugin which leaves its repository and calls another never hands it a
stale `AF_REPO`; `AF_REPO` is set again only from the new invocation's own working directory.
Nothing else is removed, and `AF_PLUGIN_BACKGROUND` in particular is inherited and never
cleared (§3.8):

| Variable | Holds |
|---|---|
| `AF_PLUGIN_PROTOCOL` | `1`; a plugin whose manifest declares another protocol is refused before it runs |
| `AF_PLUGIN_NAME` | the plugin's name |
| `AF_PLUGIN_DIR` | the directory holding its manifest and executable |
| `AF_PLUGIN_STATE` | `$XDG_STATE_HOME/af/plugins/<name>`, created on first use when the manifest declares `storage = "state"` |
| `AF_PLUGIN_CACHE` | `$XDG_CACHE_HOME/af/plugins/<name>`, likewise for `"cache"` or `"state"` |
| `AF_BIN` | the absolute path of the running `af`: the plugin calls this, never `af` from `PATH` (ADR-0123) |
| `AF_VERSION` | the running release |
| `AF_REPO` | the git toplevel the command was run in, absent outside a repository |
| `AF_DISPATCHED_FROM` | set as the browser's hand-off sets it, so the plugin's own `af` calls run this binary and never dispatch again |
| `AF_PLUGIN_BACKGROUND` | only for a background invocation: `hook:<name>` for a hook, `pane` for a pane load; inherited by every child and never cleared, and while it is set the kernel starts no Worker (§3.8) |

The plugin reads its configuration with `"$AF_BIN" config show --json`, adding
`--repo "$AF_REPO"` only when `AF_REPO` is set, and takes `plugin.<name>`; origins come with
`--origin`. The kernel passes no configuration itself,
so a plugin's keys are its own and `af config show` prints them like every other table.

Names are `[a-z][a-z0-9-]{1,31}`. Reserved: the kernel's command names (`review`, `task`,
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

Dispatch is unchanged: `af <plugin>` inside a pinned project first execs the release the lock
pins, which then runs the plugin (ADR-0044). The machine-management commands `af plugin
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
inside the installed version directory; a symlink anywhere in a bundle, and an archive entry
that would land outside it, are refused at install before any receipt is written, so the bytes
a pin verifies are the bytes that run. The schema is `schemas/plugin-v1.json` with the parity
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
| `af plugin install SOURCE [--version V] [--key FILE]` | installs from a release (`owner/repo`, GitHub through `gh` as `af self` does), a local archive, or a directory; verifies and records a receipt; writes `origin.toml` (`af.plugin-origin/1`: the source and the key) on a first install, and refuses a later install from another source unless `--origin` replaces it; makes it current |
| `af plugin install --link DIR` | a development install: `current` links the directory, no receipt, reported `dev` |
| `af plugin remove NAME [--purge]` | removes every installed version; `--purge` also removes state and cache; refuses a version a project seen on this machine pins unless `--force` |
| `af plugin list [--json]` | installed plugins with version, source and receipt; `dev` plugins on `PATH`; what the project requires and pins |
| `af plugin show NAME [--json]` | the manifest, the receipt, the directories and their bytes |
| `af plugin check [DIR]` | the conformance checks of §3.15 against a directory, before publishing |
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
| `af store events --state DIR --run ID [--from SEQ] [--type T]` | the event rows in sequence order, one line each: `sequence`, `type` as the raw string the row carries, `payload` as the raw JSON value the row carries, `known` (whether this release's `EventType` names the type); nothing is decoded into a `RunEvent` | `af/store-event@1`, JSONL |
| `af store attempts --state DIR --run ID` | the `attempt_wall` sidecar rows: Attempt, node, round, epoch, start, elapsed, usage | `af/store-attempts@1` |
| `af store artifact --state DIR ID [--payload]` | one artifact by ID, its envelope and payload verified on read, as the CAS reads it | the artifact's own type |

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
never read back by the kernel for any decision (the telemetry note's own rule). A plugin reads
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
file's top level must be a canonical JSON object, because the artifact envelope admits nothing
else (`ArtifactEnvelope::validate`); an array or a scalar at the top level is refused before
publication, and arrays live inside the object. A list-of-files form is deferred (§6).

The limits are the ones the kernel already enforces, and never by truncation: the serialized
envelope must fit the CAS bound (8 MiB, `MAX_ARTIFACT_ENVELOPE_BYTES`), and the complete
rendered request of every Worker that wires the port, this input included, must fit the Worker
request bound (1 MiB, `MAX_WORKER_BYTES`). Planning checks what it can see, the captured file
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

A hook never runs Task work. While `AF_PLUGIN_BACKGROUND` is set, the kernel refuses every command
that starts Workers with the reason: `runs_task_work`'s list (`task run`, `task start
--execute`, `review run`, bare `af review` and `provider doctor`, whose admission Attempts are
paid Workers too, ADR-0129) extended by `self optimize --execute`, which reaches
`task_execution::start` by its own route today; and the refusal is applied at that shared
entry point as well as in the classification, so no command route bypasses it. A hook is
killed at its wall with its process group while Workers live in groups of their own
(`review-process` places them there): killing a nested `af` would leave paid Workers running
without the kernel that settles them. `AF_PLUGIN_BACKGROUND` is inherited by
every process a hook starts, a plugin called through `$AF_BIN` included, and the kernel never
clears it, so a hook cannot launder Task work through a second plugin. A plugin that starts Tasks does so from a
foreground command of its own that owns, waits for and cancels them (§3.17, `af backlog`).

The document on stdin is `af/plugin-hook@1`: `schema`, `hook`, `af_version`, `repo`, `argv`,
`exit_code`, `occurred_unix_ms`, `target`, and the members the table names. The environment is §3.3's with
`AF_PLUGIN_BACKGROUND=hook:<name>`.

A hook's context is the command's, not the shell's: the repository the command resolved
(`--repo`, or the working directory's toplevel, exactly as `task_execution::state_path`
resolves it; a command run from repository A with `--repo /B` is B's) decides `AF_REPO`, the
effective configuration (`enabled`, `[plugin.<name>]`), the pin of §3.13 and therefore the
bundle that runs; the kernel resolves all of that once, before it spawns anything, and
`hook-run` receives the result as arguments, never re-deriving it from a working directory.

Hooks run after the command's result is written and flushed, after the post-command Storage
Budget sweep, and before the update check's detached child, in plugin-name order. In both
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

A plugin declaring `storage = "state"` owns `$XDG_STATE_HOME/af/plugins/<name>/` and
`$XDG_CACHE_HOME/af/plugins/<name>/`; `"cache"` owns the second only; `"none"` neither, and a
plugin writing there anyway is a bug `af plugin check` catches with a fixture run. The kernel
counts three storage kinds per plugin, `plugin-install:<name>`, `plugin-state:<name>` and
`plugin-cache:<name>`, in `af storage` and the budget (ADR-0144). Cache directories are entries
the kernel evicts whole, least recently used first, never within an hour of use. State is never
evicted by the kernel. The `storage.sweep` hook runs only from the two sweeps that happen after
a command's result, the post-command sweep and `af storage prune --apply`, and only when the
budget is still exceeded after the kernel's own eviction; it runs synchronously, each enabled
plugin in name order with the remaining `target_bytes`, under its own bound of 60 seconds
whatever the manifest's `hook_wall_ms` says, and the sweep reports what it could not free. The
sweeps the kernel runs during execution, at the free-disk floor (`ensure_free_disk`) and
before a new warm key (`before_new_warm_key`), never invoke a plugin: nothing a plugin does can
sit inside admission or an Attempt. Whatever remains is the operator's with
`af plugin remove --purge`. Installed versions other than
`current` and the pinned ones are evicted like other installed versions.

Nothing in use is evicted (ADR-0144). Every plugin process the kernel starts, a command, a hook
supervisor, a pane load or a runner adapter, holds a shared lock on
`$XDG_RUNTIME_DIR/af/plugins/<name>.lock` for its whole lifetime; the hook supervisor holds it
while the plugin's process group lives. The sweep takes the exclusive lock without blocking
before it evicts any entry of that plugin, skips the plugin and says so when it cannot, and
re-reads the entry's identity and protection under the lock before removing it, as
`campaign_in_use` guards a Campaign today. Every start records the plugin's last use.

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
case with `-` written as `_` (names never contain `_`, so the mapping is one-to-one), parsed by
an explicit plugin branch ahead of the one-split `AF_<TABLE>__<KEY>` rule, which would
otherwise read it as a table named `plugin_backlog`. Origins report the variable.

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
previews, writes nothing, and prints `af/plugin-init@1` (the paths under `.af/plugins/<name>/`
it would create and their bytes); `<bin> init --repo PATH --apply` creates exactly those files,
absent-only, refusing when any exists, within 60 seconds. The kernel cannot enforce that on a
plugin; `af plugin check` runs the preview in a fixture repository and fails on any write, then
the apply with only the absent destination writable, and fails on any other write under
`.af/`, any write outside it, or a file the preview did not name.

Worker, Pipeline and kind packages a plugin ships are pinned by a project exactly as the
kernel's own are: a `path` under `.af/plugins/<name>/packages/` in `task-catalog.toml` or a
sync from the plugin's repository into `vendor/`, digests in the lock, replacement rules
unchanged (ADR-0052, ADR-0053). The kernel knows nothing about who wrote a package.

### 3.12 A pane in the browser

A plugin with `pane = true` answers `af <name> pane [--item ID] --json` with `af/plugin-pane@1`:
`title`, `items` (the bar entries under the plugin's folder: `id`, `label`, nested `children`),
`rows` (`text`, optional `tone` from the design system's chip set: `run`, `pass`, `fail`),
`legend`, `status` and `actions` (a key to an `af` command line). The document is bounded: at
most 1 MiB, 1,024 items, a depth of eight, 4,096 rows, and a load deadline of ten seconds. The
bar gets one folder `plugins/` with a child per such plugin; one generic renderer paints every
plugin pane, loads it on a thread like the other panes, reloads on `R`, and shows nothing a
document does not carry; a superseded load is cancelled and its process reaped on reload, on
closing the pane and on quit, and a malformed, oversized or timed-out document is one error
row. A pane load runs with `AF_PLUGIN_BACKGROUND=pane`, and in that mode the kernel runs only
the commands of its declared read-only table (`READ_ONLY_COMMANDS` in `main.rs`, beside
`runs_task_work`: `config show`, `task list|show|report|explain`, `review
plan|report|ledger|show|campaigns`, `store *`, `storage` without `prune --apply`, `provider
status|models`, `plugin list|show`, `help`, `--version`), each of which opens a Store read-only
or none at all; every other command, `task plan` and `task start --file` without `--execute`
among them, is refused before any Store is opened for writing. So a load that is killed on
timeout, reload or close can orphan no Worker, and loading a pane writes no Store (ADR-0119).
The same table is the conformance broker's reader allowlist (§3.15) and the reader surface
`af help plugins` prints (§3.16): one declaration, three uses. An action prefills the `:` line and never submits it: the operator reads the line and
presses Enter or `Esc`, exactly as the Tasks pane's `r` and `D` work (ADR-0123, Prefills). A
document whose action carries `--confirm` is refused as malformed, so a plugin cannot reduce
delivery's typed Task-ID confirmation to a keypress. The plugin's pane is a projection of its
own `--json` commands, by the TUI's own rule. No plugin can add a key the browser does not have
or paint outside its rows.

### 3.13 Pins

`.af/af.lock` gains `[plugins.<name>]` with `version` and `[plugins.<name>.digests]` per target,
written by `af plugin pin` and by `af onboard --refresh-lock` for every plugin the project
requires. Inside such a project `af <name>` runs the pinned version, installing it on demand
from the source this machine's `origin.toml` for that plugin names, when the archive matches
the lock's digest for this target. A plugin never installed on this machine has no origin, and
the pin is then refused with `af plugin install SOURCE` as the next action: a repository pin
locates nothing and authorizes no source, exactly as `af self` fetches only from its configured
release source. The rest follows the rules a pin of af itself follows: a pin without a digest
for this target is never installed on demand,
`AF_SELF_OFFLINE=1` or `[self] install_pins = false` refuses with the command to run, an
installed copy must carry the digest in its receipt, and a `dev` or `PATH` plugin never
satisfies a pin. `AF_PLUGIN_VERSION_<NAME>` overrides for one invocation. The pinned version is
what a plugin's hooks run as well. A Task's replay does not depend on any of this (clause 5);
pins exist so that a team runs the same plugin, which is what sharability asks.

### 3.14 The runner seam

The second implementation a Provider kind still lacks is an external one. A runner adapter is a
plugin whose manifest declares `runner = "gemini"` (one kind per plugin) and whose executable
speaks `af.runner/1` through four verbs, so the Provider registry's `kind` may name it. A kind
resolves to exactly one installed plugin: the native kinds (`claude`, `codex`) are reserved, a
second plugin declaring a kind another installed plugin declares is refused at install, and a
`PATH` plugin never supplies a runner. The runner is captured authority, not a lookup: at plan
time the kernel records the selected runner's plugin name, version, protocol, target and
bundle digest in the Task's execution binding beside the Provider binding (ADR-0046), verifies
the installed bundle's receipt against that record before every invocation, refuses the
Attempt when `current` has moved to other bytes, and refuses substitution on resume; a plan
approved under one runner never runs under another, and replay validates the record without
the executable. It is
the exception §3.1 clause 2 names: the kernel hands it one Attempt's materialized sandbox and
the exact input bytes it composed, under the Attempt's access class, and nothing else; it never
sees a Store, another sandbox or another Attempt. P0's ADR states the two kinds of plugin and
this bound in the same clause.

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
| `identity --json` | — | `af/RunnerIdentity@1`: the account's stable identifier and authentication method under `AF_RUNNER_AUTH_DIR`; private, never printed by a public command, never stored |
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
its one `ModelWorkerReturn`. A frame that
is present but fails validation is malformed billing, not absence, and is treated as
`add_usage` treats a malformed native report (ADR-0088): the counters it carries unambiguously
are retained, the observation is recorded with `charge_complete = false`, cumulative floors are
kept, and the Attempt's business output is refused. Only an invocation that wrote no usage
frame at all is the wholly absent case, settled at zero and marked unknown (ADR-0143).

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

A conformance result is bound to one bundle: its digest and its target, and `af plugin check`
prints both. The boundary check runs the bundle inside a Linux container, so it checks a bundle
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
command line over a mounted FIFO to the harness, which runs the real `af` against the fixture
Store outside and returns stdout and the exit code. The harness lives only for the check, records every brokered call, and executes none without
checking it first against a typed allowlist: the kernel's declared read-only table (§3.12,
`task report` and `task explain` among its entries) with its flags, and, for a command or hook
invocation but never a pane load, the one write form a plugin owns, `task start --file`
without `--execute`, each only against the fixture's own Store and repository; the brokered `af` runs with the fixture's `HOME` and XDG tree, and a selector that
escapes it (`--repo`, `--state`, `--file` outside the fixture) is rejected before execution.
Any other command, `onboard --refresh-lock` among them, is rejected and recorded as a boundary
violation, since the plugin's syscall trace cannot see what a host-side `af` writes. Denial alone proves nothing, because a plugin can swallow the error and exit 0, so the
harness also audits: inside the container the plugin runs under a file-syscall tracer
(`strace -f -e trace=%file`, the container is Linux on every host, with the ptrace capability
the harness requests for this check only), and the trace, not the plugin's output, is the
record. Any path-naming syscall outside the plugin's bundle, its declared directories, the
mounted export (reads only), the fixture repository (reads, and `init`'s one destination), the
broker FIFO, the system paths the image provides and a private scratch directory is a
violation whatever its result, `ENOENT` and `EACCES` included. This harness policy is the
check's own and shares nothing with the Worker sandbox policy. Without a container provider,
or when the provider cannot grant the tracer, the boundary check is reported `not_run` and
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
| kernel declares | the read-only command table of §3.12: pane mode, the conformance broker and the printed reader surface | one table in `main.rs` |
| kernel reads | the manifest of §3.4 | `af.plugin/1` |
| kernel keeps | the origin of §3.5, the receipts, `hooks.log` | `af.plugin-origin/1`, `receipt.toml`, one line per failure |
| plugin reads | every `--json` document the CLI prints | each its own `kind@version` |
| plugin reads | `af store list`, `runs`, `events`, `attempts`, `artifact` | `af/store-*@1` |
| plugin reads | the kernel exports of §3.6: `$XDG_STATE_HOME/af/telemetry/v1/` (`segments/`, `blobs/`) | the telemetry note's `af/Telemetry*@1` records; one row per export |
| plugin writes | a Task file | `af.task-file/2` |
| plugin writes | a file-bound root input | the `file` form of `inputs` |
| plugin answers | a pane | `af/plugin-pane@1` |
| project declares | `[plugins] required`, `[plugin.<name>]`, `.af/plugins/<name>/`, `[plugins.<name>]` in the lock | TOML, as every declaration |

Call sites in existing files, the complete list: `cli.rs` (the `external_subcommand` variant,
the `plugin` and `store` namespaces); `main.rs` (one arm each for the two namespaces, one call
to `plugins::after_command`); `topics.rs` (one topic, rendered from the surface table);
`layout.rs` (one entry); `config.rs` (the `[plugin.*]` passthrough and manifest-declared
machine-only names); `project.rs` (one opaque field, one table); `storage.rs` (three kinds);
`selfmgmt.rs` (the machine-management `plugin` commands in `exempt_from_dispatch`, and the
pre-exec refusal of `plugin pin` under a release that predates plugins); `lock.rs` (one table);
`tui/mod.rs` (one folder, one pane); the Task-file adapter (one binding form, `af.task-file/2`);
`storage_sweep.rs` (the sweep record's next version with plugin kinds). New code, by
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
`changelog.d/` note, and the ADR or amendment named.

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
`LAYOUT` entry; `ProjectFile`'s field and `[plugins] required`; `fixtures/plugins/echo/` and the
boundary harness. ADR: P0's.

The help-latency bound is measured, not asserted: P1 compares `af --help` on the pinned
baseline with the implementation on a release build with twenty representative manifests
installed, 100 repetitions each, warm and cold filesystem, reporting p50 and p99 for the whole
command and for manifest discovery alone, with the hardware named; the package is not done
while p99 exceeds 50 ms.

Acceptance: `plugin_inherits_the_environment_with_the_overlay_and_passes_exit_codes` (0, 3,
and a signal; `HOME` and `XDG_CONFIG_HOME` reach the plugin's `af` child; only the six
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
`bin_escaping_the_bundle_or_a_symlink_is_refused_at_install`;
`plugin_table_merges_with_origins_and_machine_only_is_honoured`;
`kernel_capture_tables_stay_machine_only_without_the_reader_plugin` (`[telemetry]` with the
reader absent, disabled and replaced by a manifest without `machine_only`);
`project_and_local_enabled_cannot_override_a_machine_disable`;
`env_override_reaches_plugin_backlog_enabled_with_its_origin`;
`layout_declares_plugins_directory_and_drift_test_passes`;
`surface_table_matches_the_code` (every `AF_PLUGIN_*` variable in `plugins/` is in the table
and nothing else is set); `plugin_check_verifies_the_manifest_and_each_declared_verb_within_its_bound`;
`plugin_check_runs_init_preview_then_apply_inside_the_destination_only` (a file the preview did
not name, a write under a neighbouring `.af/` path);
`plugin_check_broker_admits_every_declared_reader_and_rejects_the_rest` (`task report` and
`task explain` pass; `onboard --refresh-lock`, `task plan`, and an escaping `--repo` through
`AF_BIN` are recorded as violations; `task start --file` passes for a command and is refused for
a pane load);
`read_only_table_matches_the_code` (every command in the table opens no Store for writing);
`plugin_check_boundary_records_a_direct_store_open_and_an_outside_write`;
`plugin_check_without_a_container_reports_not_run`;
`plugin_check_binds_its_result_to_the_bundle_digest_and_target` (a native Darwin bundle is
`not_run`, `--target` checks the Linux bundle and claims nothing for the other).

### P2 — `af store`

Deliverables: `list`, `runs`, `events`, `attempts`, `artifact` with their documents,
`af/store-event@1` among them. ADR: the clause of P0's ADR that narrows ADR-0145 for `af store`
alone (§3.6).

Acceptance: `store_events_prints_every_row_in_sequence_with_payloads_verbatim` (compared to the
SQLite rows); `store_events_marks_an_unknown_type_known_false_and_decodes_nothing`;
`run_event_v1_validation_is_unchanged`;
`store_artifact_verifies_envelope_and_payload_identity` (a corrupted object is refused);
`store_list_names_every_store_the_sweep_would_visit`; `store_reads_never_take_a_writer_lease`
(a running Task's Store stays writable); `store_read_latency_under_200ms_on_10k_events`;
`export_reader_writes_nothing_after_a_crash` (a fixture export with an abandoned segment: the
conformance plugin reads it, the export's bytes are unchanged, and the next kernel writer
recovers it within its bound).

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
`file_bound_many_same_as_and_derived_from_ports_are_refused_by_name` (declared with
`PortAffinityV1`); `file_with_an_array_or_scalar_top_level_is_refused_before_publication`;
`file_binding_requires_an_absolute_path_and_a_regular_file` (a relative path, an oversized
file at the 8 MiB raw ceiling, a FIFO and a symlink to a FIFO, none of which blocks planning);
`file_bound_envelope_over_8mib_is_refused_before_publication` (payload plus envelope overhead
at the bound); `file_bound_static_request_over_1mib_is_refused_at_plan_time`;
`file_bound_request_pushed_over_1mib_by_a_later_output_is_refused_before_dispatch` (an earlier
Worker's output, then retry feedback, each tipping the bound).

### P4 — Hooks, state, cache, budget

Deliverables: `plugins::after_command` and its one call site; the five hooks and
`af/plugin-hook@1`; `af plugin hook-run`, the detached supervisor; both modes with their wall
bounds; `hooks.log`; the liveness lock; the three storage kinds, the sweep record's next version
naming them, and the `storage.sweep` hook. ADR: an amendment to ADR-0144 for the kinds and the
record version.

Acceptance: `hook_runs_after_the_result_and_changes_no_output` (stdout, `--json` document and
exit code byte-identical with and without a failing hook); `interrupted_command_runs_no_hook`;
`review_run_ended_lists_the_rounds_the_run_closed` (one for a Campaign `--heavy` invocation,
several for a Task-file review pipeline, the published Round for a run that failed after its
conclusion, none twice on resume; each entry `{ round, sequence }` matching the Store's
closure events, for a Campaign target and for a `review run --file` Task target); `task_work_is_refused_inside_a_hook` (every `runs_task_work` command under `AF_PLUGIN_BACKGROUND`,
`provider doctor` with a credential-free admission fixture and `self optimize --execute` among
them, refused before any Attempt starts, and at `task_execution::start` itself);
`a_hook_calling_a_second_plugin_cannot_start_task_work` (the marker survives `$AF_BIN`);
`hook_run_receives_the_exact_document_on_a_pipe` (byte-identical transfer, malformed input, a
supervisor that never reads); `hook_target_names_what_the_invocation_recorded` (a Task, a
Campaign, a `review run --file` Task, and the `none` variant for a malformed Task file, an
unknown Task ID and an unopenable Store); `hook_context_is_the_commands_repository` (from
repository A and from outside Git with `--repo B`, B's configuration, pin and bundle are used
and `AF_REPO` is B);
`storage_sweep_hook_never_runs_at_the_free_disk_floor_or_before_a_warm_key`;
`hooks_run_in_name_order_and_a_sync_hook_is_killed_at_its_wall`;
`detached_hook_receives_the_document_is_killed_at_its_wall_and_is_reaped` (a plugin that never
reads stdin, one that forks a grandchild, one that exits 3; the failure line in `hooks.log`);
`disabled_plugin_runs_nothing`; `hook_leaves_the_event_log_byte_identical` (the log with hooks
on and off differs only by the sweep observation, when one is recorded);
`plugin_cache_is_evicted_whole_and_state_never_by_the_kernel`;
`an_aged_running_plugin_is_not_evicted_by_a_concurrent_sweep`;
`storage_sweep_hook_receives_target_bytes_and_cannot_block_the_sweep`.

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
whose printed `af onboard --refresh-lock --af <release>` parses and advances the pin).

### P6 — The pane

Deliverables: `af/plugin-pane@1` with its bounds; the `plugins/` folder; the generic renderer;
`R`, `Enter`, actions as prefills; cancellation and reaping of superseded loads. No new ADR; an
amendment to ADR-0119 and ADR-0123.

Acceptance: `plugin_pane_paints_only_what_the_document_carries`;
`plugin_pane_loads_off_the_key_loop_and_a_slow_plugin_shows_the_spinner`;
`oversized_malformed_and_timed_out_pane_documents_are_one_error_row`;
`reload_cancels_and_reaps_the_superseded_load`;
`pane_action_prefills_and_never_submits`; `pane_action_with_confirm_is_refused`;
`pane_load_runs_only_the_read_only_table` (a pane calling `task start --execute` is refused
before any Attempt and its cancellation leaves no Worker; a pane calling `task start --file`
without `--execute` or `task plan` is refused and the Store's event log is byte-identical); the existing
`terminal_data_is_printable_ascii` and the TUI's timing tests unchanged.

### P7 — The runner seam

Deliverables: `af.runner/1`, `af/RunnerRequest@1`, `af/RunnerResult@1`, `af/RunnerIdentity@1`;
`AF_RUNNER_AUTH_DIR`; the external adapter behind `WorkerModelAdapter`; the Provider registry
accepting a runner plugin's kind; `identity`, `status`, `models` and admission through the
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
by `true` that stays incomplete);
`runner_binding_is_captured_verified_and_never_substituted` (a second plugin declaring the
kind is refused at install; `current` moved to other bytes refuses the Attempt; resume under a
changed bundle is refused; replay validates the record with the plugin removed);
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
