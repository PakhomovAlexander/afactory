# Plugins: every feature that is not the kernel, as an executable af finds, runs and pins

**Status:** proposed 2026-10-19. **Baseline:** kernel `main` bf027de6 (v0.12.0-rc.4).
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
2. **A plugin reads the kernel only through `af`.** Its reader is the command line's versioned
   `--json` documents and the `af store` read commands of §3.6. It never opens `events.sqlite`,
   the `attempt_wall` sidecar, the CAS, a Warm Workspace or a Worker sandbox.
3. **A plugin writes into the kernel through exactly two forms**, both captured and pinned:
   a Task file it starts (`af task start --file`), and a file it binds to a declared root input
   port (§3.7). Everything else a plugin writes is its own state.
4. **A plugin never decides.** It cannot refuse, approve, suspend or alter a Task, a Round, a
   plan, a budget, a sandbox, a Worker input or an exit code. A pipeline that wires a
   plugin-bound input gives a Worker data, never authority: such an artifact can satisfy no
   coverage obligation and reach no `accept` or `review_accept` node.
5. **The Store records nothing about plugins.** No event, artifact or sidecar row names a plugin.
   A plugin-bound input is captured like any other input and replays without the plugin
   installed. A removed plugin leaves every recorded Task replayable.
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

The environment a plugin receives, the whole of it, is the invocation protocol:

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

The plugin reads its configuration with `"$AF_BIN" config show --json --repo "$AF_REPO"` and
takes `plugin.<name>`; origins come with `--origin`. The kernel passes no configuration itself,
so a plugin's keys are its own and `af config show` prints them like every other table.

Names are `[a-z][a-z0-9-]{1,31}`. Reserved: the kernel's namespaces (`review`, `task`,
`catalog`, `provider`, `onboard`, `config`, `self`, `storage`, `completions`, `help`) and the
two this note adds (`plugin`, `store`). A manifest naming one is refused at install.

`af --help` lists installed plugins under `Plugins:` with their summaries, read from the
manifests at `$XDG_DATA_HOME/af/plugins/*/current/plugin.toml`; the filesystem is the record,
as it is for `af self`, and there is no registry file. Twenty manifests read in under a
millisecond; `af --help` keeps its 50 ms bound (value 3). `PATH` is never scanned for help.

Dispatch is unchanged: `af <plugin>` inside a pinned project first execs the release the lock
pins, which then runs the plugin (ADR-0044); `af plugin …` is machine-local and exempt, like
the binary-management `self` commands. A pinned release older than this design answers `af
<plugin>` with its usage error, the same way it answers any command it does not have.

### 3.4 The manifest

```toml
schema = "af.plugin/1"
name = "backlog"
version = "0.3.0"
summary = "Order work items and run them as Tasks"
protocol = 1
min_af = "0.13.0"
bin = "bin/af-backlog"

# Optional. Omitted means: no hooks, no table, no storage, no pane, no packages.
hooks = ["task.finished", "task.delivered"]
hook_mode = "detached"            # or "sync"; hook_wall_ms (sync only) at most 60000
config = "backlog"                # the [plugin.backlog] table; machine_only = true allowed
storage = "state"                 # none | cache | state (state implies cache)
pane = true                       # `af backlog pane --json` exists
packages = ["packages/*"]         # Worker, Pipeline and kind packages shipped for projects
```

`min_af` is checked against the running release before the plugin runs; `af <name>` under an
older release prints the manifest's requirement and the install command. The schema is
`schemas/plugin-v1.json` with the parity tests every shape gets; unknown keys are refused.

### 3.5 Installing, removing, pinning

```text
$XDG_DATA_HOME/af/plugins/<name>/<version>/plugin.toml      the manifest
$XDG_DATA_HOME/af/plugins/<name>/<version>/bin/af-<name>    the executable
$XDG_DATA_HOME/af/plugins/<name>/<version>/packages/        shipped packages, when any
$XDG_DATA_HOME/af/plugins/<name>/<version>/receipt.toml     from where, verified how
$XDG_DATA_HOME/af/plugins/<name>/current -> <version>       the active version; the link is the record
$XDG_STATE_HOME/af/plugins/<name>/                          state (§3.9)
$XDG_CACHE_HOME/af/plugins/<name>/                          cache (§3.9)
```

| Command | Does |
|---|---|
| `af plugin install SOURCE [--version V] [--key FILE]` | installs from a release (`owner/repo`, GitHub through `gh` as `af self` does), a local archive, or a directory; verifies and records a receipt; makes it current |
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

Removal is complete by construction: nothing in the kernel references a plugin, no Store names
one (§3.1 clause 5), and `.af/plugins/<name>/` is the project's to delete. An experiment ends
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
| `af store events --state DIR --run ID [--from SEQ] [--type T]` | the event rows in sequence order, one `RunEvent@1` per line, payloads verbatim; a row of a type this release does not know is printed with `"unknown_type": true` and never interpreted | `review.kernel/RunEvent@1`, JSONL |
| `af store attempts --state DIR --run ID` | the `attempt_wall` sidecar rows: Attempt, node, round, epoch, start, elapsed, usage | `af/store-attempts@1` |
| `af store artifact --state DIR ID [--payload]` | one artifact by ID, its envelope and payload verified on read, as the CAS reads it | the artifact's own type |

These five commands are the whole generic read API, and they are the reason no later feature
needs a `--json` document added for its own sake: a plugin that needs a fact the projections do
not carry reads the rows and derives it. Printing rows is inspection, not replay: no payload is
decoded, no projection is built, and ADR-0145's refusal of another release's state stays where
it is, in replay.

### 3.7 Writing into the kernel

**Task files.** A plugin starts work exactly as a person does: it writes an `af.task-file/1`
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

At plan time the Task-file adapter reads the file (at most 16 MiB, the package bound), requires
canonical JSON, requires `type` to equal the port's declared `artifact_type`, validates the
payload against the schema the selected Worker package carries for that port when it carries
one, publishes it into the Task Store's CAS with the producer `kernel_operation` and the
operation `task_file_input`, pins its artifact ID in the compiled plan, and lists it in the
Attempt's context manifest with its bytes and estimated tokens, the way every port is listed.
Resume, retry and replay never read the file again (ADR-0117). The port must be declared by the
selected Pipeline and admitted by the Worker package's `[signature.contract.inputs]` and
`input.schema.json`, so a reviewed, locked pipeline decides which Workers see a plugin's data
and a Worker that does not wire the port sees nothing (value 2). `requirements`, `base` and
`continuation` stay refused by name, and a file-bound artifact can cover no obligation (§3.1
clause 4). The plan preview and `af task explain` print the port, the type, the bytes and the
file's digest.

This is the amendment the Project note's Brief (`af/ProjectBrief@1` on a `project` port)
needs to become a plugin's output, and it is the whole of what a plugin can put in front of a
Worker.

### 3.8 Hooks

| Hook | After | The document carries |
|---|---|---|
| `task.finished` | `af task run` or `af task start --execute` ended, whatever the outcome, interruption included | `task_id`, `store` |
| `task.delivered` | `af task deliver` created the worktree | `task_id`, `store`, `worktree`, `branch` |
| `review.round.closed` | `af review run` (or bare `af review`) ended | `campaign`, `store`, `run_id` |
| `onboard.applied` | `af onboard --apply` wrote `.af/` | `repo` |
| `storage.sweep` | the sweep over the budget, after the kernel evicted what it owns | `target_bytes` |

The document on stdin is `af/plugin-hook@1`: `schema`, `hook`, `af_version`, `repo`, `argv`,
`exit_code`, `occurred_unix_ms`, and the identifiers above. It names what finished; it does not
carry the result, because the plugin reads that with `"$AF_BIN" task show ID --state DIR --json`
or its review counterpart, so no command has to hand its document to the kernel's hook site.
The environment is §3.3's plus `AF_PLUGIN_HOOK`.

Hooks run after the command's result is written and flushed, after the Storage Budget sweep,
and before the update check's detached child, in plugin-name order. `detached` (the default)
spawns the plugin in its own process group with stdio null, as `self refresh-check` is spawned,
and waits for nothing. `sync` waits up to `hook_wall_ms` with stderr inherited and stdout null;
a plugin still running then is killed with its group and reported. Neither mode changes stdout,
the exit code or any `--json` document; a hook that cannot start or exits non-zero is one line
on stderr: `af: plugin backlog: task.finished hook exited 3`. A plugin with
`[plugin.<name>] enabled = false` in the effective configuration runs no hook and no command.

Hooks are notifications, not an event bus: there is no subscription, no replay and no
persistence in the kernel. A plugin that wants to see what it missed reads `af store runs`.

### 3.9 State, cache and the budget

A plugin declaring `storage = "state"` owns `$XDG_STATE_HOME/af/plugins/<name>/` and
`$XDG_CACHE_HOME/af/plugins/<name>/`; `"cache"` owns the second only; `"none"` neither, and a
plugin writing there anyway is a bug `af plugin check` catches with a fixture run. The kernel
counts three storage kinds per plugin, `plugin-install:<name>`, `plugin-state:<name>` and
`plugin-cache:<name>`, in `af storage` and the budget (ADR-0144). Cache directories are entries
the kernel evicts whole, least recently used first, never within an hour of use. State is never
evicted by the kernel: when the budget is still exceeded after the kernel's own eviction, the
sweep runs each enabled plugin's `storage.sweep` hook synchronously with `target_bytes`, and
whatever remains is the operator's with `af plugin remove --purge`. Installed versions other than
`current` and the pinned ones are evicted like other installed versions.

A plugin never reads or writes another plugin's directories; composition between plugins is
through their command lines, like everything else.

### 3.10 Configuration

`[plugin.<name>]` is one table per plugin in the ladder, deep-merged across layers, reported
with origins, and otherwise opaque to the kernel. One key is reserved: `enabled` (default
`true`). A manifest with `machine_only = true` joins `[self]` and `[storage]` in the
`MACHINE_ONLY` list for that name, so a directory, project or local layer carrying it is
reported and ignored; the telemetry note's `[telemetry]` is such a table. The environment form
is `AF_PLUGIN_<NAME>__<KEY>`, under the existing `AF_<TABLE>__<KEY>` rule.

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
absent-only, previewed, never credentials, never state (ADR-0032). The kernel cannot enforce
that on a plugin; `af plugin check` runs the plugin's `init`, when it has one, in a fixture
repository and fails on any other write under `.af/` or any write outside it.

Worker, Pipeline and kind packages a plugin ships are pinned by a project exactly as the
kernel's own are: a `path` under `.af/plugins/<name>/packages/` in `task-catalog.toml` or a
sync from the plugin's repository into `vendor/`, digests in the lock, replacement rules
unchanged (ADR-0052, ADR-0053). The kernel knows nothing about who wrote a package.

### 3.12 A pane in the browser

A plugin with `pane = true` answers `af <name> pane [--item ID] --json` with `af/plugin-pane@1`:
`title`, `items` (the bar entries under the plugin's folder: `id`, `label`, nested `children`),
`rows` (`text`, optional `tone` from the design system's chip set: `run`, `pass`, `fail`),
`legend`, `status` and `actions` (a key to an `af` command line). The bar gets one folder
`plugins/` with a child per such plugin; one generic renderer paints every plugin pane, loads it
on a thread like the other panes, reloads on `R`, runs an action as the `:` line runs a command
(ADR-0123), and shows nothing a document does not carry. The plugin's pane is a projection of
its own `--json` commands, by the TUI's own rule. No plugin can add a key the browser does not
have or paint outside its rows.

### 3.13 Pins

`.af/af.lock` gains `[plugins.<name>]` with `version` and `[plugins.<name>.digests]` per target,
written by `af plugin pin` and by `af onboard --refresh-lock` for every plugin the project
requires. Inside such a project `af <name>` runs the pinned version, installing it on demand
when the archive it downloads matches the lock's digest for this target, under the rules a pin
of af itself follows: a pin without a digest for this target is never installed on demand,
`AF_SELF_OFFLINE=1` or `[self] install_pins = false` refuses with the command to run, an
installed copy must carry the digest in its receipt, and a `dev` or `PATH` plugin never
satisfies a pin. `AF_PLUGIN_VERSION_<NAME>` overrides for one invocation. The pinned version is
what a plugin's hooks run as well. A Task's replay does not depend on any of this (clause 5);
pins exist so that a team runs the same plugin, which is what sharability asks.

### 3.14 The runner seam

The second implementation a Provider kind still lacks is an external one. A runner plugin is a
plugin whose manifest declares `runner = "gemini"` (one kind per plugin) and whose executable
speaks `af.runner/1` through three verbs, so the Provider registry's `kind` may name it:

| Verb | Request | Answer |
|---|---|---|
| `status --json` | — | authenticated or not, account label, the one command a human runs at a terminal to log in (ADR-0112: the plugin never starts a login) |
| `models --json` | — | the model catalog with effort levels, for ADR-0148's validation |
| `invoke` | `af/RunnerRequest@1` on stdin: the exact input bytes the kernel composed, the sandbox directory, the access class (`ReadOnly`, `ExecuteChecks`, `WriteSource`, `WriteSourceWithShell`), model, effort, deadline, the sandbox-local environment | `af/RunnerResult@1` on stdout: the output bytes, the model that answered, usage counters in `af/TaskTokenUsage@3`'s shape or `unknown` with a cause (ADR-0143), and a failure class from the kernel's own list |

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
§3.1 against the Store. `af plugin check DIR` runs the portable subset of the same checks
against any plugin: manifest valid, `--help` exits 0 within 100 ms, environment honoured, hook
document parsed, no write outside the declared directories, no Store file opened (watched
through the fixture's own strace-free method: the Store directory is made unreadable for the
run). A plugin template repository carries the fixture's structure for a new plugin.

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
| Telemetry | T1's substrate and segment writer, T2, T3's observer and taps, T5's probe and receiver, the `[telemetry]` table as a machine-only plugin table | `af-telemetry`: T4's analytics, exports and sinks, T6's pane, all reading the documented segment format and never the Store |
| Project | the Project ID derivation, `--workspace` on the Task and review commands (they resolve `--state`) | `af-project`: Surveys, the Surveyor as a `task.finished` hook, Briefs rendered to a file and bound to the `project` port by whoever starts the Task, `activity` over `af store` |

The whole kernel surface toward plugins, which `af help plugins` renders:

| Direction | Contract | Shape |
|---|---|---|
| kernel to plugin | the invocation environment of §3.3 | protocol `1` |
| kernel to plugin | the hook document of §3.8 | `af/plugin-hook@1` |
| kernel reads | the manifest of §3.4 | `af.plugin/1` |
| plugin reads | every `--json` document the CLI prints | each its own `kind@version` |
| plugin reads | `af store list`, `runs`, `events`, `attempts`, `artifact` | `af/store-*@1`, `RunEvent@1` |
| plugin writes | a Task file | `af.task-file/1` |
| plugin writes | a file-bound root input | the `file` form of `inputs` |
| plugin answers | a pane | `af/plugin-pane@1` |
| project declares | `[plugins] required`, `[plugin.<name>]`, `.af/plugins/<name>/`, `[plugins.<name>]` in the lock | TOML, as every declaration |

Call sites in existing files, the complete list: `cli.rs` (the `external_subcommand` variant,
the `plugin` and `store` namespaces); `main.rs` (one arm each for the two namespaces, one call
to `plugins::after_command`); `topics.rs` (one topic, rendered from the surface table);
`layout.rs` (one entry); `config.rs` (the `[plugin.*]` passthrough and manifest-declared
machine-only names); `project.rs` (one opaque field, one table); `storage.rs` (three kinds);
`selfmgmt.rs` (the `plugin` namespace in `exempt_from_dispatch`); `lock.rs` (one table);
`tui/mod.rs` (one folder, one pane); the Task-file adapter (one binding form). New code, by
package, is estimated at 3,500 lines including tests, most of it `plugins/` reusing
`selfmgmt`'s install, receipt and verification code. After P4 a feature like `af backlog` is
zero lines in this repository.

### 3.17 The three examples

**`af analytics`.** A `command` plugin with `storage = "state"`, hooks `task.finished`,
`review.round.closed` and `storage.sweep`, a machine-only table `[plugin.analytics]`, and a
pane. On each hook it reads the finished Task or Campaign through `af task show --json`,
`af review report --json` and `af store attempts`, and folds them into its own index under its
state directory; `af analytics spend --since 30d`, `failures`, `flakes`, `workers` and
`projects` answer from the index; `af analytics pane --json` shows the last runs. It enumerates
Stores through `af store list`, so work spread over many Stores and Workspaces is one answer.
Nothing it records is read by the kernel; `af plugin remove analytics --purge` leaves no trace.
Once the telemetry capture lands in the kernel, the same plugin reads the segments too and
becomes the `af-telemetry` reader side.

**`af backlog`.** A `command` plugin with `storage = "state"` and the hook `task.finished`. It
holds an ordered queue of work items from the sources it is told about: GitHub issues through
`gh`, a Jira filter, a directory of Markdown files, a hand-written list. `af backlog add`, `rank`,
`next`, `status`. `af backlog run --parallel 2` writes one `af.task-file/1` per item under its
state directory, binding an issue through the kernel's own issue sources (ADR-0061) and any
Brief or evidence through the `file` form, and starts it with `"$AF_BIN" task start --file
--execute`; the `task.finished` hook moves the item to done, failed or needs-human and starts
the next, each start being an ordinary Task with its own plan, budget and approval. It never
approves a plan (ADR-0056), never delivers, never pushes. Its `pane` lists the queue with the
state chips.

**`af reviewer`.** A plugin that ships reviewer Worker packages under `packages/` (a
`worker.toml` with `[runner] kind = "model"` or `"command"`, instructions, schemas), and a
command `af reviewer run --pr N` that composes a review Task file for the pull request and
starts it, reading the outcome with `af review report --json`. A project adopts the packages by
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
- **Deterministic.** The Store records nothing about plugins; plugin data enters a Task only
  as a pinned artifact; replay never runs a plugin.
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

Deliverables: `crates/af/src/plugins/` (manifest, directory layout, resolution, invoke);
clap's `external_subcommand`; `af plugin install|remove|list|show|check` for a directory, a local
archive and `--link`; the `Plugins:` section of `af --help`; `af help <plugin>`; `af help
plugins` rendered from the surface table; `[plugin.*]` passthrough and manifest-declared
machine-only names; the `plugins/` `LAYOUT` entry; `ProjectFile`'s field and `[plugins]
required`; `fixtures/plugins/echo/`; the dispatch exemption for `af plugin`. ADR: P0's.

Acceptance: `plugin_runs_with_the_declared_environment_and_passes_exit_codes` (0, 3, and a
signal); `plugin_on_path_runs_as_dev_and_off_under_path_false`; `unknown_word_is_still_exit_2`;
`help_lists_installed_plugins_without_scanning_path` (`PATH` set to a directory that would
fail if read); `help_stays_under_50ms_with_twenty_manifests`; `reserved_names_are_refused`;
`manifest_protocol_and_min_af_are_checked_before_exec`;
`plugin_table_merges_with_origins_and_machine_only_is_honoured`;
`layout_declares_plugins_directory_and_drift_test_passes`;
`surface_table_matches_the_code` (every `AF_PLUGIN_*` variable in `plugins/` is in the table
and nothing else is set); `plugin_check_catches_a_write_outside_declared_directories`.

### P2 — `af store`

Deliverables: `list`, `runs`, `events`, `attempts`, `artifact` with their documents. No new ADR;
an amendment to ADR-0145 stating that listing rows is inspection, not replay.

Acceptance: `store_events_prints_every_row_in_sequence_with_payloads_verbatim` (compared to the
SQLite rows); `store_events_marks_an_unknown_type_and_decodes_nothing`;
`store_artifact_verifies_envelope_and_payload_identity` (a corrupted object is refused);
`store_list_names_every_store_the_sweep_would_visit`; `store_reads_never_take_a_writer_lease`
(a running Task's Store stays writable); `store_read_latency_under_200ms_on_10k_events`.

### P3 — File-bound inputs

Deliverables: the `file` form in `af.task-file/1` and `task-input-bindings`; capture, type
check, schema check, publication, pinning, manifest entry, preview and explain lines. ADR:
an amendment to ADR-0134.

Acceptance: `file_bound_port_reaches_only_the_worker_that_wires_it`;
`file_bound_type_must_equal_the_port_type`; `file_bound_payload_fails_the_package_schema`;
`file_bound_input_replays_after_the_file_is_deleted`;
`file_bound_input_cannot_cover_an_obligation` (a pipeline wiring it into `coverage` is refused
at load); `refused_ports_stay_refused` (`requirements`, `base`, `continuation`);
`file_over_16mib_is_refused_before_capture`.

### P4 — Hooks, state, cache, budget

Deliverables: `plugins::after_command` and its one call site; the five hooks and
`af/plugin-hook@1`; `detached` and `sync` modes with the wall bound; `enabled`; the three storage
kinds and the `storage.sweep` hook. ADR: an amendment to ADR-0144 for the kinds.

Acceptance: `hook_runs_after_the_result_and_changes_no_output` (stdout, `--json` document and
exit code byte-identical with and without a failing hook);
`hooks_run_in_name_order_and_a_sync_hook_is_killed_at_its_wall`;
`detached_hook_outlives_the_command_and_owns_its_group`; `disabled_plugin_runs_nothing`;
`store_records_nothing_about_a_hook` (event log byte-identical with hooks on and off);
`plugin_cache_is_evicted_whole_and_state_never_by_the_kernel`;
`storage_sweep_hook_receives_target_bytes_and_cannot_block_the_sweep`.

### P5 — Pins and release sources

Deliverables: `[plugins.<name>]` in the lock; `af plugin pin`; `af onboard --refresh-lock` for
required plugins; GitHub and directory release sources; receipts with `verified_by`; the key
rule; install on demand; `AF_PLUGIN_VERSION_<NAME>`. ADR: an amendment to ADR-0045.

Acceptance: `pinned_plugin_runs_the_pinned_version_and_installs_on_demand_by_digest`;
`pin_without_a_digest_for_this_target_is_never_installed`; `dev_plugin_never_satisfies_a_pin`;
`offline_refuses_with_the_command_to_run`; `signature_mismatch_refuses_install`;
`remove_refuses_a_version_a_seen_project_pins`.

### P6 — The pane

Deliverables: `af/plugin-pane@1`; the `plugins/` folder; the generic renderer; `R`, `Enter`,
actions through the hand-off. No new ADR; an amendment to ADR-0119.

Acceptance: `plugin_pane_paints_only_what_the_document_carries`;
`plugin_pane_loads_off_the_key_loop_and_a_slow_plugin_shows_the_spinner`;
`pane_action_runs_through_the_command_hand_off`; the existing
`terminal_data_is_printable_ascii` and the TUI's timing tests unchanged.

### P7 — The runner seam

Deliverables: `af.runner/1`, `af/RunnerRequest@1`, `af/RunnerResult@1`; the external adapter
behind `WorkerModelAdapter`; the Provider registry accepting a runner plugin's kind; `status`,
`models` and admission through the plugin; one external adapter as the proof. ADR: its own.

Acceptance: `external_runner_is_supervised_cancelled_and_deadlined_by_the_kernel`;
`external_runner_output_is_redacted_and_stored_as_raw_artifacts_before_parsing`;
`external_runner_usage_is_validated_and_unknown_settles_at_zero` (ADR-0143);
`external_runner_is_trusted_unsafe_and_cannot_auto_apply`;
`native_adapters_are_byte_identical_with_the_seam_present`; the Task fixture run through the
external adapter with a command-backed fake model.

After P4 the first-party plugins start, each in its own repository from the template:
`af-backlog` and `af-analytics` first, because they need nothing beyond P4; `af-project` and
`af-telemetry` as their notes are re-planned under §3.16; `af-reviewer` with P7 when a new
Provider kind is wanted.

## 6. Not in v1

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
