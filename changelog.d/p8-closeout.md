- Performance laboratory p8 closeout. The kernel receipts reader (`af.perf-kernel-receipts/3`)
  now binds each retained baseline, candidate and comparison to the artifact, type and Snapshot
  its Task's result names, and refuses a retained output the result does not name or a named one
  that is missing, as a failed Task leaves it. It recomputes every envelope's content and
  artifact IDs and every whole stream's ID with the kernel's canonical JSON and domains, so a
  changed run under its original IDs is refused. A stream cut to its first and last 32 KiB says
  so, and its omitted middle is never claimed rehashed; `/2` is refused. Reports list only failed
  or incomplete cases in their first view, keep passing improvements and non-regressions in the
  Cases table, and say every case passed, by status, instead of calling an improvement within
  tolerance. A measured run with a missing or failed slot states how many planned records it
  holds and what is missing or failed, never that every pair ran. ADR-0186 no longer asserts an
  unmeasured hashing time. `docs/performance-evidence/p8-local/` retains one real same-source run
  of the documented workflow on a macOS arm64 host, from the frozen plan and offline build to the
  report, its linked phase-trace child, archive, history, exact reproduction and link checks; it
  is functional local evidence, not a product speed claim
  ([ADR-0187](docs/adr/0187-bind-kernel-receipts-to-their-task-results-word-reports-by-what-was-admitted-and-retain-a-real-local-run.md)).
