- The GitHub Scenario adapter now observes required Actions checks from multiple workflows at
  the approved head. It waits for the current run and attempt of each producing workflow.
  Tokens used to observe Actions checks need Actions read access.
  [Decision](../docs/adr/2026-11-25-resolve-required-checks-by-workflow.md).
