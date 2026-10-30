- Issue capture for asynchronous triage
  ([`docs/design/issue-triage.md`](../docs/design/issue-triage.md), rollout step 1): a new
  "Raw note (triaged automatically)" issue template with three fields (`What I saw` required,
  `Evidence` and `Repro` optional); the bug template now requires only `What happened`, the af
  version and `Evidence`; every issue template applies `triage/pending`.
  `scripts/backlog-labels.py` prints (or with `--apply` runs) the `gh label create --force`
  commands for the 24 `class/`, `size/`, `pipeline/` and `triage/` labels, and `--migrate` moves
  open issues from `Agent UX Improvement` to `class/aix` and from `test-perf` to `class/sdlc`; it
  never deletes a label. `AGENTS.md` gains the rule to file a defect met during work raw and
  continue.
