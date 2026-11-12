# Issues 796/797: native help-width repair

Implementer evidence on 2026-11-11, from captured source Snapshot
`sha256:80b9433b7bfdfd6bbcc766c55ee7a0e1aeb427ba11516da1bd8579da74e3b1f3`.
This bounded repair changes only line wrapping in `docs/scenarios.md` and adds this record.
Raw Markdown, interval scheduling/watch, startup/stop serialization and native retention/capacity
code, schemas, fixtures, limits and execution authority remain unchanged. No behavioral ADR is
needed for this presentation repair.

## Historical failure and repair

The supplied `ants-796-797-reviewed-fixes-20261111` gate failed
`help_hygiene::no_help_line_is_wider_than_100_columns` both ordinarily and in isolation. It reported
101- and 112-column rendered lines for the observation-time sentence ending in
`--stale-after-ms` and the authored-example link sentence. This repair reflows those sentences,
the retention version boundary, its decision/evidence links, and the roadmap journal sentence
without changing their words or the 100-column assertion.

The preceding Markdownlint MD012 repair in `fixtures/scenarios/results/README.md` is preserved.
The [retention implementation evidence](796-native-retention.md),
[recorded defect continuation](796-797-recorded-defects.md),
[earlier follow-up](796-797-followup.md) and their linked historical records remain intact.
Earlier failures are not relabelled as passes. The retention record supplies the sustained
16 MiB, accounting, uncertain recovery, capacity and native process/schema test commands; the
recorded-defect continuation supplies the controlled stop-race and authored-Markdown evidence.
Their passes do not establish independent acceptance of this candidate.

## Fresh validation environment

Darwin/arm64, pinned Rust 1.88.0 selected first on PATH, offline locked Cargo dependencies and
checkout-local `target/`. Private scratch is `target/help-width-repair/tmp`; private HOME/XDG
roots are below `target/help-width-repair/home`. `CARGO_HOME` is
`target/help-width-repair/cargo-home`, containing links only to existing registry/git caches,
with no credentials or machine configuration copied. Native tests use `crate::common::af()`
isolation. The captured tree has no Git metadata, so `git status --short` returned 128; no Git
repository or publication state was created to compensate.

## Fresh results

The native regression ran against the final help source, enumerating every command/topic through
its existing private-process harness; the assertion and 100-column limit are unchanged.

```sh
cargo test --offline --locked -p af --test it \
  help_hygiene::no_help_line_is_wider_than_100_columns -- --exact --nocapture
```

Exit **0**: **1 passed, 0 failed**, 1,039 filtered out; test execution 4.19 seconds, excluding
compilation. This fresh pass does not change the historical ordinary/isolated gate failures.

The actual cached `markdownlint-cli2@0.22.1` CLI (markdownlint 0.40.0), from the repository root,
uses the unchanged `.markdownlint-cli2.jsonc`:

```sh
node /Users/apakhomov/.npm/_npx/38d773fe1452947e/node_modules/markdownlint-cli2/markdownlint-cli2-bin.mjs \
  docs/scenarios.md docs/implementation-evidence/796-797-help-width-repair.md \
  fixtures/scenarios/results/README.md
```

Exit **0**: 3 files, **0 errors**, including the unchanged prior MD012 repair. A final source-wide
CLI run enumerates Markdown with `rg --files --hidden -g '*.md' -g '!target/**' -g '!.git/**'`,
passing those paths as separate CLI arguments. Exit **0**: **499 files, 0 errors**. This covers
all source Markdown in the retained candidate as well as both files authored in this repair.

## Acceptance boundaries

The unchanged local fast gate is **NOT RUN by this implementer**; the prescribed pipeline owns it.
Independent final-candidate build/private staging, sustained retention/capacity and recovery tests,
scheduler/watch, authored Markdown and schema regressions, `ImplementationTestEvidence`, bugs
review and evaluator conclusion are **NOT AVAILABLE in this role** and remain required. This
record claims no resolution verdict for the three substantive findings and no whole-Task acceptance.
No broad convergence campaign, external issue/message, credential use, publication, machine
configuration, production activation or live recovery monitor change occurred. Issue 799 remains
for later owner design.
