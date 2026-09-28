# `kernel/report` for this repository

The report Pipeline and Workers of package R3 of
[`docs/design/research-pipelines.md`](../../docs/design/research-pipelines.md)
([ADR-0126](../../docs/adr/0126-accept-reports-bound-to-an-exact-source-snapshot.md)),
staged here because a Task Worker may not edit `.af/`. Installing them is a reviewed hand edit:

1. Copy `report-policy.toml` to `.af/report-policy.toml`, and add
   `report_policy = ".af/report-policy.toml"` to the top of `.af/task-catalog.toml`.
2. Copy `task-packages/kernel/report`, `task-packages/kernel/analyst` and
   `task-packages/kernel/report-verifier` to `.af/task-packages/kernel/`.
3. Add the three packages to `.af/task-catalog.toml` with the versions and digests in
   `catalog.toml` and the paths under `.af/task-packages/kernel/`, and `[providers]` entries
   `"kernel/analyst" = "claude-main"` and `"kernel/report-verifier" = "codex-main"`, so the
   verifier runs on a principal distinct from the analyst's.

`kernel/analyst` is Claude Opus 5.5 at high effort with `read-source` and `execute-checks`
(a shell in a clone that seals nothing back), 1.5M tokens and 3 hours per Attempt.
`kernel/report-verifier` is GPT-6 Sol at high effort with `read-source` only. The built-in kind
`report` needs no kind package.

`catalog.toml` and `contracts.json` are the shared catalog and contract fixtures of these
packages, so the staged copies are checked as they are:

```sh
af catalog test --source . --manifest fixtures/kernel-report/catalog.toml --json
```

`crates/af/tests/task_report.rs` performs the three steps on a copy of `.af/` — or, where a
human has already taken a step, verifies that what is installed is what is staged — runs that
command, and plans a `kernel/report` Task with zero Attempts.
