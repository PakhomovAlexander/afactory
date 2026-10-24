# ADR-0197: Detect Provider logins, and register them through `add`

**Status:** accepted (2026-10-22). Extends
[ADR-0141](0141-list-a-logged-out-default-context-only-until-its-kind-is-registered.md): the
ambient discovery it narrows now skips a default directory with no login marker. Keeps
[ADR-0111](0111-keep-provider-bootstrap-machine-local-and-cross-release-safe.md) (machine-local
bootstrap, atomic publication), [ADR-0112](0112-refuse-agent-mediated-provider-logins.md) (no
agent-mediated login, identity-free documents) and
[ADR-0146](0146-refuse-a-provider-cli-that-does-not-accept-afs-invocation.md) (the capability
check before any login is asked about).

## Context

A machine with per-account harness directories — `~/.config/claude-personal`,
`~/.config/claude-work`, `~/.codex-gridgain` and a logged-out `~/.claude` — was shown one `not
authenticated` ambient row by `af provider status` and told to log in again. Ambient discovery
looks only at `CLAUDE_CONFIG_DIR` or `~/.claude` and `CODEX_HOME` or `~/.codex`, and only when the
binary is on `PATH`. Every login the operator already had was invisible, and the only fix offered
was a new one.

Four smaller faults sat beside it. `status` asked `codex login status` about a `~/.codex` that did
not exist, and that command creates its home on first contact, so a status check left a
directory behind; `setup` kept its lock, `.af-<kind>-setup.lock`, in the auth directory, a file in
a directory that belongs to the CLI. `af provider add` registered a directory without asking
whether it held a login, so the first Task found out. `setup --auth-dir` created a mistyped path.
Each status row offered two fixes, the harness's own `CODEX_HOME=… codex login` and af's `setup`
line, and the `--usage` hint appeared on a machine with no login at all. And the Codex
private-host login (issue #305) refused every codex-cli after 0.159.2, whose device prompt it
matched byte for byte, version included.

## Decision

`af provider detect [--dir PATH]... [--id ID]... [--kind claude|codex] [--all] [--apply] [--json]
[--identity]` finds the CLIs and their auth directories, asks each directory's CLI the question
`status` asks, and prints the one command that registers it. It is machine-local like `setup`:
it never dispatches through a project pin.

- **Scan sources.** Binaries on the sanitized `PATH`, then a fixed list of install directories
  (`~/.local/bin`, `~/.claude/local`, `/opt/homebrew/bin`, `/usr/local/bin`, `~/.npm-global/bin`,
  `~/.volta/bin`, `~/.bun/bin`); a binary off `PATH` is reported with the one-line PATH fix for
  `$SHELL`. Directories from `env` (the selector variables, absolute only), `default`
  (`~/.claude`, `~/.codex`), `sibling` (literal one-level names: `~/.claude-*`,
  `~/.config/claude`, `~/.config/claude-*`, `~/.codex-*`, `~/.config/codex`, `~/.config/codex-*`),
  `registry` and `explicit` (`--dir`), deduplicated by canonical path, the most specific source
  kept. No shell rc file, `npm prefix` or `nvm` is evaluated: a directory a shell alias selects
  needs `--dir`, and the output says so. Unsupported harness CLIs on `PATH` (a fixed list) are
  named with their version and nothing of theirs is scanned.
- **Marker rule.** A Claude directory holds `.claude.json`, `.credentials.json` or
  `settings.json`; a Codex directory holds `auth.json` or `config.toml`. The names are examined
  with `fstatat(AT_SYMLINK_NOFOLLOW)` below a descriptor of the directory opened `O_NOFOLLOW`; no
  file under a candidate is opened. The owner, mode and symlink rules `setup` applies are a
  `safe` column, not a later failure.
- **No probe of an empty directory.** A directory without a marker is listed `empty` and is not
  handed to its CLI unless `--all`; an unsafe or absent one never is. `status` applies the same
  rule to every context, after locating the CLI and before its capability check: an absent or
  unmarked context is `not_probed` (a value `af/provider-status@2` already has), with its setup
  line, and a default one is left out once its kind is registered, as ADR-0141 leaves out a
  logged-out one. For the default Claude context probed without a selector, `~/.claude.json`
  counts as its marker, because that is where the CLI keeps that context's state.
- **The probe** is `status`'s, shared code: the version and capability checks once per binary,
  then one bounded login check per directory, at most four at a time, the selector variable always
  set, so `authenticated` here means `add` sees the same login. A binary with no marked
  directory is checked against a private empty directory, never one of the operator's.
- **ID rule.** `<kind>-main` for the default directory; for a sibling, the kind and the directory
  name without its leading dot and kind prefix (`~/.config/claude-work` → `claude-work`,
  `~/.config/claude` → `claude-config`); for the `env` directory the sibling rule when its name is
  a sibling name, else `<kind>-env`; for `--dir` the sibling rule, or `--id` in `--dir` order,
  required when more than one `--dir` names one kind. A taken ID gets `-2`, `-3`; `*-ambient` is
  never generated.
- **`--apply`** registers, in table order, every authenticated row not registered yet, through
  `add`'s locked, atomic publication and safety checks, and prints each line it ran. It never
  logs in, never creates a directory and never touches a logged-out row, whose `setup … --login`
  line it prints. A registry conflict stops it at that row; the rows before it stay published.
  Without `--apply` nothing is written.
- **Identity rule.** LOGIN is af's label for the plan or method (`Claude Max (claude.ai)`,
  `ChatGPT`, `API key`, `not logged in`); the Codex plan is not in the status probe's answer and
  stays `unknown`. `--identity` adds the CLI-reported account label, through the credential
  filter, only when stdout is a terminal and `--json` is absent. `af/provider-detect@1` carries no
  email, organization, credential or Provider-authored text; versions are af's reading of a
  version line.
- **Exit codes.** 0 the scan completed (with nothing found, the install guidance); 1
  unclassified; 2 usage (an `af/error@1` document under `--json`); 4 a found CLI cannot start or
  is incompatible, rows still printed; 5 a registry conflict under `--apply`. Never 3 or 6: detect
  starts no login and judges none.

Beside it: `add` probes and refuses a context that is not authenticated with exit 6 and its
`setup … --login` line unless `--unverified`, which prints `warning: not verified here`. `setup`
creates a missing directory only with `--login`. The auth-context lock moves to
`$XDG_STATE_HOME/af/locks/<kind>-<sha256 of the canonical auth directory>`; the private-host
login takes the same lock, and its guard is told the owner's state root. Each status row names one
fix, `af provider setup <id> --kind <kind> [--auth-dir …] --login`; `--verbose` adds the CLI's
own login command; the `--usage` hint needs an authenticated row. With nothing registered,
`status` prints the install guidance when no CLI is on `PATH`, and names what a names-only scan
found. The Codex device prompt parser reads the banner's version as its own field, by the
grammar `MAJOR.MINOR.PATCH[-pre]`, between two exact halves of the framing; everything else stays
byte-exact.

A debug build's test fixture may replace the two system install directories with
`AF_TEST_DETECT_SYSTEM_DIRS`, so no test can find a developer's own CLI; a release binary never
reads it, as ADR-0114's probe-timeout setting.

## Considered options

- **Widen ambient discovery in `status`.** More `*-ambient` rows, each probed on every status.
  Rejected: status would grow slower and keep inventing unstable labels, and the operator would
  still have to register each directory by hand. Detection is a one-off that ends in registry
  entries.
- **Read the credential files to tell a login.** Rejected by ADR-0112: af never reads a
  credential, and the CLI is the authority on its login.
- **Probe every candidate.** Rejected: `codex login status` creates its home, so a scan would
  leave a directory under every sibling name it tried.
- **Evaluate `npm prefix`, `nvm` or rc files to find binaries and aliases.** Rejected: running the
  operator's shell configuration is a side effect a scan must not have; the fixed list and `--dir`
  cover the cases, and the output names the gap.
- **Keep the lock in the auth directory.** Rejected: it is the CLI's directory, and a lock file
  there is a file a setup that changed nothing left behind.

## Consequences

- The first ten minutes on a new machine are install, `af provider detect --apply`, `af onboard`,
  review.
- `add` is stricter: scripts that registered a directory before logging in pass `--unverified`.
- A setup lock left in an auth directory by an earlier release is ignored and may be deleted; no
  release reads it (ADR-0145). An earlier release's setup and this one's no longer share the
  auth-context lock; registry publication stays serialized by the registry lock, which did not
  move.
- A status context without a marker reports `not_probed` where it used to report the CLI's
  answer; logging in there, or `af provider detect --all`, brings the answer back.

## Amendment, 2026-10-24: what detect says when it cannot know

Close-out C3 of the release-polish campaign makes `af provider detect` print
`af/provider-detect@2` (`schemas/provider-detect-v2.json`; version 1 is neither printed nor
read), and `af provider status` print `af/provider-status@3`.

- **The auth probe waits 10 seconds at most**, whatever the kind. Claude's structural timeout
  is 30 s and Codex's 15 s; a scan of every login on a machine does not wait either. A row whose
  login is not known carries `auth_cause` (`timeout`, `unstartable`, `incompatible`,
  `unparsable`, `output_too_large`, `cancelled`, `unchecked`, `other`) and `auth_detail`, af's
  own words for the cause, never the CLI's.
- **A directory that cannot be listed** (it grants search but not read) has its markers checked
  by name, one path each, and is probed. Its row says `listing unreadable`, so it is never
  reported as an empty directory. `af provider status` applies the same rule.
- **An unreadable registry is unknown, never empty.** A registry that is malformed, cannot be
  read or is mid-transaction gives every row `registration: unknown` and the suggestion
  `fix_registry`. Its note is in every output, the empty scan's included, and `--apply` refuses
  with exit 1 (`af/error@1` under `--json`) before it scans.
- **`--apply` checks a registered row again under the registry lock.** If the entry was removed
  after the scan, a logged-in row is registered again and a logged-out one is reported gone. It
  is never called `already_registered`.
- **A login whose CLI is off `PATH` is not registered**, because `af provider status` and Task
  admission look on `PATH` only. Its suggestion is `fix_path`, the PATH line. `--apply` leaves
  it `skipped_cli_off_path` and exits 4, the code of a CLI af cannot use.
- **A registry conflict suggests the retry that works.** The applied row names `conflict_with`.
  Its suggestion becomes `retry_apply`, `af provider detect --dir D --id OTHER --apply` with the
  first ID nothing holds, or `af provider status` when the directory is registered under
  another ID now. The closing `next:` line follows the outcome: that retry after exit 5, the
  CLI's upgrade or PATH line after exit 4, and `af provider status` only after an apply that
  succeeded.
- **One names-only scan** lists the directories found but not registered. `af provider status`
  (text, and `found_unregistered` with `next_action` in its document), `af doctor` and
  `af onboard` all use it.

## Amendment, 2026-10-24: typed next actions, and one default Claude context

The final fix round of the release-polish campaign makes `af provider detect --json` print
`af/provider-detect@3` (`schemas/provider-detect-v3.json`). It is the one version printed:
versions 1 and 2 are neither printed nor read, and each has no schema file any more
([ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)). Where
this record says `af/provider-detect@1` or `@2` above, the current document is version 3.

- **Next actions are typed fields.** `install` gives, for each Provider kind scanned whose CLI
  was not found, `{kind, command, docs}`: the official install command and documentation page.
  `next_action` is the scan to run again once one is installed, `af provider detect` with the
  scan's own `--kind`, `--dir`, `--id` and `--all`, when no CLI was found, and null otherwise,
  when each row's `suggested` is the next action. `notes` stay, for a person.
- **One default Claude context.** `af provider status` and `detect` ask one predicate: the
  canonical `~/.claude`, with no marker inside it, is a marked context when a regular file
  `~/.claude.json` sits beside it, where the CLI keeps that context's state, with a selector or
  without one. Detect lists such a directory with the marker `~/.claude.json`, probes it as it
  probes every marked directory, and `--apply` registers it as `claude-main`; status then probes
  the registered Provider instead of calling it `not_probed`.
