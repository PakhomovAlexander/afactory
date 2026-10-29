- `af self channel [stable|rc|nightly] [--no-update]` shows or switches the release channel: it
  writes `[self] channel` into the user layer, keeping the file's other keys and comments, then
  installs the channel's newest and makes it the default, saying so when that moves back to an
  older version. The new `nightly` channel installs the newest nightly build of main by its date
  (`nightly-YYYY.MM.DD`, version `<core>-nightly.<YYYYMMDD>`, Linux only), verified against a
  separate nightly key (`crates/af/keys/nightly.pub`, or `AF_NIGHTLY_KEY`); a build without that
  key refuses the channel. `af self update`, the release check and `af self status` follow the
  channel, a nightly is never pinned, and `install.sh` takes `--nightly` /
  `AF_INSTALL_CHANNEL=nightly`. The check cache is now `af/self-check@4`
  ([ADR-0267](docs/adr/0267-offer-a-nightly-channel-in-af-self.md)).
