- Self-management, the installer and the release tooling are hardened after the 2026-10-20
  security audit. An install a project's `.af/af.lock` asks for now needs the archive to match
  both the lock's digest and the release's signed `SHA256SUMS` (receipt
  `verified_by = "lock+signature"`); a release without signature material is refused under a lock
  as outside one. `[self] install_pins` is `"ask"` (the new default), `"always"` or `"never"`
  (`true` and `false` still mean `"always"` and `"never"`): under `"ask"` a pin that is not
  installed is installed only after a terminal confirmation naming the version, the lock, and
  whether the release is older than the running `af` or a pre-release; without a terminal nothing
  is installed and an older pin's command runs under the current `af` after a notice naming
  `af self install <version>`. `af review plan` never installs. `af self update` without
  `--version` refuses a listed newest that is older than the running release. Releases of a public
  repository are listed and downloaded anonymously over HTTPS with `curl` (pinned to HTTPS and
  TLS 1.2), so updates work without `gh`, which is used only when that is refused, and judged by
  the call it makes rather than by `gh auth status`. `af self uninstall` leaves
  `$XDG_BIN_HOME/af` in place unless it is af's link into its versions, and `af self remove` and
  `prune` delete through the same descriptor-based removal as uninstall, never following a link.
  `install.sh` pins `curl` to HTTPS, refuses without `minisign` unless
  `AF_INSTALL_ALLOW_UNSIGNED=1` (saying so on stderr), and verifies an installed binary again on a
  re-run. The PR-report check exempts a `release/` branch only from this repository itself, never
  a fork's. The release workflow installs `cargo-zigbuild` and `ziglang` from
  `scripts/release-toolchain.txt` with `--require-hashes --only-binary=:all:` (hashes of every
  published wheel, taken from PyPI), and a weekly, on-demand `cargo deny check advisories bans
  sources` workflow with `deny.toml` gates advisories, duplicate crates and sources.
  `SECURITY.md` states what the update check sends and the pinned projects `self.toml` keeps,
  and the release docs the `pin` job's blast radius
  ([ADR-0044](docs/adr/0044-af-manages-itself-and-dispatches-to-the-pinned-release.md),
  [ADR-0045](docs/adr/0045-one-release-train-and-a-pin-that-binds-bytes.md),
  [ADR-0142](docs/adr/0142-carry-the-af-task-report-in-every-pull-request.md),
  [ADR-0152](docs/adr/0152-push-the-release-pin-with-a-deploy-key-that-bypasses-mains-ruleset.md),
  amended in place). Outside this package's own files, the `af help self` text in
  `crates/af/src/topics.rs`, the `af self` and `af onboard` help in `crates/af/src/cli.rs`, and the
  TUI settings golden (`install_pins = "ask"`) change with the new default.
