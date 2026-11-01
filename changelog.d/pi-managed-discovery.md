- Pi setup discovers the SDK through the standard managed launcher without executing it,
  rejects unsafe version/entrypoint paths, and prints failed setup details on stderr while
  preserving JSON stdout. Cancellation fixtures bound readiness and retain usage/reaping checks.
- Retain the original nested-write issue and source-bound historical measurement in the
  [Pi validation record](docs/design/pi-provider-validation.md), alongside current checks;
  no paid live smoke is claimed. The architecture remains proposed
  [ADR-0274](docs/adr/0274-own-the-pi-sdk-host-and-confine-its-tools.md).
