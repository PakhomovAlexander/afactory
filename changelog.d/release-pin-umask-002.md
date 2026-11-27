- Tighten `scripts/pin-release.sh` with `umask go-w` so `git checkout` under an inherited `0002`
  umask never creates group-writable lock files, and create owned release-pin test fixtures under
  a non-group-writable umask in `scripts/test-af-pin.py` with a `umask 002` regression test.
