- Select end-to-end validation from the resolved release tag. RCs skip e2e and its Linux container
  probes. Stable releases require e2e on Linux and macOS and require the Linux probes, including
  with `release:fast`. Release PRs continue to select checks from their labels on Linux and macOS.
  The tagged validation plan, check summary and release notes record the e2e decision.
  See the [decision](../docs/adr/2026-11-25-select-tagged-release-e2e-by-version.md).
