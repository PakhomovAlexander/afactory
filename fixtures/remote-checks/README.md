# Remote Check fixtures

Offline fixtures for Remote Checks
([ADR-0139](../../docs/adr/0139-run-a-declared-check-through-a-gate-pull-request.md)). Nothing
here holds a credential, a push URL or a mapping path. Job logs are written by the tests that
serve them, not recorded here.

- `evidence/` holds one `af/RemoteCheckEvidence@1` payload per state and reason the schema
  parity suite (`crates/review-core/tests/schema_parity/remote_check.rs`) checks against both the
  schema and the Rust validator, with the status each derives.
- `github/` holds recorded GitHub REST documents — workflow runs for a head commit and the jobs
  of one run attempt — that the fake `gh` of
  `crates/review-pipeline/tests/it/remote_checks.rs` serves. `@HEAD@` and `@TASK@` stand for the
  gate head commit and the Task ID, which the fake substitutes from its local bare repository.
