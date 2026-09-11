# ADR-0049 — Run Task Workers through shared durable Attempts

**Status:** accepted for the unreleased Task increment, 2026-09-11. Command Workers and code
operators and the Task-file CLI execute through the shared runtime; legacy entry-point cutover
and production model admission remain in progress.

## Decision

`TaskRuntime` implements the existing graph scheduler's dispatch interface. The common Store
owns Task invocations, reservations, Attempt starts, settlements and published ports. Domain
handlers supply operations and validate receipts; they cannot schedule children or mint budgets.
Every initial Attempt and retry rechecks the same exact-plan and developer-approval boundary.

Success selects its output at durable settlement. The scheduler publishes selected ports in
canonical order. Recovery after settlement can publish the saved result under a new writer
lease without paying for another Attempt. An unsettled old-writer Attempt is fenced: started
work retains its full reservation, while provably unstarted work releases its allowance. Late
usage may increase charges after Task completion without changing the completed result.

The Store verifies that paid output envelopes and their artifacts belong to the recorded
Attempt. Invalid outputs retain their usage and cannot feed downstream nodes. Root outputs
equal the normalized Task inputs. Select outputs equal the arm chosen by the recorded condition.
Scheduler guards stay in execution authority and never become undeclared Worker input ports.

Command Worker packages include `input.schema.json` and `outputs/PORT.schema.json` for their
declared protocol ports. Input is `af.worker-request/1`; output is `af.worker-reply/1`. Schemas
resolve only local references, with byte, depth and item bounds. A Worker supplies payloads;
the host assigns canonical producers and Snapshot affinity. A document payload is never parsed
as a Reviewer Result. Captured instructions, declared input artifacts, output contracts and
admitted retry feedback form an exact, durable context manifest. Host runtime directories are
outside the source tree so language caches cannot become implementation output.

An installed environment may own additional output ports. A source-writing Worker has a
`candidate` port of type `af/CandidateTree@1`; Worker JSON cannot fill it. Before that Attempt
is accepted, the host seals its sandbox and captures every candidate byte and Manifest in CAS.
The installed `seal` operator creates `af.task-snapshot/1` lineage and `af/SourceTree@1` output.
These records do not reuse frozen Review Integration's checked `Capture::Derived` meaning.

The installed `check` operator records typed passed, failed or inconclusive receipts for the
current Snapshot. All named checks must exist in captured policy before compilation succeeds.
A conditional evaluator executes only after those checks pass. The additive `accept` operator
assembles exact check and evaluator evidence and retains the final Snapshot on every path:

```text
implement -> durable candidate -> seal S1 -> check S1
                                             |
                      +----------------------+-------------------+
                      | passed               | failed/unavailable|
                      v                      |                   |
                 evaluate S1                 |                   |
                      +----------------------+-------------------+
                                             v
                                      accept exact S1
                              passed / failed / inconclusive
```

`accept` performs no Worker call. It cannot turn a missing evaluator into positive evidence or
omit a mandatory check. It makes negative checks useful typed results while leaving failed or
missing execution distinguishable. Task acceptance is derived from receipts independently of
execution status; review's separate domain conclusion remains a P06 adapter obligation.

Task-file commands release writer leases explicitly, allowing immediate plan/run handoff. Active
runtimes renew short leases; old started Attempts are fenced and conservatively charged on
recovery. Known usage is also recorded before output CAS admission, so a crash there cannot
hide a reported overrun. Completed Tasks may append typed local-delivery records without
reopening execution. Delivery reuses the existing exact-source and worktree recovery code.

Generic model adapters keep Provider framing separate from the typed Worker contract. The host
requires the exact plan binding and the adapter's explicit model/effort, and refuses a rendered
context exceeding its reservation before start. Failed or schema-invalid responses retain usage
and raw evidence. Retry feedback records only a bounded code and exact Attempt/contract IDs.
Native Task-file admission now obtains account identity through the fixed Provider protocol,
captures its canonical digest and rechecks it on resume. A model label or auth-directory name
is not proof. A fixed capability probe is compiled as an internal charged Task node, with
downstream Workers guarded by its successful receipt. Matching slots share a probe; probes
serving verifiers hold protected capacity. The probe has an exact context manifest and receives
no business inputs. Its failure and replay use the same durable Attempt and budget rules.

## Compatibility and remaining work

Legacy implementation and Review histories retain their native identities, original Stores
and charge records. The common Store's read-only links capture stable origin and history-prefix
identities; linking twice is idempotent and never imports legacy charges as new Task spending.
Historical delivery and Review fixtures remain unchanged.

The remaining legacy-entry-point adapters, authenticated developer CLI and supported contained
execution routes must still use these same boundaries. Live Provider probes remain release gates.
`trusted_local` remains an explicit non-isolating environment; it cannot claim container policy
or protect a host approval service from arbitrary model-controlled host code.
