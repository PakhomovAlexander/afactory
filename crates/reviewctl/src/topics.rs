//! Long-form help pages: `af help <topic>`, also rendered as `man af-<topic>`.

pub(crate) const TOPICS: &[(&str, &str, &str)] = &[
    ("config", "How af is configured", CONFIG),
    (
        "layers",
        "The configuration ladder and where each file lives",
        LAYERS,
    ),
    ("environment", "Environment variables af reads", ENVIRONMENT),
    ("exit-codes", "What every exit code means", EXIT_CODES),
    ("json", "The --json contract", JSON),
    (
        "self",
        "Installing, updating, pinning, and removing af",
        SELF_TOPIC,
    ),
    (
        "trust",
        "Which configuration may execute code, and when",
        TRUST,
    ),
];

pub(crate) fn find(name: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    TOPICS.iter().find(|(topic, _, _)| *topic == name)
}

const CONFIG: &str = "\
Everything a human writes is TOML; everything af writes goes to the Store, except `.af/af.lock`
(machine-written TOML, committed, like Cargo.lock).

The project file is `.af/af.toml` at the git toplevel. Every table is optional; an empty file
works with defaults. Named tables for entities (`[worker.x]`, never `[[worker]]`), at most three
levels deep, a `kind` on every entity, references by name.

  version = 1
  [project]   name = \"myrepo\"      min_af = \"0.7\"
  [defaults]  pipeline = \"review\"  task_pipeline = \"implement\"
  [worker.correctness]  package = \"correctness\"

The user file is `~/.config/af/config.toml` (XDG on macOS too). It holds what belongs to the
person and the machine, never to the project: the `[self]` update policy, personal defaults,
trust. Provider logins and cache policy stay in `providers.toml` and `caches.toml` beside it.

`af config show` prints the effective configuration; `--origin` names the file and line each
value came from; `af config paths` lists every file a layer would read. See `af help layers`.";

const LAYERS: &str = "\
Lowest to highest. Tables deep-merge, scalars last-wins, arrays replace.

  built-in    the binary's defaults — everything has one
  system      /etc/af/config.toml and /etc/af/conf.d/*.toml
  user        $XDG_CONFIG_HOME/af/config.toml and conf.d/*.toml (~/.config/af)
  directory   .af/af.toml in every directory ABOVE the repository's git toplevel, from / down;
              nearer wins. Outside a repository: every ancestor of the current directory.
  project     .af/af.toml at the git toplevel (found by walking up; nothing below it is read)
  local       .af/af.local.toml beside it, gitignored — this checkout's overrides
  environment AF_<TABLE>__<KEY>, e.g. AF_SELF__AUTO_UPDATE=never

A directory layer lets one `~/work/.af/af.toml` name a gateway provider for every checkout
beneath it, and can carry `workers/` packages shared by the tree. Executable-bearing keys in
directory and project layers are subject to trust (`af help trust`).

Branch-specific configuration needs no layer: `.af/` is versioned, so the project layer on a
branch is whatever that branch commits. Each git worktree has its own `.af/` copy and its own
`af.local.toml`.";

const ENVIRONMENT: &str = "\
  AF_<TABLE>__<KEY>       override one configuration value (highest layer)
  AF_VERSION              run this installed version regardless of the project's pin
  AF_SELF_OFFLINE=1       never contact a release source: no pin installs, no update checks
  AF_RELEASE_SOURCE       a local directory of releases (<tag>/<asset>) instead of GitHub
  AF_RELEASE_KEY          a minisign .pub file to verify SHA256SUMS with, instead of the embedded key
  AF_PROVIDERS_FILE       absolute path of the provider registry (default: ~/.config/af/providers.toml)
  AF_CACHE_POLICY_FILE    absolute path of machine-local cache policy (default: ~/.config/af/caches.toml)
  AF_DISPATCHED_FROM      set by af when it execs a pinned version; never set it yourself
  XDG_CONFIG_HOME         config          (~/.config)
  XDG_STATE_HOME          state, trust    (~/.local/state)
  XDG_DATA_HOME           installed versions, receipts, man pages (~/.local/share)
  XDG_CACHE_HOME          caches, the release check (~/.cache)
  XDG_BIN_HOME            where the default `af` symlink lives (~/.local/bin)
  NO_COLOR, CLICOLOR_FORCE, EDITOR, PAGER, CI    honoured as their conventions say
  HOME                    required; must be absolute";

const EXIT_CODES: &str = "\
  0    pass — the command succeeded; for `review run`, the Round passed
  1    error — af could not do what was asked; stderr names the entity, the knob, and the fix
  2    usage — the command line was wrong; only the failing command's usage is printed
  3    fail — the review or evaluation ran to completion and did not pass
  4    incomplete — the run ended without a verdict (budget, timeout, missing coverage)
  10   outdated — `af self update --check`: a newer release exists

Under --json the same codes apply and the document on stdout carries the typed outcome.";

const JSON: &str = "\
stdout is the result, stderr is progress. With --json every reading command prints exactly one
JSON document on stdout — no banner, no trailing text — so `af … --json | jq` always works.
Errors under --json are one document on stdout with an `error` field and the same exit code.

Documents carry `kind@version` identifiers where they describe persisted records; a payload shape
change bumps the version rather than reinterpreting an old one. Nothing af prints under --json is
affected by a TTY, colours, or the update notice (which goes to stderr, and only on a TTY).";

const SELF_TOPIC: &str = "\
Layout
  $XDG_DATA_HOME/af/versions/<v>/af          every installed version, immutable once verified
  $XDG_DATA_HOME/af/versions/<v>/receipt.toml what was installed, from where, verified how
  $XDG_BIN_HOME/af  ->  versions/<v>/af       the default; the symlink is the record
  $XDG_STATE_HOME/af/self.toml                activation history, last check
  $XDG_CACHE_HOME/af/self/latest.toml         the cached release check

Pins
  A project's `.af/af.lock` records the af release that wrote it and, per target, the digest of
  that release's archive: the pin names bytes, not just a version. When you run `af` inside such a
  project, af execs that version; if it is absent, af installs it only when the archive it
  downloads matches the lock's digest for this machine's target, and an installed copy must carry
  that digest too. A pin without a digest for this target is never installed on demand:
  `af self install <v>` trusts the release checksums explicitly, and `af onboard --refresh-lock`
  run online records every target. AF_SELF_OFFLINE=1 or `[self] install_pins = false` refuses,
  printing the command to run. AF_VERSION=<v> overrides the pin for one invocation;
  `af onboard --refresh-lock --af <v>` moves the pin to <v> (that release writes the lock).

  Only a released, receipted binary writes a pin; a source build leaves the lock unpinned or the
  existing pin untouched. Nothing older than 0.7.1 — the first release with `af self` — is ever
  dispatched to or made the default: it could not read the lock or update itself back.

What binds bytes
  Under a lock: the lock's digest. Outside one: the release's SHA256SUMS, which every release
  since 0.8.0 signs (minisign) with the key embedded in the binary at build time; an unsigned or
  badly signed SHA256SUMS is refused. `af self status` shows whether this build carries the key;
  a receipt's verified_by says which check installed each version (lock, minisign, sha256sums).

Updates — `[self]` in ~/.config/af/config.toml
  update_check = true      look for a newer release, at most once per check_every
  check_every = \"24h\"
  auto_update = \"notify\"   notify | always | never
  channel = \"stable\"       stable | rc
  install_pins = true
  keep_versions = 3

  The check never runs on the hot path: after a command's result is written, on a TTY only,
  never under CI, --json, or AF_SELF_OFFLINE, af spawns a detached child that refreshes the cache
  and exits. The next invocation prints one line on stderr. With `always`, the child installs the
  newer release and retargets the default; the running process and every pinned project are
  untouched.

Commands
  af self status | update [--check] | rollback | install V | remove V | prune
  af self setup-shell [--write] | uninstall [--purge]
  af self refuses to touch a binary it did not install (brew, cargo install, a launcher).
  remove and prune keep the default and every version a project seen on this machine pins.";

const TRUST: &str = "\
Keys that can execute code or move money — environments, tools, worker commands, hooks — apply
from the directory and project layers only after `af trust` has recorded the path and content
hash of the file (the same gate mise, direnv, and Codex use). Until then such keys are reported
and ignored, never silently applied.

Directory and project layers may never carry an `auth` reference, a Store connection, or any
provider secret reference; such keys are rejected with the file and line.

This release reads no executable-bearing key from the configuration ladder — Workers, Gates, and
tools still come from the committed `.af/` authority pinned in `af.lock` — so the gate has nothing
to guard yet. `[trust] paths` in the user file is accepted and reported by `af config show`.";
