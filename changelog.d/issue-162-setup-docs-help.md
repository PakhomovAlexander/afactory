- Setup help and guides now describe what the CLI does. `af onboard --help` and the generated
  `.af/README.md` say that `--refresh-lock` re-pins every route and oversized-policy pipeline as
  well as the selected one, with their Workers, and drops stale pins. The Task prerequisites say
  that `install.sh` checks the release signature only when `minisign` is on `PATH`, apart from a
  checksum-only install. The preflight guide separates a successful empty-Diff `af review plan`
  from the `af review run` refusal and the helper's `empty_intended_diff` conflict. The
  shared-catalog guide lists all eight installed Task-kind profiles. `af catalog --help` and its
  `--destination` flags say the destination must be an absent, safe project-relative path. No
  behaviour changes.
