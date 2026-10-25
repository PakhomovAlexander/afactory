- Isolate the CLI usage-error fixture from the repository release pin so prerelease source
  checks preserve exact JSON and stderr assertions while the signed pin awaits publication
  ([ADR-0213](../docs/adr/0213-isolate-cli-usage-tests-from-the-repository-release-pin.md)).
