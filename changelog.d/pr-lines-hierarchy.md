- Render PR additions in one directory table with crate and nonzero `main/`, `test/`, `other/`
  children. Crate tests stay in the `crates/` aggregate; direct files stay file children.
  Shares use immediate parents and bars the largest sibling. Safely escape tree-like names.
  CI repairs the complete hierarchy from exact commits; PR-body parsing and help match it.
  `pr-lines.py --json` emits `af/PrLines@3` with exact totals and nested rows. See the
  [decision](../docs/adr/2026-11-09-split-pr-additions-by-crate-and-code-kind.md).
