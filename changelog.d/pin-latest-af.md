- This repository's own `.af/af.lock` pins `0.10.0` (it still pinned `0.9.0-rc.6`). From now on
  the release workflow pins each release it publishes, and `make check` fails when the pin falls
  more than one release behind `CHANGELOG.md`
  ([ADR-0138](docs/adr/0138-the-repository-pins-its-newest-release.md)).
