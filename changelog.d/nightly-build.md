- The owner's nightly build: `scripts/nightly/build.py` chooses the newest green commit of main
  (walking back over red ones and skipping a night when main has not moved), builds
  `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` in the laboratory's musl image as
  the release does (that image's `musl` stage now installs `aarch64-unknown-linux-musl` beside
  `x86_64-unknown-linux-musl`), smokes and packages each as
  `af-nightly-YYYY.MM.DD-<target>.tar.gz`, signs `SHA256SUMS` with a separate nightly key, and
  records the night as `af.nightly-build/1`.
  Nightlies are tagged `nightly-YYYY.MM.DD` and versioned `<core>-nightly.<YYYYMMDD>`; `install.sh`,
  `af self` and `.af/af.lock` never take one for a release
  ([ADR-0265](docs/adr/0265-build-nightlies-from-the-newest-green-main-on-the-owners-vm.md),
  [`docs/nightly.md`](docs/nightly.md)).
