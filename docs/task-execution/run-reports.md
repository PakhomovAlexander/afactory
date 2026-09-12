# Task run diagnostics and domain publication

Task inspection retains every scheduler run, including failures that happen before an Attempt
can be prepared. `af task show` and `af task explain` include `run_reports` in JSON output; text
output shows the latest node errors. Each report binds an exact revision, plan and history
sequence. Entries distinguish completed output receipts, failed diagnostics and suppressed
nodes. Reports explain execution; domain acceptance still requires its typed verifier evidence.

```text
Task node
   |
   +-- reserve -> capture fails ---> release + diagnostic, zero started Attempts
   |
   v
start Attempt -> invoke Worker -> settle charge -> publish Task output
                                                       |
                                                       v
                                              commit domain evidence
                                                |              |
                                               okay          failure
                                                |              |
                                                v              v
                                          downstream       needs-human
                                                               |
                                                         explicit run
                                                               |
                                              retry same publication
                                              using the same output
```

A domain publication hook runs after the paid Attempt is settled. It may idempotently publish
domain evidence through the same Store connection, and must not invoke a Worker or reset the
Task allowance. If its acknowledgement is lost, replay retries that publication before
downstream execution. The durable report retains the failure reason and the Task waits for
recovery. Resuming still checks the captured plan, developer approval, lease and remaining
resources.

The report lists all nodes in compiled order. A completed entry must name its actual published
Task output. A diagnostic retains at most 65,536 Unicode characters and marks truncation.
Historical Tasks without reports remain readable; no reports are invented during inspection.
The contracts are [TaskRunReport@1](../../schemas/task-run-report-v1.json) and
[TaskDiagnostic@1](../../schemas/task-diagnostic-v1.json), with the authority and compatibility
decision in [ADR-0065](../adr/0065-persist-task-run-diagnostics-and-recover-domain-publication.md).
