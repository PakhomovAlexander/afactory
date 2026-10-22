- `af provider auth begin --kind codex` accepts the official device prompt of Codex releases
  other than 0.159.2, such as 0.163.0, instead of refusing it as `unsupported`: the displayed
  version may be any bounded canonical stable `MAJOR.MINOR.PATCH`, while every other byte of the
  prompt must still match exactly, and a changed prompt still fails closed (#305,
  [ADR-0196](docs/adr/0196-recognize-the-codex-device-prompt-by-its-frame-not-its-version.md)).
