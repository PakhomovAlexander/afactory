# `af` manages itself: dispatch to the pinned release, policy-driven updates, a layered configuration

**Status:** accepted (2026-09-03) — designed with a consuming project that moved its pin into
`af.lock`; implements issues #55–#59.
Part 2's "verifying it against the release checksums" is revised by
[ADR-0045](0045-one-release-train-and-a-pin-that-binds-bytes.md): under a lock, the lock's own
per-target digest is what the bytes must match.

Consumers ran `af` through per-repository launcher scripts that carried a version and digests,
while `af.lock` (ADR for #47) already recorded the release that wrote it and could only warn or
refuse on drift. The CLI parsed argv by hand with one usage string for every command, had no
completions, no man page, and no way to install, update, or remove itself. Configuration had one
project layer and a handful of `AFACTORY_*` / `REVIEWCTL_*` knobs.

The decision, in five parts that ship together:

1. **One command tree.** clap derive defines every command once; help, `af help <topic>`, shell
   completions, and man pages are rendered from it. `af <namespace>` shows only that namespace;
   a usage error prints only the failing command's usage. Every existing flag and `--json` shape
   is unchanged; `--help` answers in milliseconds.
2. **Dispatch to the pin.** Every invocation except `self`, `help`, `config`, `completions`, and
   `--version` reads the project's `.af/af.lock`; when it pins a different release, `af` execs
   that version from `$XDG_DATA_HOME/af/versions/<v>/af`, installing it on demand and verifying it
   against the release checksums. `AF_SELF_OFFLINE=1` or `[self] install_pins = false` refuses
   with the exact command and falls back to the #47 behaviour (a newer binary notes, an older one
   refuses). `AF_VERSION` overrides; `AF_DISPATCHED_FROM` stops recursion.
3. **`af self`.** Install, update, rollback, remove, prune, setup-shell, uninstall, over a layout
   whose record is the filesystem: a receipt per version, the default as a symlink, activation
   history in XDG state. `af self` refuses to touch a binary without a receipt. Releases come
   through a `ReleaseSource`: GitHub via `gh` (no token stored) or a directory
   (`AF_RELEASE_SOURCE`) — the second implementation and the test double.
4. **Update policy.** `[self]` in the user configuration: `update_check`, `check_every`,
   `auto_update = notify | always | never`, `channel`, `install_pins`, `keep_versions`. The check
   never runs on the hot path: after the result is written, on a TTY, never under `CI`, `--json`,
   or offline, a detached child refreshes a cache; the next run prints one stderr line. Under
   `always` the child installs and retargets the default; the running process and every pinned
   project are untouched. This revises issue #17's "never auto-update".
5. **A configuration ladder.** built-in · system `/etc/af` · user `$XDG_CONFIG_HOME/af` (+
   `conf.d`) · directory (`.af/af.toml` in every ancestor above the git toplevel, nearer wins) ·
   project · local (`.af/af.local.toml`) · environment (`AF_<TABLE>__<KEY>`). Tables deep-merge,
   scalars last-wins, arrays replace; `af config show --origin` names the file and line of every
   value. No branch layer: git already versions `.af/` per branch. `AFACTORY_*` / `REVIEWCTL_*`
   knobs are renamed `AF_*` and the old names are refused with the new one; the config directory
   moves from `afactory/` to `af/` with a read-through of the legacy path.

## Considered options

- **Keep launcher scripts as the pin** — every consumer ships and updates a script; any agent
  or CI job calling `af` directly bypasses it. Rejected: the pin must hold wherever `af` runs.
- **Keep the hand-written parser and add per-command usage strings** — a second copy of every
  flag for help, a third for completions. Rejected: the tree is the only source that cannot drift.
- **Auto-update on start (rustup-style proxies, Homebrew)** — a tool that changes its own
  version underneath a Task breaks replay. Rejected in #17 and still rejected; the detached
  check plus pin immunity is what makes `always` safe.
- **An in-binary HTTP client** — TLS and a dependency tree for one download. Rejected for now:
  `gh` is already the credential boundary and `tar` is on every host; a directory source covers
  offline and tests. Revisit when the repository is public.
- **Branch overlays (`[branch."<glob>"]`)** — a mechanism for what git does by committing
  `.af/` on the branch. Rejected.

## Consequences

- The hub and every consumer can drop the versioned launcher once their pin reaches a release
  that dispatches; `install.sh` and the release archive (binary, completions, man pages,
  `SHA256SUMS`) are the distribution.
- Values 3 and 7 against value 4: no polling on any path, the notice is stderr-only, and a pinned
  run is byte-identical with any policy. Tests prove the last point.
- Tests that pin a different release must set `AF_SELF_OFFLINE=1` (the lock tests and the
  consumer fixture check do), or they would install a real release.
- The trust gate for executable-bearing configuration keys is declared (`af help trust`) but
  has nothing to guard yet: this release reads no such key from the ladder.
- Provenance beyond `SHA256SUMS` (attestations or minisign) stays open in #17.
