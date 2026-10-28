- A Task's gate evidence comes from its check report
  ([ADR-0258](docs/adr/0258-task-gate-evidence-comes-from-the-check-report.md), #346). A check
  writes one `tests` record per test run, and af folds them into one, so a gate that runs one
  nextest selection per test class no longer has its `af/TaskCheckReport@1` refused (`line 26 is
  a second tests record`). The gate's failed and flaky tests and the step that stopped a check
  come from its report when the check writes one, and from the text scan of its output only for
  a check without one (absent or refused): the failing fixtures a preflight prints are no longer
  reported as the Task's failing or flaky tests, a passed gate names only its real flaky tests,
  and `af task show --checks` ends each check line that prints this evidence with
  `evidence: report` or `evidence: output`.
