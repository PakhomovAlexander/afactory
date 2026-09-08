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
| `hub/` | `PakhomovAlexander/afactory-hub` policy as of 2026-09-02 (single correctness reviewer), moved to `.af/` by `af onboard --migrate --apply` on 2026-09-06 | `.af/`: one correctness Worker mirroring this repo's `.af/workers/correctness`, two gate checks, `af.lock` with Worker and pipeline pins and no `af` pin (a source build wrote it) |

The hub fixture's markdownlint Check was corrected in place on 2026-09-08, and re-pinned with
`af onboard --refresh-lock`: it declared `npx --yes markdownlint-cli2@0.22.1`, the live Gate fetch
the kernel had just removed from its own pipeline, and its header cited `.review/` paths and a
lock generator that no longer exists. A fixture is read as the exemplar of a consuming repo — it
may not teach what the project tells consumers not to do. The consumer it mirrors is expected to
follow; the *shape* under compatibility test (pipeline `version = 2`) is unchanged, which is what
this fixture exists to hold.

The hub's previous `.review/` layout survives only as the migration test fixture under
`crates/reviewctl/tests/fixtures/legacy-hub/`; from `v0.8.0` on (unreleased at this commit) that
layout is no longer read for new Campaigns (ADR-0043), so it cannot be a consumer fixture.

## What checks them

- `crates/reviewctl/tests/consumer_compat.rs` materializes every fixture into a temporary git
  repository and runs the built `af review plan --policy-rev HEAD --base HEAD --candidate HEAD
  --json` against it. It runs inside `make check`. A second test removes the `DemandSet@1`
  Ledger output from the hub fixture, re-pins the lock, and asserts the plan is rejected — the
  exact break this fixture exists to catch must stay detectable.
- `check.sh <af-binary>` does the same with an arbitrary binary. The release workflow runs it
  with every freshly built archive's binary before the release is published; `make
  consumer-check` runs it locally against `target/release/af`.

Planning is token-free and creates no Campaign state, so both checks are free to run anywhere.

## Refreshing a fixture

Copy the consumer's policy directory in verbatim — lock included — and update the table above.
`cp -R <hub>/.af fixtures/consumers/hub/` is the whole procedure for the hub; a fixture's lock
should carry no `[af]` pin, or the consumer check would dispatch to that release instead of
planning with the binary under test. Never edit a fixture to make a test pass: a format change
that rejects a fixture is a compatibility break, and the fix is to migrate the fixture *and the
consumers it mirrors* deliberately, then refresh the fixture from the migrated consumer.
