- A Provider CLI that cannot start is reported as a Provider installation failure instead of
  surfacing later as an unrelated Worker or credential error. A CLI cannot start when it is
  missing, not executable, or exits non-zero on its own `--version` check. `af provider status`
  marks the context `installation_failed` and exits 4, and `af provider setup` returns
  `provider_cli_missing`. Task Provider admission refuses the binding before any Worker is
  dispatched or any Attempt is charged. Each report names the Provider ID, the program path, the
  CLI's own first error line and the fix it suggests. A CLI that breaks after admission fails its
  Attempt as a Provider environment failure, not a model or credential failure
  ([ADR-0127](docs/adr/0127-report-a-provider-cli-that-cannot-start.md)).
