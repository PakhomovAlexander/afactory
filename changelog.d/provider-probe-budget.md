- The Codex status and subscription probes allow 15 seconds instead of 5. On a loaded machine
  (a full build, an IDE indexing) starting `codex` alone took long enough that `af task run`
  refused a healthy provider with `Codex subscription probe timed out after 5 seconds`, and
  provider tests failed in Task gates. Claude's structural probe already allows 30 seconds.
