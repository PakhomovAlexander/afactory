- `executor comparison-report`, `comparison-export` and `comparison-verify` now refuse a core
  aggregate that names another comparison ID or frozen plan digest than its declaration, and
  one that leaves out a complete, admitted partition of this comparison by labelling it
  `refused`. Each partition's inclusion and verification are derived from its binding,
  terminal state and admission, as the aggregate writer derives them, so the check holds even
  when the diagnostic children's parent receipts were resealed to the altered aggregate. The
  report is incomplete with verification `refused`. A partition that is incomplete, foreign
  or not admitted still reports honestly as incomplete
  ([ADR-0240](docs/adr/0240-execute-performance-experiments-independently-on-a-locked-vm.md)).
