# Remote Check fixtures

Offline fixtures for Remote Checks
([ADR-0140](../../docs/adr/0140-run-a-declared-check-through-a-gate-pull-request.md)). Nothing
here holds a credential, a push URL or a mapping path. Job logs are written by the tests that
serve them, not recorded here.

- `evidence/` holds one `af/RemoteCheckEvidence@1` payload per state and reason the schema
  parity suite (`crates/review-core/tests/schema_parity/remote_check.rs`) checks against both the
  schema and the Rust validator, with the status each derives.
- `github/` holds recorded GitHub REST documents — workflow runs for a head commit and the jobs
  of one run attempt — that the fake `gh` serves. `@HEAD@` and `@TASK@` stand for the gate head
  commit and the Task ID, which the fake substitutes from its local bare repository.
- `fake-gh.sh` is that fake `gh`, shared by `crates/review-pipeline/tests/it/remote_checks.rs`
  and `crates/af/tests/it/task_remote_checks.rs`. A test substitutes `@STATE@`, `@BARE@` and
  `@TASK@` and writes the result onto its own PATH; it never contacts a network. It closes the
  gate pull request when asked (`PATCH`, as the cleanup of a finished Task does), and refuses to
  while a `refuse-close` file is in its state directory. It keeps the pull request's labels in
  `labels` and the repository's in `repo-labels`, one name per line, adds and removes one label
  of the pull request (`POST` and `DELETE` on its issue labels, refused while
  `refuse-label-add` or `refuse-label-remove` exists), and records in `labels-created` every
  addition of a label the repository did not have, which af must never make
  ([ADR-0196](../../docs/adr/0196-skip-pull-request-measurements-only-for-an-explicit-no-perf-label.md)).
- `packages/` stages this repository's remote pipeline twins, their catalog pins and the
  `[checks.kernel.remote]` table for a person to install; its README says how.

`pull-list-fails` refuses the pull-request list with HTTP 502; `labels-malformed` returns a
label without a name. Both exercise refusal before a stale exemption can reach a new head.

`pull-record-malformed` returns a pull-request list entry without its required fields. It
must be an unreadable list, never an empty list that permits a head update.
