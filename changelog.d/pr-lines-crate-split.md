- PR additions reports retain the directory overview and add each crate's main-code, test and
  other additions, total and PR share. CI repairs the complete report from its exact commits.
  Classification uses paths; inline Rust tests in ordinary source files remain main code.
  `pr-lines.py --json` now emits `af/PrLines@2` with exact directory and crate counts.
  Names containing Markdown, HTML or whitespace render safely. See the
  [decision](../docs/adr/2026-11-09-split-pr-additions-by-crate-and-code-kind.md).
