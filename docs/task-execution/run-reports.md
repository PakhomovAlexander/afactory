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

For captured Review, an expired publication pause has a separate recording-only recovery route.
It requires the same current authority and pins the outputs already published at the failed
report's exact prefix. It can retain the missing canonical Reviewer fact, but the original
deadline still prevents any new invocation, including pure work. An output published after the
report cannot join that recovery scope. This route cannot mark the Task Satisfied; missing
pipeline work remains Inconclusive. Pending Attempts, active Integration, a changed Round or
expired/revoked plan approval refuse. A selected settlement that never reached Task output
publication is outside this route.

Inspection uses `af/task-inspection@8` when history contains this additive recording transition.
Show and explain preserve the recorded events and remain read-only; explain retains the
Inconclusive exit status. See
[ADR-0086](../adr/0086-record-expired-review-publication-without-restarting-work.md).

The report lists all nodes in compiled order. A completed entry must name its actual published
Task output. A diagnostic retains at most 65,536 Unicode characters and marks truncation.
Historical Tasks without reports remain readable; no reports are invented during inspection.
The contracts are [TaskRunReport@1](../../schemas/task-run-report-v1.json) and
[TaskDiagnostic@1](../../schemas/task-diagnostic-v1.json), with the authority and compatibility
decision in [ADR-0065](../adr/0065-persist-task-run-diagnostics-and-recover-domain-publication.md).
