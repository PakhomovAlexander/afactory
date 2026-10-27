# Providers

How a machine gets the model access `af review` and `af task` need, and what that flow promises a
caller that is not a human sitting at a terminal.

A **Provider** is an operator-named, machine-local authentication context for one adapter kind
([`../CONTEXT.md`](../CONTEXT.md)). `af` never holds a credential: the official `claude` or `codex`
CLI owns it, and the machine-local registry at `~/.config/af/providers.toml` records only an ID, a
kind, and an auth directory. Bootstrap runs in the invoking release rather than a repository's
pinned one, and registry publication stays atomic for older readers
([ADR-0111](adr/0111-keep-provider-bootstrap-machine-local-and-cross-release-safe.md)).

## In a first run

The [README Quickstart](../README.md#quickstart) registers a Provider in its second step, and
every first-run surface points at it:

- `af provider detect --apply` finds the `claude` and `codex` logins on this machine and registers
  them; without a login yet, `af provider setup claude-main --kind claude --login` (or
  `codex-main --kind codex`) at your own terminal starts one. A successful `setup` or `add` ends
  with `next: af onboard (in a repository)`.
- `af onboard` reads the registry without starting any CLI: it picks `claude` or `codex` Workers
  when only that kind is registered, and its `providers` block names, for every model Worker, a
  registered Provider of its kind and the `--provider NODE=ID` binding to pass, or the command
  that registers one. `--probe` adds each Provider's login state from the status probe.
- `af doctor` lists every registered Provider's login state from the same probe, and the logins
  it found where the CLIs look by default that no registry entry names; it never charges, and
  `af provider doctor` stays the charged end-to-end check.
- `af` alone at a terminal opens on the Providers pane while nothing is registered.

## The default terminal boundary

An interactive Provider login prints an **OAuth URL and an authorization code**. Those are
credentials in transit. Anything that can read them — a pipe, a chat window, an agent transcript, a
CI log — is a place they must never reach through ordinary output. An explicitly permissioned
private host can instead use [the dedicated host protocol](provider-auth-host.md); its human-private
transport is separate from every ordinary command stream.

So `af provider setup` starts an official login only when **both** are true:

1. the operator passed `--login`, and
2. this process owns an interactive terminal on stdin, stdout **and** stderr.

Otherwise setup fails fast with the `human_action_required` result, exit code 3, and the exact
command a human should run at a private terminal. In this default path, `af` does not proxy, broker, or headlessly
complete an OAuth flow, and it never captures, parses, stores or re-emits what that login prints
([ADR-0112](adr/0112-refuse-agent-mediated-provider-logins.md)).

What the terminal check protects against is **accidental capture**: a pipe, a CI job, a chat or
agent transcript, a tool call that runs `af` without a terminal. It cannot tell a human from a
program. A caller that holds a pseudo-terminal (`script`, `expect`, `tmux send-keys`, `ssh -tt`,
a PTY library) passes the check, sees the OAuth URL and can type the code, exactly as a person at
that terminal would; such a caller is out of this boundary's scope, and a test pins that it
reaches the login. An agent that must start a login on someone's behalf uses the private host
protocol below, whose permission and private delivery are explicit capabilities.

## Detect

On a new machine, start here. `af provider detect` finds the official CLIs and every auth
directory on the machine, asks each directory's CLI whether it holds a login, and prints the one
command that registers it; `--apply` runs those commands
([ADR-0206](adr/0206-detect-provider-logins-and-register-them-through-add.md)):

```sh
af provider detect              # what is here; writes nothing
af provider detect --apply      # register every logged-in directory not registered yet
af provider detect --json       # the same scan, as af/provider-detect@3
```

On a machine whose `CLAUDE_CONFIG_DIR` is `~/.config/claude-personal`, with logins in
`~/.config/claude-work` and `~/.codex-acme` as well, `af provider detect` prints:

```text
KIND   AUTH DIR                   SOURCE   LOGIN                  REGISTERED AS    SUGGESTED
claude ~/.config/claude-personal  env      Claude Max (claude.ai) -                claude-personal
claude ~/.config/claude-work      sibling  Claude Max (claude.ai) -                claude-work
codex  ~/.codex-acme              sibling  ChatGPT                -                codex-acme

binaries: claude 2.1.299, codex 0.163.0
  claude ~/bin/claude
  codex ~/bin/codex

next, one line per directory:
  af provider add claude-personal --kind claude --auth-dir ~/.config/claude-personal
  af provider add claude-work --kind claude --auth-dir ~/.config/claude-work
  af provider add codex-acme --kind codex --auth-dir ~/.codex-acme

3 logins are not registered: run af provider detect --apply to register them with the IDs above, or
  copy a line
shell aliases or functions that set CLAUDE_CONFIG_DIR/CODEX_HOME are not read; pass --dir for such
  a directory
```

With a logged-out `~/.claude` beside them, `af provider detect --apply` prints its table, one row
longer, and then:

```text
registered: af provider add claude-personal --kind claude --auth-dir ~/.config/claude-personal
not logged in, so not registered: ~/.claude; run this yourself at a private terminal:
  af provider setup claude-main --kind claude --auth-dir ~/.claude --login
registered: af provider add claude-work --kind claude --auth-dir ~/.config/claude-work
registered: af provider add codex-acme --kind codex --auth-dir ~/.codex-acme
```

A second `--apply` changes nothing and says `already registered: claude-work
(~/.config/claude-work)` for each.

What it looks at:

- **Binaries.** `claude` and `codex` on the sanitized `PATH` first, then in `~/.local/bin`,
  `~/.claude/local` (claude only), `/opt/homebrew/bin`, `/usr/local/bin`, `~/.npm-global/bin`,
  `~/.volta/bin` and `~/.bun/bin`. One found off `PATH` is reported as `found off PATH: <path>`
  with the line that adds its directory to `PATH` for your `$SHELL` (`fish_add_path <dir>` for
  fish, otherwise `export PATH="<dir>:$PATH"`), and is probed through that path. No shell rc file,
  `npm prefix` or `nvm` is evaluated. `gemini`, `cursor-agent`, `opencode`, `amp`, `copilot`,
  `aider`, `qwen` and `goose` found on `PATH` are listed by name and version as `not supported by
  af yet`; none of their directories is scanned.
- **Auth directories**, each with its SOURCE: `env` (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, absolute
  only), `default` (`~/.claude`, `~/.codex`), `sibling` (`~/.claude-*`, `~/.config/claude`,
  `~/.config/claude-*`, `~/.codex-*`, `~/.config/codex`, `~/.config/codex-*`: literal names one
  level deep, no recursion), `registry` (every `auth_dir` already registered) and `explicit`
  (`--dir PATH`, repeatable). A directory found twice, by its canonical path, is listed once,
  with the most specific source: explicit, env, default, sibling, registry. A shell alias or
  function that sets one of the variables is not read: pass `--dir` for such a directory.
- **Markers, by name only.** A Claude directory holds `.claude.json`, `.credentials.json` or
  `settings.json`; the default `~/.claude` may instead have a regular file `~/.claude.json`
  beside it, where the CLI keeps that context's state, and its row's marker is then
  `~/.claude.json` (`af provider status` asks the same predicate). A Codex directory holds
  `auth.json` or `config.toml`. af examines those names
  with `fstatat` below a descriptor opened with `O_NOFOLLOW` and never opens a file under the
  directory. A directory that cannot be listed (one that grants search but not read) has each
  marker checked by its path instead and is probed; its row says `listing unreadable`, never
  `empty`. A directory with none is listed `empty` and not probed, because a CLI asked about a
  directory creates files there (`codex login status` creates its home on first contact);
  `--all` probes it anyway. A directory that is a symlink, is not owned by you or is writable by
  group or others is listed `unsafe (not probed)` with the reason and, for permissions, the
  `chmod` that fixes it: the same rules `setup` and `add` apply. `--dir` needs `--kind` when the
  markers do not say which CLI the directory belongs to.
- **The probe** is `af provider status`'s: the capability check once per binary, then one bounded,
  token-free login check per directory, at most four at a time, in the probe environment, each
  cut at 10 seconds whatever the kind. A row whose login is not known says why: in the table as
  `unavailable: provider status probe timed out after 10 seconds`, and in the document as
  `auth_cause` (`timeout`, `unstartable`, `incompatible`, `unparsable`, `output_too_large`,
  `cancelled`, `unchecked`, `other`) with `auth_detail`, af's own words for it. LOGIN
  shows the plan or method — `Claude Max (claude.ai)`, `ChatGPT`, `API key`, `not logged in` —
  never an email or an organization. The Codex plan is not part of that check's answer, so it
  reads `ChatGPT` and `plan: unknown`. `--identity` adds the account label the CLI reported, under
  each row, only when stdout is a terminal and `--json` is absent.

`--apply` registers, in table order, every row whose LOGIN is authenticated and REGISTERED AS is
`-`, through the same locked, atomic publication and safety checks as `af provider add`, and
prints each line it ran. It never logs in, never creates a directory, and never touches a row
that is not logged in: it prints that row's `af provider setup … --login` line instead. A row
the scan read as registered is checked again under the registry lock: one whose entry was
removed since is registered again, never reported `already registered`. A login whose CLI was
found only off `PATH` is not registered, because `af provider status` and Tasks look on `PATH`
only: its suggestion is the PATH line (`fix_path`), `--apply` leaves it `skipped_cli_off_path`
and exits 4, and the closing line is that PATH line. A registry conflict stops it at that row
with exit 5; the rows before it stay registered. The row names the ID it met
(`applied.conflict_with`) and suggests the retry that works, `af provider detect --dir D --id
OTHER --apply` with the first free ID (`retry_apply`), which the message and the closing `next:`
line repeat. A registry that cannot be read (malformed, unreadable or mid-transaction) makes
every row's `registration` `unknown` and its REGISTERED AS column `unknown`, its note is printed
with every scan, and `--apply` refuses with exit 1 before it scans. The IDs:

| Directory | ID |
|---|---|
| default (`~/.claude`, `~/.codex`) | `claude-main`, `codex-main` |
| sibling | the kind and the directory name without its kind prefix: `~/.config/claude-work` is `claude-work`, `~/.codex-gridgain` is `codex-gridgain`, `~/.claude-personal` is `claude-personal`, `~/.config/claude` is `claude-config` |
| env | the sibling rule when its name is a sibling name, else `claude-env`, `codex-env` |
| explicit | `--id`, in `--dir` order, required when more than one `--dir` names the same kind; else the sibling rule |
| taken | `-2`, `-3`, …; an `*-ambient` ID is never generated |

With nothing found, the output is the install guidance — `npm install -g
@anthropic-ai/claude-code` or `curl -fsSL https://claude.ai/install.sh | bash`, `npm install -g
@openai/codex` or `brew install codex`, the directories searched and `then run: af provider
detect` — under exit 0, with `applied` true in the document when `--apply` was given. The
document carries the same next steps as typed fields: `install`, one `{kind, command, docs}` per
kind whose CLI was not found (the official install command and documentation page), and
`next_action`, the scan to run again once one is installed (`af provider detect` with the scan's
own `--kind`, `--dir`, `--id` and `--all`), null when a CLI was found; `notes` say it to a person. `af
provider status` prints the same guidance when no CLI is on `PATH` and nothing is registered, and
it names what a names-only scan found that no row lists: `found but not registered:
~/.config/claude-work, ~/.codex-acme (not probed) → af provider detect`, which its document
carries as `found_unregistered` with `next_action` `af provider detect`. `af doctor` and `af
onboard` list the same directories from the same scan.

Exit codes: 0 the scan completed, 1 unclassified failure, an `--apply` refused for an unreadable
registry among them, 2 usage error (both as one `af/error@2` document under `--json`), 4 a found
CLI cannot start or is incompatible, or a login was left unregistered because its CLI is off
`PATH` (the rows are still printed), 5 a registry conflict stopped `--apply`. Never 3 and never
6: detect starts no login.

## Onboarding a Provider

Already authenticated with the official CLI? Then no login is needed and none is started:

```sh
af provider setup codex-main --kind codex            # registers an authenticated context
af provider setup codex-main --kind codex --json     # the same, as one JSON document
```

Not authenticated yet? Run this yourself, in your own terminal:

```sh
af provider setup codex-main --kind codex --login
```

`setup` verifies the named auth directory and writes only an ID, the Provider kind and the
auth-directory path to the machine-local registry; credentials remain owned by that CLI. A login
starts from an empty allowlisted environment in an owned auth directory that is not writable by
other users, and one canonical auth context admits only one setup or login at a time. `setup` is
machine-local, so it never dispatches to a repository's older pinned `af`, and registry
publication stays atomic for those older readers. If a crash leaves publication fenced,
`af provider recover` validates the marker plus the live, candidate and prior hashes before
archiving it; it reports which version is live and retains both preserved copies.

`--login` prints the warning above, hands the terminal to the official CLI, verifies the result,
and only then writes the registry entry. It cannot be combined with `--json`: JSON is the
non-interactive automation surface, while login deliberately hands the private terminal to the
Provider CLI. Re-running any of these is safe. `--auth-dir` names an existing directory: only
`--login` creates a missing one, because a login needs a home; without it setup refuses with
`auth directory <PATH> does not exist; pass --login to create it, or check the path`. setup's
lock lives in af's state, `$XDG_STATE_HOME/af/locks/<kind>-<sha256 of the canonical auth
directory>`, never in the auth directory.

`af provider add` registers an already-authenticated context and never offers a login at all. It
probes the directory first, as `status` does, and refuses a context that is not authenticated
with exit 6, a message naming the directory and the `af provider setup <id> --kind <kind>
--auth-dir <dir> --login` line that logs in there; `--unverified` registers it without the probe
and prints `warning: not verified here`.

Generic automation should run `setup` **without** `--login` and respect the terminal-only refusal.
An assistant with an authorized private host integration should own browser-only setup using
[`af provider auth`](provider-auth-host.md), rather than ask its user to obtain SSH or terminal
access. This explicit path also reauthenticates a stale context even if local status says logged in.
Without the private host capability, report that narrow integration blocker; never publish raw
OAuth output to a shared conversation. Authentication completion remains `authenticated_unverified`.

`provider auth begin/status/cancel` are machine-local, have a separate versioned non-secret status
schema, and never invoke model verification or resume Tasks. See the host contract for lifecycle,
expiry, cancellation, supported providers, and the remaining runtime scope of issue #122.

## Temporarily disabling a Provider

```sh
af provider disable claude-personal --for 2d
af provider status
af provider enable claude-personal
```

Disable requires a registered ID. Enable also accepts an ID still present in disable state after
registry removal, reclaiming that entry's capacity; an ID in neither place is refused. Neither
command starts a Provider process or spends tokens.
`--for` accepts a positive integer followed by `s`, `m`, `h` or `d`, up to `30d`; `2d` is exactly
48 hours. Repeating disable replaces the deadline. Enable restores a registered Provider's
availability immediately and is idempotent while the ID remains registered. Clearing an orphan
leaves its ID unknown, so a second enable is refused. Both commands run in the invoking release
as machine-local bootstrap operations.
Execution, status and model discovery keep their existing project pin dispatch: an older pinned
release does **not** honor suspension. Use a pin containing this feature for the work it controls.

Suspensions live separately from registry v1 in
`$XDG_CONFIG_HOME/af/provider-disables-v1.json` (default
`~/.config/af/provider-disables-v1.json`), independent of `AF_PROVIDERS_FILE`. They do not edit the
registry, auth directories or credentials. The file is private, versioned, bounded to 32 entries
and 64 KiB, and atomically published under a lock. IDs use the registry's validation without a
separate length cap. Malformed or unsafe state refuses Provider work. Correct the reported
path or permissions before retrying; enable deliberately refuses to overwrite malformed state.

Each new lookup compares the current wall clock to the stored absolute Unix timestamp, including
nanoseconds: the Provider is available at the expiry instant, across restarts and downtime.
Changing the system clock changes the remaining suspension. No background process is needed.
Status shows a disabled row, the RFC3339 UTC expiry and `af provider enable ID` without probing
that Provider, even with `--usage`. JSON is `af/provider-status@3`, with `disabled_until` and `enable_command`
(non-null only while disabled), `auth = "not_probed"` and `usability = "unusable"`.

Review and model preflights, Task admission and every captured or resumed Task private send
recheck suspension before account/model probes. A refusal names the expiry and enable command.
Calls admitted before a concurrent disable may finish; this command does not kill them or revoke
bytes already sent. The control is per registered ID; another separately registered Provider has
its own availability. See [ADR-0199](adr/0199-disable-providers-until-a-machine-local-expiry.md).

## Tuning Worker independence

The repository's authorized bootstrap currently uses model diversity with
`distinct_principals = false` and `distinct_models = true`. This policy has no expiry: it remains
until an operator changes the trusted catalog. Distinct Worker packages and fresh isolated
sessions remain required. Local bindings select already-admitted packages and Provider aliases;
they cannot relax trusted independence policy.

The existing `[independence]` table in the trusted `.af/task-catalog.toml` controls each axis.
To require different native account principals, set:

```toml
[independence]
command_workers_by_package = true
distinct_principals = true
distinct_providers = false
distinct_models = false
```

To choose model diversity on one account, use:

```toml
[independence]
command_workers_by_package = true
distinct_principals = false
distinct_providers = false
distinct_models = true
```

`distinct_providers` compares Provider kinds (adapter families, such as `claude` versus `codex`);
two registered IDs of the same kind do not satisfy it. Different IDs alone do not prove different
principals. Setting both principal and model booleans to true requires both. Change this trusted
table through the project's implementation and verification Tasks, review and commit the catalog,
and bind independent slots to the appropriate registered accounts or exact model packages.
Inspect the new capture before executing it, for example:

```sh
af provider status
af task plan --file ticket.json --bindings local-bindings.toml --json
af task start --file ticket.json --bindings local-bindings.toml
```

Inspect the captured hierarchy with `af task explain TASK_ID --tree`, then use the plan ID
printed by `task start` to execute that exact plan with
`af task run TASK_ID --confirm-plan PLAN_ID`. Existing Tasks retain captured policy and bindings;
create a new Task to adopt a changed catalog. Provider enable/expiry restores access only; it
never changes a captured independence decision. See [local bindings](task-execution/local-bindings.md).

## Removing a Provider

```sh
af provider remove codex-main                # drop one registry entry
af provider remove claude-work codex-work    # drop several in one publication
```

`remove` deletes registry entries and nothing else. Each auth directory, and the login its
Provider CLI keeps there, is left untouched, so `af provider add` or `setup` registers the context
again. The registry is rewritten through the same locked, atomic publication `add` uses, and the
version it replaced is preserved; the command prints where
([ADR-0136](adr/0136-remove-a-registered-provider-by-id.md)). A comment directly above a removed
entry goes with it.

The command is all or nothing. An ID that is not registered refuses it with exit 1 and removes
nothing. `af` does not check whether a pipeline, Campaign or Task still binds a removed ID.

Two cases come up often:

- **A deleted auth directory.** One entry whose auth directory no longer exists makes the whole
  registry invalid: `status` warns and lists no registered Provider. Removing that entry is the
  repair. When several are stale, name them all in one command, because `af` publishes only a
  registry that is valid.
- **An ambient ID.** `claude-ambient` and `codex-ambient` are discovery labels, not registry
  entries, so there is nothing to remove. One is listed while the directory the Provider CLI uses
  by default (`CLAUDE_CONFIG_DIR` or `~/.claude`; `CODEX_HOME` or `~/.codex`) is not a registered
  Provider's auth directory. Once a Provider of its kind is registered, it is left out when its
  status is `not authenticated` or `not probed` (no such directory, or no login marker in it);
  with a login, or when the status probe fails (`unavailable`, `installation failed`), it stays
  listed ([ADR-0141](adr/0141-list-a-logged-out-default-context-only-until-its-kind-is-registered.md)).
  Start `af` with that variable naming a registered directory, or register the default
  directory, and the label goes away.

In the browser, `d` on a registered Provider fills the `:` line with `provider remove ID`, and
Enter runs it.

## Checking a Provider

```sh
af provider status                 # fast: the registry, the CLI capability check and one authentication check per context
af provider status --json          # the same, as af/provider-status@3
af provider status --usage         # additionally probe subscription and quota windows
af provider doctor --campaign NAME --provider correctness=codex-main
```

`status` is deliberately cheap and says only what it checked. A context that does not exist or
holds none of its CLI's login markers (see [Detect](#detect)) is not probed at all, because the
CLI would create files there; its row says `not probed` and why. Each row without a login names
one fix, `af provider setup <id> --kind <kind> [--auth-dir <dir>] --login`; `--verbose` adds the
CLI's own login command (`CODEX_HOME=<dir> codex login`) under it. The `--usage` hint is printed
only when a row is authenticated. It reports four independent axes:

| Axis | Values | Means |
|---|---|---|
| `registered` | true / false | in the registry, so selectable by `--provider NODE=ID`; false is an ambient discovery label |
| `auth` | `authenticated`, `not_authenticated`, `unknown`, `unavailable`, `installation_failed`, `incompatible`, `not_probed` | what the Provider CLI said about this auth directory; `installation_failed` means the CLI cannot start, so it said nothing; `incompatible` means it starts but does not accept af's invocation, so its login was not asked about; `not_probed` means the directory is absent or holds no login marker, so the CLI was not asked |
| `usability` | `usable_or_untested`, `unusable`, `unknown` | status never claims `usable`: only `af provider doctor` dispatches and can prove it |
| `usage.state` | `not_requested`, `not_applicable`, `unsupported`, `available`, `unavailable` | the optional plan and quota axis |

A quota window whose reset time the Provider does not report says so in its name, `Claude does
not report when this window resets`, and its document window has `resets_at_unix: null` with
`reset_reported: false`.

Usage is separate on purpose. A subscription probe can time out, be unsupported for an API-key
context, or be missing entirely — none of which says anything about whether the Provider is
authenticated. An unanswered probe leaves `auth` and `usability` exactly where they were and exits
7. `af provider doctor` remains the charged, explicit end-to-end check, and is never implicit.

`af provider doctor` and Task Provider admission send the bound model one fixed line on stdin,
from a fresh empty directory, with no file to read and no tool to use: `This is a Provider
admission check by the af review tool: it only verifies that this account can run the bound
model. No file needs to be read and no tool needs to be used. Reply with exactly: OK`. Only the
reply `OK` passes. The line states its purpose because Claude Fable 5.1's safety classifier
refused the bare `Reply with exactly: OK` every time it was sent from an empty directory; a
refusal that happens anyway is still `model_refused`
([ADR-0147](adr/0147-stop-the-claude-model-fallback-and-name-the-model-that-answered.md)).

## Models and effort levels

```sh
af provider models                                        # every registered Provider's catalog
af provider models claude-work --json                     # the same, as af/provider-models@1
af provider models claude-work --resolve 'Fable 5.1/xhigh'
af provider models codex-main --resolve 'GPT-6 Sol/ultra'
```

`af provider models` lists, for every registered Provider or the named ones, the model catalog
its harness offers, read through the harness's own local protocol under that Provider's auth
directory, in the bounded probe environment `af provider status` uses. It spends no model
tokens and reads no credential:

| Kind | Source | Freshness |
|---|---|---|
| `claude` | `claude list_models`: the `list_models` and `get_binary_version` control requests of a `claude -p --input-format stream-json --output-format stream-json --verbose` session, which af closes once both are answered, before any turn | `live` |
| `codex` | `codex app-server model/list`, after `initialize` and `initialized` on a bare `codex app-server`; the version from `codex --version` | `fetched_at` of `models_cache.json` under the auth directory when it is readable, else `live` |

Each Provider gets a header with the source, the harness version and the freshness, one row per
exact ID with its display name, the aliases that resolve to it and its effort levels (the
default marked `*`), and one sentence saying what the list proves: af recognises these spellings
for this Provider. The list is what the harness offers, not every ID its API accepts: Claude
Code's catalog is the account's `/model` picker, which leaves out exact IDs such as
`claude-opus-4-8` that the API still accepts. Whether this account can run a model is proven
only by the charged `af provider doctor`.

`--resolve SPELLING[/EFFORT]` reads an alias, a display name (case-insensitively), an exact ID
or a family and version such as `Fable 5.1` or `GPT-6 Sol`, and says what it is: an exact ID
the catalog lists (confirmed), an alias of an exact ID (af binds exact IDs, never the alias), a
display name or family form of an exact ID, a misspelling with the nearest catalog IDs (the
same family or at most two edits away), or an exact ID the catalog does not list, which passes
or fails af's spelling rule and whose usability only `af provider doctor` proves. It then checks
the effort level against that model's levels and, for a listed pair only, prints the three
configurations to copy. Each is printed under its label, one table header or key per line, and
is a TOML document that parses on its own and pastes unchanged into the file it is named for:
the review package's `[runner]` table for `reviewer.toml`, the Task package's `[runner]` table
for `worker.toml`, and the `[providers]` table of the Task catalog or a `--bindings` file, with
the review-time equivalent as a comment line. `af provider models claude-work --resolve
'Fable 5.1/xhigh'` prints:

```text
    reviewer.toml  [runner]
                   program = "claude"
                   args = [{ value = "--model" }, { value = "claude-fable-5-1" }, { value = "--effort" }, { value = "xhigh" }]
    worker.toml    [runner]
                   kind = "model"
                   provider_kind = "claude"
                   model = "claude-fable-5-1"
                   effort = "xhigh"
    bindings       [providers]
                   "PACKAGE" = "claude-work"
                   # af review run … --provider NODE=claude-work
```

A Codex reviewer names `program = "codex"` and passes the effort level as `{ value = "-c" },
{ value = "model_reasoning_effort=\"ultra\"" }` in place of the last two args. With `--json`,
`configuration.reviewer_runner`, `configuration.worker_runner` and `configuration.binding` each
carry the text of one block, its lines joined by newlines. af never substitutes a model or an
effort level, so an alias or an effort level the model does not list prints no configuration.

In `af tui`, the Providers pane shows under each Provider a `models` section with that
Provider's own catalog. A Worker declaration names only a Provider kind, never the Provider that
will be bound, so the Workers pane's `runner` row gives the verdict once per Provider of that
kind whose catalog was read, each named by its ID in inventory order (`verdict claude-personal:
ok  claude-work: effort not listed: low, medium, high`), and `verdict (static rules): …` from the
static rules alone when no Provider of that kind has a catalog.

Every binding is checked against the same rules before anything is charged
([model bindings](task-execution/model-bindings.md)): grammar G, an alias Claude Code documents
(`fable`, `opus`, `sonnet`, `haiku`, refused with `claude-fable-5-1`, `claude-opus-5-5`,
`claude-sonnet-5-5`, `claude-haiku-5-5`), a case or separator variant of a known exact ID, the
versioned-ID rule, and the effort levels of the kind (Claude `low medium high xhigh max`, Codex
`low medium high xhigh max ultra`), then, at plan time, the catalog. The command exits 0 when
every catalog was listed, 4 when a Provider CLI cannot start or is incompatible, and 7 when a
catalog is unavailable; then it prints these static rules instead of the rows and invents
nothing ([ADR-0148](adr/0148-discover-provider-models-and-validate-every-binding-before-a-charge.md)).

## A Provider CLI that cannot start

An update can leave the official CLI unable to start at all. One npm auto-update left `codex`
without its platform package, and every invocation failed at once with the CLI's own error
([issue #136](https://github.com/PakhomovAlexander/afactory/issues/136)):

```text
Error: Missing optional dependency @openai/codex-darwin-arm64. Reinstall Codex: npm install -g @openai/codex@latest
```

`af` reports this as a **Provider installation failure**, never as a login, credential or model
problem ([ADR-0130](adr/0130-report-a-provider-cli-that-cannot-start.md)). A CLI cannot start when
any of these is true:

- the program is not on PATH;
- a file of that name is on PATH but is not executable;
- the operating system refuses to start it;
- it exits non-zero on its own `--version` check, before doing any provider work.

`af` runs the version check only when the CLI's own status or account probe got no answer it
recognizes. A CLI that answered, even to say "not logged in", has started. A probe that timed out
or was cancelled proves nothing, so neither triggers the check. The report names the Provider ID,
the program path, the CLI's own first error line and the fix it suggests, for example:

```text
af provider status: provider codex-main: Provider installation failure: the codex CLI `/opt/homebrew/bin/codex` exited 1 on its own `--version` check before doing any provider work: Error: Missing optional dependency @openai/codex-darwin-arm64. Reinstall Codex: npm install -g @openai/codex@latest; fix: Reinstall Codex: npm install -g @openai/codex@latest
```

| Where | What happens |
|---|---|
| `af provider status` | The context's `auth` is `installation_failed` and its usability `unusable`. The command exits 4, which outranks 7. stderr carries one line per broken Provider, and the table's note repeats it. |
| `af provider setup` | `provider_cli_missing`, exit 4, instead of `authentication_failed`. The document's `diagnostic` is the af-authored summary; the CLI's words go to stderr only. |
| Task Provider admission (`af task plan`, `af task run`, `af review run`, `af provider doctor`) | Refused before any Worker is dispatched and before any Attempt is reserved or charged. The error begins `Task Provider admission refused before any Worker was dispatched`, under the usual exit 1. |
| A Worker Attempt after admission | The identity recheck before the private send finds the CLI cannot start. The Attempt fails with zero charge and the diagnostic `Provider environment failure, not a model or credential failure: …`, never as a changed account or a failed model reply. |

The CLI's error line and suggested fix are Provider-authored text. They are bounded to one line
each, with terminal escapes and control characters removed. They appear on stderr, in the human
table and in Attempt diagnostics, and never in a versioned JSON document or in retry feedback.
Fix the installation with the suggested command, then rerun `af provider status`.

## Supported CLI versions and required capabilities

Authentication success never implies invocation compatibility. A machine with Claude Code 2.1.101
and codex-cli 0.121.0 once reported both Providers authenticated; the first Claude Worker
then failed on `error: unknown option '--safe-mode'`, and every Codex probe on `error: unexpected
argument '--stdio' found` ([issue #259](https://github.com/PakhomovAlexander/afactory/issues/259)).
So before `af provider status`, `af provider setup` or Task Provider admission asks a CLI about its
login, `af` runs a token-free **capability check** of the resolved executable
([ADR-0146](adr/0146-refuse-a-provider-cli-that-does-not-accept-afs-invocation.md)):

- It runs the (sub)command's help page — `claude --help`, `codex exec --help` — in the probe
  environment: a fresh probe directory, the sanitized `PATH`, no credential read or written, and
  no model call. It reads the options the page declares and compares them with the list the
  adapter crate keeps next to the code that builds the command
  (`review_runner_claude::CLAUDE_INVOCATION`, `review_runner_codex::CODEX_INVOCATION`); a unit test
  holds each list equal to what its command builder emits.
- The decision is made on the help page, never on a version string, so a wrapper or a fork is
  judged by what it accepts. Only a refused CLI is also asked for `--version`, to report it.
- For Codex, the check also proves the app-server protocol the identity and usage probes need: it
  starts the bare `codex app-server` in the same probe environment, sends `initialize`, accepts its
  answer and stops the process. Only a JSON-RPC response with id 1 whose `result` is a JSON object
  proves the protocol. It sends no account or rate-limit request, its answer is never a login
  answer, and it is bounded by the probe timeout. `af` starts `codex app-server` with no
  transport flag: `stdio://` is the default on every verified version, and `--stdio` does not
  exist on 0.121.0.
- No isolation flag is ever dropped, reordered or replaced to make an older CLI run. A CLI that
  lacks one is refused; there is no weaker fallback invocation.

### Claude Code

| What | Value |
|---|---|
| af invokes | `claude -p --output-format json [--model M --effort E] --safe-mode --restricted --permission-mode dontAsk --strict-mcp-config --tools … --allowedTools … [--json-schema …]`, prompt on stdin |
| Attempt environment | `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`, `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1` and `CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1`, for every access mode, beside the auth grants. The third stops Claude Code's automatic model fallback: when the bound model's safety classifier flags a request, the client would otherwise re-run it on another model (Opus 5 or Opus 4.8 for Claude Fable 5.1) and keep the session there; with it a flagged request ends as a refusal on the bound model. `CLAUDE_CODE_NO_MODEL_FALLBACK` is not set, because it also disables the model substitution compaction may need. These are a bounded client mitigation: af still refuses any reply whose usage names a model the Worker did not bind ([ADR-0147](adr/0147-stop-the-claude-model-fallback-and-name-the-model-that-answered.md)) |
| Required options (`claude --help`) | `-p`/`--print`, `--output-format`, `--model`, `--effort`, `--safe-mode`, `--restricted`, `--permission-mode`, `--strict-mcp-config`, `--tools`, `--allowedTools`, `--json-schema` |
| Required protocol | `claude auth status --json` answers the identity probe |
| Checked | by `af provider status` and `af provider setup` for every context, and by Task Provider admission before any Worker or admission Attempt |
| Floor | 2.1.248, the first release with `--restricted` (`--safe-mode` arrived in 2.1.169), from the Claude Code changelog |
| Recommended | 2.1.295 or later: it fixed `--tools` and `--restricted` not applying to built-in tools that register after launch |
| Verified | 2.1.299 passes (macOS arm64; its `--help` and `--version` were rerun on 2026-10-14); 2.1.101 is refused, missing `--safe-mode` and `--restricted` (verified by hand on 2026-10-14) |
| Remediation | `npm install -g @anthropic-ai/claude-code@latest` |

### Codex

| What | Value |
|---|---|
| af invokes | `codex exec --ephemeral --skip-git-repo-check --json -C <sandbox> -s <mode> -o <file> [--output-schema <file>] [--model M -c model_reasoning_effort=…] -`, prompt on stdin; `--output-schema` names the typed reply's strict schema ([ADR-0208](adr/0208-recover-an-attempt-from-a-transient-provider-failure-and-a-reply-that-does-not-parse.md)) |
| Required options (`codex exec --help`) | `--ephemeral`, `--skip-git-repo-check`, `--json`, `-C`/`--cd`, `-s`/`--sandbox`, `-o`/`--output-last-message`, `--output-schema`, `--model`, `-c`/`--config` |
| Required protocol | `codex app-server`, started bare, answers JSON-RPC `initialize` (id 1) over its default stdio transport, then `account/read` or `account/rateLimits/read` (id 2); `codex login status` answers the status probe |
| Checked | the options and the `initialize` answer by `af provider status` (with or without `--usage`), `af provider setup` and Task Provider admission; the whole protocol again by the identity probe before every private Worker send, and by `status --usage` |
| Floor | 0.121.0 verified; older not verified |
| Verified | 0.121.0 and 0.162.1 pass with the bare `codex app-server` (verified by hand on 2026-10-14: on a ChatGPT login `account/rateLimits/read` returns the rate limits, on an API-key login a JSON-RPC error `chatgpt authentication required to read rate limits`); 0.162.1's `codex exec --help` was rerun on 2026-10-14 |
| Remediation | `npm install -g @openai/codex@latest` |

A refused CLI is reported with its Provider ID and kind, the executable path, the detected version,
every missing option or protocol capability, and the upgrade command:

```text
af provider status: provider claude-main: incompatible Provider CLI: the claude CLI `/usr/bin/claude` (version 2.1.101 (Claude Code)) does not accept af's invocation; missing: --safe-mode, --restricted; fix: upgrade it with `npm install -g @anthropic-ai/claude-code@latest`, then rerun `af provider status`
```

| Where | What happens |
|---|---|
| `af provider status` | The context's `auth` is `incompatible` and its usability `unusable`, whatever its login. The command exits 4. stderr carries the report above, and the table's note repeats it. |
| `af provider setup` | `provider_cli_missing`, exit 4, before any login is asked about or started. The document's `diagnostic` is the af-authored summary, without the version; the report goes to stderr. |
| Task Provider admission | Refused before any Worker process starts and before any Attempt is reserved or charged: `Task Provider admission refused before any Worker was dispatched: provider …: incompatible Provider CLI: …`. A refused selection also names each candidate's reason on stderr, below `No Pipeline selected for Task …`. |
| A Codex app-server that exits before `initialize` | The check reports `codex app-server exited 2 before answering initialize: error: unrecognized subcommand 'app-server'` — its exit status and the CLI's own first error line — and refuses the CLI as missing the protocol capability `` `codex app-server` answering JSON-RPC `initialize` over its default stdio transport ``. Plain `af provider status`, `status --usage`, setup and admission give the same `incompatible` report and exit 4; an app-server that answers `initialize` with an error is refused the same way, reported as `codex app-server exited 0 after answering initialize with an error: {"id":1,"error":{"code":-32600,"message":"bad init"}}`, and so is one whose answer has no `result` object (`{"id":1}`, a `null` or non-object `result`) or that writes, before that answer, a line that is malformed under the JSON-RPC line grammar — anything but a JSON object that is a response (an `id` and exactly one of `result` or `error`) or a notification or request (a string `method`): not JSON (`not json`), not an object (`[1,2]`), an `id` with neither `result` nor `error` (`{"id":2,"foo":"bar"}`) or neither `id` nor `method` (`{"foo":"bar"}`) — reported as `codex app-server exited 0 after answering initialize without a result object: {"id":1}` or `codex app-server was stopped by af after answering initialize with a line that is not a JSON-RPC message: {"foo":"bar"}`. Such a line is refused at once, never waited on until the timeout, and af stops the app-server; a notification, a request or a response to another id (`{"id":7,"result":{}}`) before the result is skipped, and blank lines are framing. Each carries its exit status when it exited, or `was stopped by af` when af stopped it still running, and the line it answered with. The identity and usage probes read the `initialize` answer by the same rule, and every line after it until their own id-2 answer by the same grammar: a malformed line there makes `status --usage` and admission report the CLI `incompatible` at once, as ``codex app-server was stopped by af after answering initialize, before answering `account/rateLimits/read`, with a line that is not a JSON-RPC message: {"id":2,"foo":"bar"}``. A response answers a request only if af read it after writing that request: an id-2 response written before af sent `account/read` or `account/rateLimits/read`, for example before the `initialize` answer, is skipped like a response to another id, so admission binds no principal and `status --usage` shows no limits from it; an app-server that then never answers is reported as `` codex app-server exited 0 after initialize without answering `account/read` `` or the probe timeout. When the CLI's own `--version` check also fails it is an installation failure instead. A check that times out leaves the context `unavailable`, never `authenticated`. Before a private Worker send the identity recheck refuses an app-server that stopped answering. |
| A Worker the CLI still refuses | A parsable result is, for Claude, a JSON object with a boolean `is_error` and its `result` (or, for a schema-constrained Attempt, its `structured_output`), and for Codex the `-o` final message of a process that exited 0 without an `error` or `turn.failed` event. Anything else is none: `{}` or other JSON on Claude's stdout, a missing final-message file at exit 0, stdout holding only unrelated events, a non-zero exit after `turn.failed`. When a Worker process exits without a parsable result, the Attempt diagnostic carries its exit status and a bounded stderr excerpt, whatever the failure is classified as (`network`, every `auth_*`, `quota`, `capacity`, `model_unavailable`, `unknown`, `unsupported_invocation`), and keeps the classification: `Codex Worker failed: Provider network request failed (network); it exited 1 without a parsable result; stderr: network failed: proxy refused connection`. The excerpt is read before the privacy filter replaces the captured stream. When the CLI refused af's own arguments (`unknown option`, `unexpected argument`, `unrecognized …`, a `Usage:` block), the failure is classified `unsupported_invocation` and the diagnostic says the installed CLI does not accept af's invocation and names the upgrade. Without a parsable result, one decision per adapter classifies the Attempt in this order: such an argument error on stderr first, whatever the exit status (`0` included), the events a Codex Worker framed first, an `Authentication required` beside it or the transport outcome, so an old CLI is never blamed on credentials; then the native classification; then `unknown`. A parsable result stays a success whatever stderr warns about. |

The help page, the version, the CLI's error line, every stderr excerpt and the Codex rate-limit
names and ids `status --usage` shows are Provider-authored text; plan types and subscription labels
are mapped to af's own names and never quoted. They are bounded and stripped of terminal escapes and control characters. One shared
credential filter judges every such line before it reaches stderr, the human table, a probe error or
an Attempt diagnostic, and replaces the whole line with `[line withheld: looks like credential
material]` when it contains:

- a key, token, secret, password, passphrase, credential or auth assignment, in any case and
  with any prefix: `API_KEY=…`, `OPENAI_API_KEY="…"`, `ANTHROPIC_API_KEY: …`,
  `"refresh_token": "…"`, `DB_PW=…`;
- a password after whitespace alone: `--password hunter2`, a `.netrc` line
  (`machine … login alice password hunter2`), and `mysql -u root -phunter2`; prose such as
  `password is incorrect` passes;
- a URL carrying a password (`postgres://app:pw@db`) or any user in an `http(s)` URL
  (`https://token@github.com`); `ssh://git@github.com/…` passes;
- `Bearer` followed by a token (`Bearer: …` too), or an `Authorization` header;
- a token prefix such as `sk-`, `sk-ant-`, `sk-proj-`, `ghp_`, `github_pat_`, `xoxb-` or `glpat-`,
  and an AWS access key id (`AKIA…`, `ASIA…`);
- a run of 32 or more hexadecimal characters, or 20 or more mixing letters and digits; a run of
  32 or more base64 or base64url characters (`A-Z`, `a-z`, `0-9`, `+`, `/`, `=`, `_`, `-`) that
  holds at least one upper-case letter, one lower-case letter and one digit; a run of 40 or more
  lower-case letters and digits holding both; or a JSON Web Token (three non-empty base64url runs,
  `+` and `/` allowed, joined by single dots, the first starting with `eyJ` and 16 or more
  characters long; `error: eyJabc.def.ghi` passes). A run of letters alone, such as a long option
  name or a path (`--dangerously-bypass-approvals-and-sandbox`, `/Users/alice/projects/af`), and
  a git object id or a SHA-256 digest standing alone (`sha256:…`, a 40-hex commit), pass by
  design;
- two long words joined by one space that pass for one of these together (`sk- ant-api03-…`, a
  key split in two);
- a device code (`ABCD-EFGH`) on a line that says `code`, and the account a login holds
  (`Logged in as alice@example.com`).

The filter is a bounded heuristic, not a promise to recognise every secret.

An ordinary argument error such as `error: unexpected argument '--stdio' found` passes through. A
stream that carries login-challenge material anywhere (an official login origin or a device page
such as `/login/device`, a device, user or one-time code, an OAuth parameter such as `?code=` or
`#code=`) is that marker alone, and so is a Worker's stderr excerpt when its stdout
carries such material, because both captured streams are then replaced. The exit status and the classification always
stay, and a withheld rate-limit label keeps its window figures. Provider-authored text never
reaches a versioned JSON document or retry feedback.

## Output and exit codes

stdout carries the result — human lines, or exactly one JSON document under `--json`. stderr
carries progress, warnings, and one diagnostic line for any non-zero outcome, so a pipeline that
discards stdout still shows the operator what happened.

| Code | Condition | Commands |
|---|---|---|
| 0 | success | all |
| 1 | unclassified failure (unsafe auth directory, unreadable registry, an ID `remove` does not find, …) | all |
| 2 | usage error, from clap | all |
| 3 | human action required | `setup` |
| 4 | Provider CLI missing, cannot start, or does not accept af's invocation | `setup`, `add`, `status`, `models`, `detect` |
| 5 | registry conflict | `setup`, `add`, `detect --apply` |
| 6 | authentication failed; for `add`, a context that is not logged in | `setup`, `add` |
| 7 | optional usage unavailable; a model catalog unavailable | `status --usage`; `models` |

The versioned documents are [`provider-detect-v3.json`](../schemas/provider-detect-v3.json),
[`provider-status-v3.json`](../schemas/provider-status-v3.json),
[`provider-setup-v1.json`](../schemas/provider-setup-v1.json) and
[`provider-models-v1.json`](../schemas/provider-models-v1.json). They are closed vocabularies; the
models document carries model IDs, aliases, effort levels and bounded display names only. None
can represent an account email, an organization or account identity, a credential, OAuth material,
or raw Provider output — quota windows carry only the numbers `af` derived itself, and the human
table keeps the Provider's own window labels.

## Troubleshooting

| Symptom | Meaning and action |
|---|---|
| exit 3, `human_action_required` | Run the printed `af provider setup … --login` yourself at a private terminal. Never paste the OAuth URL or code anywhere else. |
| exit 4, `provider_cli_missing` or `installation_failed` | The official CLI is missing, not executable, or cannot start. Run the fix printed on stderr (for npm installs, usually the CLI's own reinstall command), then rerun `af provider status`. |
| exit 4, `incompatible` | The CLI starts but is too old for af's invocation. Run the upgrade printed on stderr, then rerun `af provider status`. |
| exit 5, `registry_conflict` | The ID or the auth directory already belongs to another entry. Choose a different ID, or point `--auth-dir` at the context you meant. |
| exit 6, `authentication_failed` | The Provider CLI answered, and not with a usable login. Repeat the login at a private terminal and confirm with `af provider status`. |
| exit 6 from `af provider add`, `has no … login` | The directory is not logged in. Run the printed `af provider setup … --login` yourself at a private terminal, or pass `--unverified` to register it anyway. |
| `not probed` in `status`, `empty (not probed)` in `detect` | The directory is absent or holds no login marker, so af did not hand it to its CLI. Log in there with the printed setup line, or run `af provider detect --all` to ask the CLI anyway. |
| `model_refused` on the stderr of `af task run` or `af review run`, or in an Attempt diagnostic: `the safety classifier of MODEL (effort E) flagged the request and MODEL refused it` | The bound model's safety classifier flagged the Worker's input and Claude Code, with its automatic fallback turned off, ended the turn with a refusal (`safety_stops` above zero, `stop_reason` `refusal`, or `…'s safeguards flagged this message` in the result of an error result, whatever its `errors` holds). It is not a credential or spelling problem. af charged what the result reported and did not retry, because the same input is flagged again. Change the input or bind another model, then rerun ([ADR-0147](adr/0147-stop-the-claude-model-fallback-and-name-the-model-that-answered.md)). |
| `Claude Code switched models, a safety-classifier fallback (model_refused)` or `an unexplained switch (model_switched)` | Claude Code answered with a model the Worker did not bind; the line names the bound model, each model that answered with its output tokens (a model ID outside the printable grammar reads `[not printable]` with its byte length), and `safety_stops` and `num_turns`. The diagnostic in `af task show` starts with it even when the usage was also malformed or listed more than 32 models, and gives those accounting reasons after it; an entry for another model that reports no activity, or only malformed counters, is not a switch. af refused the reply, because it never runs or accepts an unrequested model, and charged what the result reported. af already sets `CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1`, so a client that still switches ignores it: upgrade it with `npm install -g @anthropic-ai/claude-code@latest` and rerun. If the classifier flags the input again, change it or bind another model. |
| `model_unavailable` after `Claude Code rejected the model`, quoting the model ID | Claude Code answered "There's an issue with the selected model (…). It may not exist or you may not have access to it.": it does not know that model ID or the account cannot use it. The line names the Provider, the nearest exact IDs af knows and `af provider models <id>`. Correct the binding's model ID (Claude IDs use hyphens, such as `claude-fable-5-1`), then check the Provider with `af provider status` (token-free) and `af provider doctor` (charged). |
| `af refused the binding of model … (model_not_accepted)` from `af task plan`, `af review plan`, `af review render` or a run | A rule refused the binding before any process started: an alias (with the exact ID it resolves to), a misspelling of a known exact ID (with the nearest IDs), an unversioned model, an effort level the Provider kind does not accept or the model does not list (with the levels it lists). Bind the exact ID and an effort level `af provider models <id>` lists; `--resolve` prints the configuration to copy ([ADR-0148](adr/0148-discover-provider-models-and-validate-every-binding-before-a-charge.md)). |
| `its --model value (N bytes) is not a model spelling af accepts` | The binding's model (or `--effort`) is outside the one grammar af prints: an ASCII letter or digit, then at most 127 ASCII letters, digits, `-`, `.`, `_` or `:`, with no `/`, `@` or whitespace. `af task plan`, `af review run` and both adapters refuse it before any process starts and never print the value, prefixed with the Worker or node and its Provider label; every Worker's binding is checked before the first Provider is probed, and this rule before every other. Put the model ID itself in the binding, such as `claude-fable-5-1` or `us.anthropic.claude-opus-5-5-v1:0` ([ADR-0147](adr/0147-stop-the-claude-model-fallback-and-name-the-model-that-answered.md)). |
| exit 7 from `status --usage` | Optional usage probing did not answer. Authentication is unaffected; drop `--usage` if you only needed that. |
| `auth directory … is writable by another user` | Tighten ownership and permissions on the directory (and its rename-controlling parents) before registering it. |
| `provider registry … is invalid` after an auth directory was deleted | Run `af provider remove ID` for the entry that names it; name every stale entry in one command. |
| `unfinished publication transaction` | Run `af provider recover`: it validates the marker and every preserved hash before archiving, and deletes no version. |
