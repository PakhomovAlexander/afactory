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
pre-release instead, the installer's form of `[self] channel = "rc"`; `AF_INSTALL_VERSION=X.Y.Z`
installs one release. A re-run is idempotent and verifies the installed binary again against the
release, never on the strength of its receipt.

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

After an eligible command (any but `help`, `--version`, `af self`, `af config`, `doctor`,
`completions` and the browser), `af` spawns a detached `af self refresh-check` that caches the
newest release and exits. Headless and `--json` runs check too: the command's output and exit
status never change, and nothing waits for the network. `[self] check_every` (24h) throttles
invocations; no timer, daemon or service runs between them. The cached notice reaches stderr only
when stderr is a terminal and the run has no `--json`. `CI`, `AF_SELF_OFFLINE=1`,
`[self] update_check = false` and `auto_update = "never"` turn the check off
([ADR-0154](adr/0154-check-for-updates-after-headless-and-json-commands.md)). While a background
check runs, `af self uninstall` refuses rather than race an installation it may be making; run it
again when the check has finished.

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
