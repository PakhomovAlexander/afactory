# ADR-2026-11-09-split-pr-additions-by-crate-and-code-kind: Show PR additions and deletions in one hierarchy

**Status:** proposed (2026-11-09). Supersedes the counting, grouping, report shape and name escaping of
[ADR-0195](0195-state-the-lines-a-pull-request-adds-per-directory-and-check-them.md).
The automatic maintenance of [ADR-0270](0270-let-ci-maintain-the-lines-added-block.md) stays in force.

## Context

The owner wants one readable table showing where code grew or shrank, including each crate's
main code, tests and other files. Crate tests belong within their crate's subtotal. Rounded
human figures cannot prove exact arithmetic; exact commit regeneration owns numeric correctness.

## Considered options

- A second crate table or global crate-tests bucket: rejected because both obscure the hierarchy.
- Structural arrows or bars in the share cell: rejected to keep hierarchy and shares readable.
- Separate addition and deletion bars on a common sibling scale: chosen for comparable lengths.
- Shared-source categories or parsing inline tests: rejected in favor of deterministic path counts.
- One hierarchical table with both counts: chosen.

## Decision

1. Keep the `**Lines added: <figure>**` title and `af-pr-lines:v1` refresh markers. Emit exactly
   one table with headers `Area | Changes | + | − | Share of additions`. Changes use `+382/−10` with
   U+2212 minus and no spaces. There are no structural arrows, tabs or displayed directory suffixes.
   Root rows and crate parents are bold in all nonempty cells; terminal children are plain.
   Separate `+` and `−` bar columns use one common scale per sibling group: the largest exact
   added or deleted count among those siblings. Children use their own sibling scale, not
   their ancestor's scale. Bars are at most twelve full cells with Unicode eighth blocks
   (`▏▎▍▌▋▊▉█`), rounded half up with integer arithmetic. Positive counts get at least a `▏`
   sliver; zero counts have truly blank bar cells, even on bold rows. Deletion-only rows
   have a blank `+` cell and visible `−` bar. This presentation adds no JSON fields or version.
   Four `&nbsp;` entities per depth visibly indent children before any bold markup.
2. Root rows partition additions and deletions independently by actual first path component.
   `crates` includes all crate tests. Crate children have lowercase `main`, `tests`, `other`
   leaves; direct crates files remain plain file children without leaves. Rust paths within any
   `tests` directory inside a crate, or named `tests.rs`, `test_*.rs`, `*_test.rs`, are tests.
   Remaining Rust under `src/` is main. Residual manifests, config, fixtures, `build.rs` and
   ordinary examples are other. Inline `#[cfg(test)]` in ordinary source and common/shared
   source remain main. There is no shared category: this measures paths, not Rust semantics.
3. Use the same `git diff --numstat -z -M BASE...HEAD` merge-base range for both counts, with
   rename destination attribution. Binary None/None and 0/0 entries are absent. A row appears
   when either count is positive. Each parent's exact additions and exact deletions reconcile
   separately with its children, as do roots with PR totals. Subtotals never add to totals again.
   Siblings sort by descending additions, descending deletions, then raw identity.
4. Shares use immediate parent additions, or PR additions for roots. A zero child of a positive
   parent shows `0%`; a zero parent denominator is undefined (`—`). Percentages round half up;
   positive shares below 0.5% show `<1%`. Figures are exact below 1000, round half up to 0.1k
   below 10k and 1k thereafter. Deletion-only reports retain the full table with title
   `**Lines added: 0**`. Only zero additions and zero deletions yield `**Lines added: none**` alone.
5. Display names omit internal directory suffixes. When sibling bare names collide, the
   directory keeps its name and only the file becomes `name (file)`. Allocate `(file2)`,
   `(file3)`, etc. until unique, reserving all literal bare sibling names and already allocated
   labels in raw identity order. Apply name escaping after this display-only projection; exact
   JSON identities, ordering and both counts stay unchanged, including root type conversions.
   Names escape Markdown/HTML syntax, backslashes, whitespace and tree arrows as numeric HTML
   entities. Names containing whitespace have visible quote delimiters, so leading NBSP cannot
   impersonate hierarchy depth. Literal `&nbsp;` becomes `&#38;nbsp;`, never structural
   indentation. Each subtree is contiguous; siblings can be reordered without changing the parsed report. Bold depth-one
   rows require children and distinguish crate parents from direct files without display slashes.
6. JSON writes only `af/PrLines@4`, defined by `schemas/pr-lines-v4.json`: exact `added`, `deleted`
   totals and ordered `rows` with raw `name` (retaining internal directory suffixes), exact
   nonnegative `added`, `deleted`, and `children` (empty for leaves). Every node has at least one
   positive count. Remove obsolete unmerged v3; no old aliases or compatibility readers exist.
7. The checker regenerates the entire report from exact PR `base.sha...head.sha`, including
   deleted counts and nested changes. Missing or stale old shapes refresh through the existing
   bounded mechanism. Shape parsers reject malformed depth, parent, bold, count, share and
   bounded bar glyph forms, including nonblank zero bars and blank positive bars. The previous
   three-column human format is refused. Canonical regeneration owns exact sums, missing rows,
   numeric truth and each exact bar length independently. Python and Rust parsers have parity
   tests, and CLI help describes the complete block.
8. Base-checkout generation, refresh authority, Task reports and publication boundaries stay
   unchanged. No workflow or `.af` authority changes belong to this design.

## Consequences

Readers see additions and deletions in their directory and crate context in one table. JSON
consumers must use the current version. Counting remains one Git query and at most three levels
of aggregation; regression fixtures are local and token-free.
