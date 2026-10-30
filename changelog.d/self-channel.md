- `af self channel [stable|rc|nightly] [--no-update]` shows or switches the release channel: it
  installs and verifies the channel's newest, then writes `[self] channel` into the user layer,
  keeping the file's other keys and comments, and makes that newest the default, saying so when
  that moves back to an older version. A switch that fails leaves the channel and the default as
  they were and names `af self channel <channel>` to try again. The new `nightly` channel
  installs the newest nightly build of main by its date (`nightly-YYYY.MM.DD`, version
  `<core>-nightly.<YYYYMMDD>`, Linux only), verified against a separate nightly key
  (`crates/af/keys/nightly.pub`, or `AF_NIGHTLY_KEY`); a build without that key refuses the
  channel. `af self update`, the release check and `af self status` follow the channel, a
  nightly is never pinned (a lock that pins one is refused at dispatch), an installed nightly is
  reused only as the version asked for, and `install.sh` takes `--nightly` /
  `AF_INSTALL_CHANNEL=nightly`. The check cache is now `af/self-check@4`
  ([ADR-0267](docs/adr/0267-offer-a-nightly-channel-in-af-self.md)).
