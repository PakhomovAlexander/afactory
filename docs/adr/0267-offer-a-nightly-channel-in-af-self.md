# ADR-0267: Offer a nightly channel in af self

**Status:** proposed (2026-10-29).

## Context

`af self` knew two channels, `[self] channel = "stable" | "rc"`, and `install.sh` the same two.
The owner wants to live on builds of main: one command that says so, and moves back. Nightlies
are built outside the release train, on the owner's machine: a GitHub pre-release tagged
`nightly-YYYY.MM.DD` whose assets are `af-nightly-YYYY.MM.DD-<target>.tar.gz` (the release
layout), `SHA256SUMS` in the release format and `SHA256SUMS.minisig` signed with a separate
nightly minisign key. A nightly binary reports the version `<core>-nightly.<YYYYMMDD>`, for
example `0.14.0-nightly.20261029` while main is `0.14.0-rc.3`. Its tag says its date, not its
version: the version is known only once its binary says it. Nightlies are built for Linux only.

## Considered options

1. Rank nightlies with releases by semantic version on one pre-release channel. Rejected: a
   nightly of `0.14.0` sorts below `0.14.0-rc.3` (`nightly` < `rc`), and its tag carries no
   version, so the ranking would need every nightly downloaded first.
2. Sign nightlies with the release key. Rejected: the release key's secret lives only in the
   release workflow; a key on the owner's machine that could sign a release would widen what a
   compromise of that machine can publish as a release.
3. A third channel whose newest is the newest nightly by the date its tag names, verified
   against a nightly key of its own, and a command that switches channels. Chosen.

## Decision

- `[self] channel` takes `stable`, `rc` or `nightly`; any other value is refused, naming all
  three. On the nightly channel the newest is the newest `nightly-YYYY.MM.DD` tag by its date. A
  `vX.Y.Z` or `vX.Y.Z-rc.N` tag is never the nightly channel's, and a nightly is never the
  stable or rc channel's. The archive of every tag is `af-<tag>-<target>.tar.gz`.
- A nightly's installed version is what its downloaded binary answers to `af --version --json`,
  asked before it is installed; it must be `<core>-nightly.<YYYYMMDD>` with the tag's date, or
  the install is refused. An installed nightly is reused for its tag only when it is the version
  asked for, if one is; another version of the same date is refused. Its receipt adds `tag`,
  `channel = "nightly"` and the `commit` the binary reported; a release's receipt is unchanged.
- A nightly's `SHA256SUMS.minisig` is verified against the nightly key alone, embedded at build
  time from `crates/af/keys/nightly.pub` as `AF_NIGHTLY_KEY` (empty without the file), with an
  `AF_NIGHTLY_KEY` environment override like `AF_RELEASE_KEY`'s. A release's sums are never
  accepted under the nightly key, nor a nightly's under the release key. A build without a
  nightly key refuses the nightly channel and every nightly install in one sentence: `this af
  carries no nightly key`. No nightly installs unsigned.
- `af self channel` prints the configured channel and its layer. `af self channel
  stable|rc|nightly` installs the channel's newest, then writes `[self] channel` into the user
  layer with `toml_edit`, keeping the file's other keys and comments, and makes that newest the
  default. The switch is the one place `af self` may move the default to an older version, and
  it says so. `--no-update` only records. Before anything is fetched the switch checks the
  nightly key, that no layer above the user file sets another channel, and, unless
  `--no-update`, a receipted af and an unset `AF_SELF_OFFLINE`; it then lists the channel,
  downloads and verifies its newest (signature, and a nightly's version) and installs it. A
  switch that fails at any step leaves the channel and the default as they were: the user file
  is put back when the default cannot be moved after the write, and the default link when the
  activation cannot be recorded after it moved (the one unrecoverable case, a running binary the
  link replaced, is said); the message names `af self channel <name>` to try again.
- `af self update` follows the channel. On nightly, `--check` exits 10 when a nightly with a
  later date than the running nightly exists, and an af that runs no nightly is always told of
  the newest nightly, with `af self channel nightly` as the command, since its version is
  unknown until downloaded. `af self update` installs a newer nightly and activates it only when
  its version is newer than the running one. The background check asks for nightlies, and under
  `auto_update = "always"` moves only an af that already runs a nightly. `--rc` and
  `--version X` keep their meaning; `--version` of a nightly's version installs that nightly.
- The check cache becomes `af/self-check@4`: `channel` may be `nightly`, whose `latest` is the
  newest nightly's tag, and `latest_prerelease` is never a nightly nor recorded on that channel.
  A version 3 cache is not read (ADR-0145). `af/self-status@2` keeps its fields: its
  `policy.channel` may be `nightly` and its `latest` a nightly's tag, and its `receipt` shows the
  fields every receipt has (a nightly's tag follows from its version; its commit is `commit`).
  The text status adds a `nightly:` line on that channel.
- A nightly is never pinned. `af onboard` writes no pin from a nightly, a lock's install never
  installs one, and dispatch refuses a lock that pins a nightly in one sentence, whether that
  nightly is installed, running or neither, and runs the af it was started as. `.af/af.lock`
  and `scripts/pin-release.sh` are unchanged. A pinned project keeps running its pin whatever
  the channel (ADR-0044). Rollback, remove, prune,
  `keep_versions` and dispatch (ADR-0044, ADR-0105) treat an installed nightly like any
  installed version.
- `install.sh` takes `AF_INSTALL_CHANNEL=nightly` and `--nightly`, ranks nightly tags by date,
  verifies the nightly against `NIGHTLY_PUB`, embedded beside the release key and empty until
  `crates/af/keys/nightly.pub` exists (`AF_INSTALL_NIGHTLY_KEY` overrides it), and refuses the
  channel while it is empty.

## Consequences

- Until the owner adds `crates/af/keys/nightly.pub` and embeds it in `install.sh`, every build
  and the installer refuse the nightly channel; nothing else changes for stable and rc users.
- A user on nightly who switches back to stable gets an older default, by choice and said so.
  `af self update` still never moves the default backwards.
- The release check and `install.sh` list the newest 100 releases; daily nightlies share that
  listing with releases, so the nightly build must keep few enough nightlies for the newest
  release to stay among them.
- A debug build reads `AF_TEST_EMBEDDED_NIGHTLY_KEY` in place of the embedded nightly key, so a
  fixture proves the refusal of a build without one whatever the checkout holds; a release
  binary never reads it. A debug build likewise reads `AF_TEST_RUNNING_VERSION` as the version
  `af self update` on the nightly channel and `af self status` take the running af for, so a
  fixture runs the nightly it activated as that nightly runs.
