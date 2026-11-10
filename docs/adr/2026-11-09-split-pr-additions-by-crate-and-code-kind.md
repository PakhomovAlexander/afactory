# ADR-2026-11-09-split-pr-additions-by-crate-and-code-kind: Split PR additions by crate and code kind

**Status:** proposed (2026-11-09). Amends the report shape and name escaping of
[ADR-0195](0195-state-the-lines-a-pull-request-adds-per-directory-and-check-them.md).
The automatic maintenance of
[ADR-0270](0270-let-ci-maintain-the-lines-added-block.md) stays in force.

## Context

The directory overview puts most Rust code under `crates`, with files inside any `tests`
directory under `tests`. It cannot show which crate grew, or distinguish main code from Rust
tests kept beside it. A crate split must retain the existing total and directory semantics,
use the same exact PR commit range, and avoid counting any addition twice in either view.

## Considered options

- Replace the directory overview with crates: rejected because non-crate paths still matter.
- Parse Rust to count inline test modules: rejected because a partial parser is fragile and
  adds complexity to a path metric.
- Count everything outside test directories as main code: rejected because manifests, fixtures,
  examples and other files would overstate main-code growth.
- Add a disjoint path-based crate view beside the overview: chosen.

## Decision

1. Keep the total, directory grouping, figures, bars, merge-base range, rename destinations and
   binary handling. Add a second view for positive additions under `crates/<crate>/`.
   Rust files (case-sensitive `.rs`) inside any `tests` directory **within that crate**, or
   named `tests.rs`, `test_*.rs` or `*_test.rs`, are Tests. Of the remaining files, Rust files
   under that crate's `src/` are Main code. All others are Other. Test rules take priority.
   Inline `#[cfg(test)]` modules in ordinary source paths remain Main code. The overview's
   existing rule still groups all file types inside any `tests` directory as `tests`.
2. The enriched report is `af/PrLines@2`. Its Markdown extension starts with
   `**Crate additions**`, followed by a table with columns `Crate`, `Main code`,
   `Tests`, `Other`, `Total`, `Share`. Each added line of each crate belongs to exactly one
   category; Total is their sum, Share is of the entire PR total. This table never increases
   the overall total. Sort by descending crate total, then name. Use the overview's figure
   rounding and percentage rules; the crate share has no bar. Omit the entire extension when
   no crate has positive additions, preserving the unchanged directory-only shape in that case.
3. `--json` writes only the versioned object in `schemas/pr-lines-v2.json`: `schema` is
   `af/PrLines@2`, `directories` holds exact overview counts, and `crates` maps crate names to
   exact `main`, `tests`, `other` counts. The old unversioned JSON map is neither emitted nor
   accepted by the schema. The Markdown reader refuses an untitled second table. The
   outer `af-pr-lines:v1` markers remain the unchanged refresh container, not the payload's
   type version; this also preserves empty template markers.
4. Escape name characters that carry Markdown or HTML syntax, backslashes and whitespace as
   numeric HTML entities. This keeps pipes, newlines, tabs and apparent HTML in a single visible
   table cell. Apply the same escaping to directory and crate names.
5. The checker and CLI use one shared entry-to-report renderer. The checker compares the entire
   parsed report, including every crate category, total and share, against the event's exact
   `base.sha...head.sha`. Missing crate rows and category-only differences are stale and are
   repaired by the existing bounded refresh. Shape-only mode cannot infer missing crate rows
   without commits. `af task report --pr-body` accepts the same enriched shape; its parser
   parity fixture includes valid and malformed crate sections. No publication authority changes.

## Consequences

Reviewers can see each crate's main-code, test and other growth without losing the directory
view. This measures changed paths, not Rust semantics; a file with inline tests can still show
all additions as main code. Existing JSON consumers must use the current versioned shape.
Counting stays one Git numstat query with linear aggregation; tests remain functional, use local
fixture repositories and spend no model tokens or network calls.
