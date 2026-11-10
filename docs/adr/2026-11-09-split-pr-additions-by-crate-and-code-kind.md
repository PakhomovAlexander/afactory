# ADR-2026-11-09-split-pr-additions-by-crate-and-code-kind: Render PR additions as one crate hierarchy

**Status:** proposed (2026-11-09). Supersedes the grouping, report shape and name escaping of
[ADR-0195](0195-state-the-lines-a-pull-request-adds-per-directory-and-check-them.md).
The automatic maintenance of
[ADR-0270](0270-let-ci-maintain-the-lines-added-block.md) stays in force.

## Context

The owner wants to see which crate grew and how much is main code, tests or other files,
within one additions table. A separate crate view repeats counts without showing their place
in the directory hierarchy. Moving crate tests into a top-level test bucket obscures the crate
aggregate. The report must partition all additions exactly, including direct files below crates.

## Considered options

- A second crate table: rejected because the report must be one hierarchy in one table.
- A top-level tests bucket for paths inside any tests directory: rejected because those
  additions belong to their actual first path component, including the crates aggregate.
- A shared-source category: rejected by the owner; common/shared source is ordinary main code.
- Parse Rust to count inline tests: rejected because this is a deterministic path metric.
- One table with nonzero crate and code-kind children: chosen.

## Decision

1. Use the existing `**Lines added: <figure>**` title, one `Directory`/`Added`/`Share` table,
   and unchanged `af-pr-lines:v1` refresh markers. With zero additions print only
   `**Lines added: none**` between the markers. No second heading or table is emitted.
2. Top-level rows partition all additions by actual first path component; a root file keeps
   its own name. `crates/` includes every addition below it, including tests. Valid
   `crates/<crate>/<file...>` paths create crate children; direct files below `crates/` create
   file children without leaves. Each crate has only nonzero `main/`, `test/`, `other/` leaves.
   Rust files (`.rs`, case-sensitive) inside any `tests` directory within the crate, or named
   `tests.rs`, `test_*.rs`, `*_test.rs`, are `test/`. Remaining Rust under the crate's `src/` is
   `main/`; everything else is `other/`, including manifests, non-Rust fixtures, `build.rs` and
   ordinary examples. Inline `#[cfg(test)]` in ordinary source and common/shared Rust source
   remain main. There is no shared category.
3. Every parent count equals the exact sum of its children. Only top-level counts contribute
   to the PR total; repeated subtotals never increase it. Every sibling group sorts by descending
   exact count, then raw name. Top-level percentages use the PR total, nested percentages their
   immediate parent. Bars use the largest sibling, twelve cells with eighth blocks and at least
   a sliver. Keep the existing half-up figure/percentage rounding, exact `BASE...HEAD` merge-base
   numstat range, rename destinations and binary zero handling.
4. Directory names have a trailing `/`, files do not. Prefix child names with `↳` and
   grandchildren with `↳ ↳`, each followed by a space, in the first cell. Escape Markdown/HTML syntax, backslashes,
   whitespace and the structural arrow in names as numeric HTML entities. Tree-like path
   components cannot introduce hierarchy. Each subtree is contiguous; siblings may be reordered
   by a reader without changing the parsed report. The generator always uses canonical order.
5. `--json` writes only `af/PrLines@3`, defined in `schemas/pr-lines-v3.json`: an exact `total`
   and ordered `rows`, each with raw `name` (including a directory suffix), exact positive
   `added`, and nested `children` (empty for leaves). The obsolete unmerged v2 schema is removed;
   neither earlier JSON shapes nor the separate Markdown crate table have compatibility readers.
6. The checker regenerates the complete hierarchy from the event's exact `base.sha...head.sha`.
   Missing parents, duplicate paths, empty branches, invalid kinds and invalid nesting fail
   shape checks. Missing or stale nested rows, figures, bars, shares and category-only changes
   are repaired by the existing bounded refresh. Shape-only parsing cannot infer absent rows
   from commits or derive exact integer sums from rounded figures; canonical regeneration owns
   numeric correctness. Python and the Rust PR-body parser accept the same shape, with parity
   tests for valid and invalid hierarchies. CLI help describes the complete single-table block.
7. Base-checkout CI generation, refresh authority, Task reports and publication boundaries stay
   unchanged. This change edits neither workflow nor `.af` authority.

## Consequences

Reviewers see each crate's main-code, test and other growth in its directory context, with no
second view to reconcile. This measures changed paths, not Rust semantics. Counting remains one
Git query and aggregation over at most three levels; regression fixtures are local and token-free.
JSON consumers must use the current versioned tree.
