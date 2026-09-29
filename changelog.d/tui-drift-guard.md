- The browser's Pipelines pane reads every committed pipeline, the catalog and their drift
  with the same two batched git calls as the Workers pane, instead of two git processes per
  pipeline. Tests assert both panes spawn as many git processes for 30 entries as for 2, so
  a per-item read cannot come back unnoticed.
- A declaration marked `skip-worktree` or `assume-unchanged` in the index is still marked `*`
  when its working-tree copy differs from `HEAD`: `git diff` does not compare such files, so the
  panes hash those, and only those, against the committed blob.
