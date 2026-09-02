# Consumer fixtures — policies real consumers pin

A consuming repository pins one `af` release and owns its review policy. When the pipeline
format changes — a new required Ledger output, a new node field, a lock schema change — the
release still builds, still checksums, and breaks every consumer at its next `af review plan`.
That happened at `v0.7.0`: the Afactory hub's `.review/pipelines/heavy.toml` was rejected with
`Ledger node must declare a review.kernel/DemandSet@1 output` and nothing noticed for a week
(hub PR #11 fixed the policy; product issue #45 asked for this fixture).

Each directory here is one consumer-shaped policy, copied verbatim:

| Fixture | Mirrors | Layout |
|---------|---------|--------|
| `hub/` | `PakhomovAlexander/afactory-hub` `.review/` as of hub PR #11 (2026-09-02) | legacy `.review/`: two model reviewer nodes (architecture, performance), two gate checks, digest-pinned packages, `review.lock` |

## What checks them

- `crates/reviewctl/tests/consumer_compat.rs` materializes every fixture into a temporary git
  repository and runs the built `af review plan --policy-rev HEAD --base HEAD --candidate HEAD
  --json` against it. It runs inside `make check`. A second test removes the `DemandSet@1`
  Ledger output from the hub fixture and asserts the plan is rejected — the exact break this
  fixture exists to catch must stay detectable.
- `check.sh <af-binary>` does the same with an arbitrary binary. The release workflow runs it
  with the freshly built artifact on every target before the release leaves draft; `make
  consumer-check` runs it locally against `target/release/af`.

Planning is token-free and creates no Campaign state, so both checks are free to run anywhere.

## Refreshing a fixture

Copy the consumer's policy directory in verbatim — lock included — and update the table above.
`cp -R <hub>/.review fixtures/consumers/hub/` is the whole procedure for the hub. Never edit a
fixture to make a test pass: a format change that rejects a fixture is a compatibility break, and
the fix is to migrate the fixture *and the consumers it mirrors* deliberately (issue #46 asks
`af onboard` to do that migration), then refresh the fixture from the migrated consumer.
