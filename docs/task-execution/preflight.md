# Advisory orchestration preflight

`scripts/task-preflight.py` makes three planning mistakes visible before an orchestrator
dispatches work: changing the original source to the retained candidate, overlooking declared
admission/implementation/verification allowances, and scheduling required evidence after its
consumer. It reads exports and an operator-authored checklist. It never invokes a process,
contacts a Provider, accesses the Store, changes a plan, or grants execution authority.

This is a repository helper, not a new `af` command or a second admission engine. Existing
plan confirmation, compiler checks, current Provider admission, protected reserves and
independent goal acceptance remain authoritative. An export can be edited or stale; hashes in
the report identify the exact input files, not their authenticity.

## Usage

Capture a plan normally, then export its detailed inspection. Keep these run files outside the
repository, as with Task files and other execution state:

```sh
af task explain TASK_ID --repo /path/to/repository --json > /tmp/task-explain.json
python3 scripts/task-preflight.py \
  --inspection /tmp/task-explain.json \
  --checklist /tmp/task-checklist.json
```

Add `--json` for a machine-readable report. Exit 0 means no structural conflicts were found in
the supplied mapping, 2 means a conflict or unknown needs review, and 1 means invalid input.
None means accepted, approved or safe to dispatch. The tool does not execute another command
based on its result. Use `--now-unix-ms` only to reproduce a report at an explicit historical
clock; the real AF deadline still applies at dispatch.

The helper consumes `af/task-inspection@11` with `plan` and `graph` present, as produced by
`af task explain --json`. `af task show` without these sections is insufficient. Unsupported
formats are rejected rather than silently guessed. It validates the fields it uses, not the
entire AF schema or CAS history.

## Checklist

Copy the exact plan identity and the original source Snapshot identity from a trusted capture.
Preserve that original identity across continuations; do not automatically replace it with
whatever the latest candidate calls its source. Snapshot IDs and source artifact IDs are
different fields and must not be interchanged.

This illustrative checklist uses the native pagination fixture's node names. Replace all
identities and names with those from the actual plan:

```json
{
  "schema": "af-preflight-checklist/1",
  "plan_id": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "original_source_snapshot_id": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "criteria": [
    {
      "id": "pagination-bounds",
      "requirement": "Preserve pagination bounds",
      "obligation": "verified",
      "check_name": "pagination",
      "producer": {"node": "root.nodes.check", "port": "result"},
      "consumer": {"node": "root.nodes.evaluate", "port": "checks"}
    }
  ]
}
```

Each criterion names a captured acceptance obligation and the exact producer output bound to
the verifier's input. The helper checks declared ports, data dependency and order, checks that
the verifier feeds the obligation's coverage, and flags conditional evidence. For a Check
producer, `check_name` identifies the particular declared check; a generic gate cannot stand
in for an omitted twenty-run proof. Different Check/verifier source addresses are reported
as unknown: they may resolve to the same Snapshot through aliases, which AF must validate.
Optional inputs and conditions along the verifier-to-coverage path also remain unknown.
Normal AF plans often have these guards: an unknown here requests interpretation of the
runtime contract, not removal of a valid guard or a finding that AF's acceptance is broken.

The mapping deliberately requires a direct producer-output to consumer-input binding. A more
complex transform or aggregated proof needs human inspection; merely appearing earlier in
the graph is not evidence delivery. The report cannot prove that a check command performs
what its name claims, that every prose requirement was mapped, that conditional branches will
execute, or that the evidence is sufficient. It is a planning checklist, not a verifier.

For a retained candidate, add `retained_candidate_snapshot_id`. If it equals the captured
source, the report flags possible candidate-as-source rerooting. Optionally provide
`--review-plan /tmp/review-plan.json`, exported by `af review plan --json` for the intended
base and candidate. Only an exact match of both Snapshot identities permits reporting that
export's diff as nonempty. An empty diff is flagged; unavailable/mismatched evidence never
becomes a successful diff check. Replay of the retained bytes into the next implementation
remains explicitly unverified, even with a matched nonempty diff.
The consumed format is `af/review-plan@1`, specifically `resolved.base_snapshot_id`,
`resolved.candidate_snapshot_id` and `subject.kind`, `subject.empty`, `subject.changed_paths`.
A real capture is retained in `fixtures/task-runtime/preflight/review-plan.json`. The helper
does not validate the full Review plan schema. AF refuses native empty-Diff planning itself;
the helper also rejects an empty summary supplied in an edited export.

## Budget explanation

If native planning refuses with `Task cannot protect the compiled verifier allocation`, its
reason lists required and available verification tokens, Attempts and wall milliseconds,
identifies the exceeded resources, and lists each protected node's contribution. Provider
admission is included when protected by the compiled graph. Contributions multiply each
node's declared allowance by its protected Attempt count; optional retries are excluded.
These sums are reservation requirements, not predicted elapsed time or actual spend, and
parallel execution does not discount them. Review the request's reserve against the complete
allocation before authorizing another run. A rejected selection is not an executable plan
or a Task inspection for this helper.

The report separates the captured cap, exact cumulative charge, begun Attempts, absolute
deadline, configured verification reserve and each compiled node's allowance. Provider
admission, implementer and verifier roles are named separately. Decimal charged tokens are
never converted to floating point. Configured verification is part of the total cap, not an
extra allocation to add to it.

`headroom_before_live_reservations` is cap minus recorded charge, not spendable tokens.
For a pristine plan (ready/submitted, zero begun Attempts and charge, no execution records)
the helper flags a non-verification Worker reservation that cannot coexist with the full
configured reserve. Operator kind, not the free-form slot role name, selects these rows.
After execution begins it does not reconstruct still-required
verification, held reservations, retries or remaining branch costs: these are unknown from
this projection and AF's budget machinery remains authoritative. Per-node allowances are
listed, not summed as mandatory future spend; conditional work, token scopes and already
finished nodes make such a total misleading. Provider model usability still requires native
admission. A structurally connected checklist is never permission to lower a reserve.
Owned-child templates, token-scope caps, Integration allowances and experiment slots are
printed separately without summing overlapping caps. Dynamic work produces an explicit
budget unknown; its scheduler admission is not reconstructed by this helper.

The underlying budget semantics are defined in
`crates/review-attempt/src/task_budget.rs`: `prepare` checks each reservation plus protected
verification against the remaining Run budget, and `begun_attempts` includes begun and retired
Attempts. The helper's configured-reserve comparison is a conservative explanation rather
than a replay of `protected_after`, live holds or scope admission.

## Verification

`make preflight-check` runs offline tests using the recorded native inspection fixture, with
regressions for changed source, stale plan, mismatched/empty diff, omitted named proof,
evidence after evaluation, absent data binding, different source, missing coverage,
conditional evidence, exact large token counters, protected reserve and malformed input.
The same tests are included in `make check`.
