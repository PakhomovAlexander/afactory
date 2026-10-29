# Installing, updating and removing af

The [README](../README.md#install) installs af in one line. This page is what that line does, how
af keeps itself up to date, and how to take it off a machine again. `af help self` is the same
policy as the binary states it, and `af doctor` checks an installation, token-free.

## What the installer does

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://github.com/PakhomovAlexander/afactory/releases/latest/download/install.sh | sh
```

`install.sh` places the newest stable release in the self-managed layout
(`$XDG_DATA_HOME/af/versions/<v>/`, with the default symlink at `~/.local/bin/af`), and `af`
takes over from there: the installed binary records its own activation, so `af self rollback`
works from the first update on. `AF_INSTALL_CHANNEL=rc` (or `sh -s -- --rc`) installs the newest
pre-release instead, the installer's form of `[self] channel = "rc"`, and
`AF_INSTALL_CHANNEL=nightly` (or `sh -s -- --nightly`) the newest nightly by its date, the form of
`[self] channel = "nightly"` ([Channels](#channels)); `AF_INSTALL_VERSION=X.Y.Z` installs one
release. A re-run is idempotent and verifies the installed binary again against the release, never
on the strength of its receipt.

It ends with at most six lines: the installed version and its path, the next steps of a first
run (`af provider detect`, `af onboard`, `af self setup-shell --write`), and, when the bin
directory is not on `PATH`, the line that puts it there for the shell `$SHELL` names
(`fish_add_path` for fish, an `export PATH=…` line and its rc file for bash and zsh).

## Signatures

Every release ships per-target tarballs, a `SHA256SUMS` file, and `SHA256SUMS.minisig` signed
with the release key in [`crates/af/keys/release.pub`](../crates/af/keys/release.pub); the binary
embeds that key and refuses an unsigned or badly signed release. `install.sh` verifies the
archive digest and the signature, fetching over HTTPS only. Without `minisign` on `PATH` it
refuses and prints the install line for this machine's package manager (`brew install minisign`,
`sudo apt install minisign`, …), unless `AF_INSTALL_ALLOW_UNSIGNED=1` accepts the checksum alone,
which comes from the same release as the archive and so catches a corrupt download only. Such an
install records `verified_by = "sha256sums"` in its receipt; `af self status --json` and
`af doctor` show it.

A nightly is signed with a key of its own, the nightly key, whose public half belongs at
`crates/af/keys/nightly.pub`: a build embeds it as it embeds the release key, and
`AF_NIGHTLY_KEY` names another key file. A nightly's `SHA256SUMS` is verified against the
nightly key alone, and a release's never against it. A build without the file carries no nightly
key and refuses every nightly install in one sentence (`this af carries no nightly key`);
`install.sh` embeds the same key beside the release key (`AF_INSTALL_NIGHTLY_KEY` overrides it),
refuses the nightly channel while that is empty, and never installs a nightly unsigned.

## Updates

Updates need no `gh` for the public repository: `af self` lists and downloads releases
anonymously over HTTPS with `curl`, as the installer does, and turns to the GitHub CLI (under
your own `gh` login) only when that is refused, as it is for a repository that is not public.
When a download fails both ways, the error names both reasons. `af self update` without
`--version` never moves the default to a release older than the one running.

```sh
af self update --check   # exit 10 when a newer release exists
af self update           # install it and make it the default
af self rollback         # back to the previous default
```

`af self update` follows the configured channel; [Channels](#channels) says what each one
installs and how to switch. After commands, `af` also checks for a newer release on its own;
[the release check](#the-release-check) below says when it runs, what bounds it, what it
records and what `af self status` prints from it.

## Channels

`[self] channel` says what `af self` installs and updates to:

- `stable`, the default: the newest release.
- `rc`: the newest release or release candidate, by semantic version.
- `nightly`: the newest nightly build of main, by the date its tag names (`nightly-YYYY.MM.DD`).
  A nightly reports the version `<core>-nightly.<YYYYMMDD>` (`0.14.0-nightly.20261029` while main
  is `0.14.0-rc.3`), sorts as a pre-release of its core, and installs beside releases under
  `$XDG_DATA_HOME/af/versions/<version>/`. Its receipt records its `tag`, `channel = "nightly"`
  and the `commit` its binary reported. Nightlies are built for Linux only until macOS nightlies
  exist.

```sh
af self channel                    # the configured channel and the layer it comes from
af self channel nightly            # write it, install the newest nightly, make it the default
af self channel stable             # back to the newest release, even an older one
af self channel rc --no-update     # write it only; af self update follows it from then on
```

`af self channel <name>` writes `[self] channel = "<name>"` into the user layer, the `user` file
`af config paths` names, keeping the file's other keys and comments and creating it when absent.
Unless `--no-update`, it then installs the channel's newest and makes it the default. A switch is
an explicit choice, so it may make an older version the default, nightly to stable for one, and
it says so (`moved back from af … to af …`); `af self update` never moves the default backwards.
What the switch needs, the nightly key for `nightly`, an af installed by `af self` and the
network unless `--no-update`, is checked before the file is written.

On the nightly channel `af self update --check` exits 10 when a nightly with a later date than
the running one exists and 0 otherwise, naming the nightly's tag; an af that runs no nightly is
always told of the newest one, with `af self channel nightly` as the command. `af self update`
installs a newer nightly and makes it the default only when its version is newer than the running
one. The background check asks for nightlies and, under `auto_update = "always"`, moves an af
that already runs a nightly to a newer one; moving onto the channel is `af self channel`'s.
`af self status` prints a `nightly:` line with the running nightly's tag and commit and the
newest nightly the last check found.

A project whose `.af/af.lock` pins a release keeps running that pin whatever the channel
([ADR-0044](adr/0044-af-manages-itself-and-dispatches-to-the-pinned-release.md)). A nightly is
never pinned: `af onboard` writes no pin from a nightly, and an install a lock asks for is never a
nightly. `af self rollback`, `remove`, `prune` and `keep_versions` treat an installed nightly like
any installed version ([ADR-0267](adr/0267-offer-a-nightly-channel-in-af-self.md)).

## The release check

`af` manages its own installation (`af self`,
[ADR-0044](adr/0044-af-manages-itself-and-dispatches-to-the-pinned-release.md)) and checks for
newer releases after commands
([ADR-0154](adr/0154-check-for-updates-after-headless-and-json-commands.md)). This section says how
that check behaves: when it runs, what bounds it, what it records and what `af self status`
prints from it. `af help self` has the whole `[self]` policy.

### When the check runs

- After an eligible command (any but `help`, `schema`, `--version`, `af self`, `af config`,
  `doctor`, `completions` and the browser) `af` spawns a detached `af self refresh-check` when the check is
  due; it caches the newest release and exits. Headless and `--json` runs check too: the child
  holds none of the command's pipes, never changes the command's output or exit status, and
  nothing waits for the network. No timer, daemon or service runs between commands.
- `af self status` schedules the same detached check when it is due, so the `last check` it
  prints never stands still on a machine where only status runs.
- `af self status --refresh` runs the check in its own process, before it prints, whether or not
  it is due.
- `af self update --check` asks the release source at once and exits 10 when a newer release
  exists, 0 when none does and 1 with the reason when the source cannot be asked.
- `[self] check_every` (24 hours by default) counts from the last attempt, answered or not: an
  outage costs one query per `check_every`. `CI`, `AF_SELF_OFFLINE=1`, `[self] update_check =
  false` and `auto_update = "never"` turn the check off. Offline, `af self update --check` prints
  `offline: check skipped` and exits 0, and `af self status --refresh` says it skipped the check.
- The cached notice reaches stderr only when stderr is a terminal and the run has no `--json`.
- While a background check runs, `af self uninstall` refuses rather than race an installation it
  may be making; run it again when the check has finished.

### What bounds it

Releases come anonymously over HTTPS with `curl` from a public repository; the GitHub CLI is
asked, under your own `gh` login, only when that route is refused (HTTP 401, 403 or 404) or
`curl` is missing. Neither can stall the check:

- `curl` runs with `--connect-timeout 5 --max-time 30` to list releases (600 s to download an
  asset), its retries within the same limit, and `af` ends its process group if it outlives
  them.
- `gh` runs under a 30 s deadline to list releases.
- When the anonymous route fails, the reason comes first, the HTTP status or `curl`'s own error,
  then what `gh` added, and only then a `gh auth login` hint for a repository that is not
  public. A public repository never needs `gh`; neither does `install.sh`.

The check holds `$XDG_CACHE_HOME/af/self/latest.lock` while it runs and stamps in it when it
started. A check that has held the lock for more than an hour counts as no check: the cache does
not move, and `af self status` and `af self update --check` report it with the command that
clears it:

```text
check:      a release check has been running since 2026-09-12T08:00:00Z; fix: rm ~/.cache/af/self/latest.lock
```

### What it records

The cache, `$XDG_CACHE_HOME/af/self/latest.toml`, is `af/self-check@4`
([schema](../schemas/self-check-v4.json)):

- `checked_at_epoch`: when a check last answered;
- `attempted_at_epoch`: when one was last attempted, answered or not;
- `last_error`: why the last attempt failed, one line of at most 512 bytes, absent once one
  succeeds;
- `latest`: the newest release on the configured channel, on the `nightly` channel the newest
  nightly's tag;
- `latest_prerelease`: the newest pre-release, never a nightly, asked for whatever the channel
  while the running `af` is itself a pre-release or the channel is `rc`, and never recorded on the
  `nightly` channel;
- `auto_updated`: the activation a background check made under `auto_update = "always"`.

A cache an earlier release wrote, of any version, is not read: that machine is due a check,
which writes this one ([ADR-0145](adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).

### What status prints

```text
last check: 2026-10-20T09:12:44Z · latest 0.13.0 · last attempt 2026-10-23T08:00:02Z failed: curl https://api.github.com/repos/…: curl did not answer within 70 s and was stopped — fix: check the network and retry, or set AF_SELF_OFFLINE=1 to skip release checks
```

`latest X` alone is printed only when the running `af` is a stable release no newer than X. A
pre-release user sees both, and the command when a newer pre-release waits:

```text
last check: 2026-10-23T08:00:02Z · latest stable 0.11.0 · latest pre-release 0.12.0-rc.5 (you run 0.12.0-rc.3: af self update --rc)
```

A release older than the running one is always qualified (`latest stable 0.12.0` under a running
0.12.1), never shown as simply the latest. The notice after commands names the same command: `af
0.12.0-rc.5 is available (running 0.12.0-rc.3) — af self update --rc`.

`af self status --json` prints one `af/self-status@2` document
([schema](../schemas/self-status-v2.json)) with `last_check`, `last_attempt`, `last_error`,
`latest`, `latest_prerelease`, `check_stalled_since`, `check_lock` and `next_action` beside the
installation, the pin and the policy. `next_action` is the command the text names: `rm` of the
lock file for a check that has held it over an hour, else `af self update --rc` while a newer
pre-release is known. A check that fails, `af self update --check` included, is recorded as the
last attempt with its cause, and `af doctor` warns about it and about a stalled check with the
same fix. `latest` always names the newest release of the configured `[self] channel`, a
nightly's tag on the `nightly` channel, where `policy.channel` is `nightly`: an explicit
`--check --rc` on the stable channel records what it found as `latest_prerelease` only.

## macOS: Spotlight and Gatekeeper

On macOS, `af doctor` warns in its `storage` row when Spotlight indexes the warm build cache,
the warm Workspaces or the sandbox root, which every gate writes thousands of files to. The fix,
Spotlight's Search Privacy or roots named `*.noindex`, and the Gatekeeper exemption for your
terminal are in [the troubleshooting section](README.md#macos-spotlight-and-gatekeeper).

## Shell completions and man pages

`af self setup-shell --write` writes the completion script for `$SHELL` (bash, zsh, fish, elvish
or powershell) into that shell's autoload directory and the man pages into
`$XDG_DATA_HOME/man/man1`, then says what is left to do: nothing for fish, the rc line it printed
for the others. It never edits an rc file. `af self setup-shell --remove` deletes exactly the
completion file and the af man pages it would write.

## Source builds

Supported release targets: `aarch64-apple-darwin`, `x86_64-unknown-linux-musl`, and
`aarch64-unknown-linux-musl` (static: no glibc floor). On x86_64 macOS, or another Linux or macOS
target, build from source:

```sh
cargo install --path crates/af --locked
```

A source checkout can be ahead of the latest published release. `af --version --json` identifies
its version, commit and target. A source build has no release receipt, so onboarding generates
package pins but cannot create a verified `af` release pin; use an official installed release for
that byte-binding pin. Onboarding leaves an existing release pin in the lock unchanged. Dispatch
enforces the pinned bytes only through install receipts: a source build of any other version runs
the pinned release, but a source build of the pinned version itself has no receipt to compare and
runs as built.

A source install has no signed release receipt, so `af self update`, `rollback`, and `uninstall`
refuse to manage it. `af self status` identifies that state and prints the exact
`af self install <version>` command. That command replaces the running source-installed executable
with a verified release and adopts the self-managed layout. It never replaces an unrelated file at
the default path.

`af` runs on Linux and macOS only. Windows and other non-unix hosts are unsupported, and so are the
BSDs: a source build on any of them stops with a compile error.

## Removing af

```sh
af self uninstall           # every installed version and the default link
af self uninstall --purge   # also config, state, cache, Stores, completions and man pages
```

Without `--purge`, `uninstall` keeps af's config, state and cache, and names the completion files
and man pages `setup-shell --write` installed, with the `rm` line that removes them: af itself is
gone once it finishes. `--purge` removes those too, with every registered Store and the Claude
history af's Workers left in each registered Claude config directory
([ADR-0144](adr/0144-hold-afs-disk-use-to-a-machine-budget.md)).
