# `backlog/triage` for this repository

The issue-triage Task of [`docs/design/issue-triage.md`](../../docs/design/issue-triage.md)
§6 (rollout step 2): the business kind `triage` on the installed report profile, an author and
an independent verifier that triage one issue each, and the check that merges them. It is
staged here because a Task Worker may not edit `.af/`
([ADR-0133](../../docs/adr/0133-accept-reports-bound-to-an-exact-source-snapshot.md)).
Installing it is a reviewed hand edit, made after the `kernel/report` install of
[`fixtures/kernel-report/`](../kernel-report/README.md): the triage Task runs on the report
policy that install names as `report_policy = ".af/report-policy.toml"`.

1. Copy `task-packages/backlog/triage-kind`, `task-packages/backlog/triage`,
   `task-packages/backlog/triage-author` and `task-packages/backlog/triage-verifier` to
   `.af/task-packages/backlog/`.
2. Add the four packages to `.af/task-catalog.toml` with the versions and digests in
   `catalog.toml` and the paths under `.af/task-packages/backlog/`.
3. Add `triage = "backlog/triage-kind"` under `[kinds]`, and the `[providers]` entries
   `"backlog/triage-author" = "codex-main"` and `"backlog/triage-verifier" = "claude-main"`, so
   the two triagers run on different Providers and models.
4. Commit the authority.

| Package | What it is |
|---|---|
| `backlog/triage-kind` | `af.task-kind/1`: the kind `triage` on the `report` profile |
| `backlog/triage` | `af.pipeline/1`: the shape of `kernel/report` (author, `report_seal`, `report_check`, verifier, `report_accept`), accepting the kind `triage` |
| `backlog/triage-author` | Codex `gpt-6-astra`, effort medium, `read-source` only: no shell |
| `backlog/triage-verifier` | Claude Opus 5.5, effort medium, `read-source` only, independent of the author |

Each triager has 60,000 tokens and 10 minutes per Attempt. The author returns its
`af/issue-triage@1` document as the one fenced `json` block of its report's `Triage` section,
beside a `Rationale` section and the `Findings` and `Recommendation` sections the installed
report policy requires of every report. The verifier triages the issue itself before it reads
the author's, and ends its `af/ReportEvaluation@1` summary with its own document. Both
instructions quote §4 and §5 of the design verbatim and treat the issue text as untrusted data.

## Budget

§6.1 budgets a triage Task at 150k chargeable tokens, the report pipeline's 8 Attempts and
20 minutes. With the Workers above, the planner admits a Task only from 185,536 tokens (two
60,000-token Attempts and two 32,768-token Provider admissions) and 1,355,000 ms (two
600,000 ms Attempts, two 45,000 ms admissions, the policy's 60,000 ms report check and 5,000 ms
for the start of the run), with a verification reserve of 92,768 tokens, 3 Attempts and
705,000 ms. `example/task.json` uses those limits. Before the runner (step 3) reserves a
Task's allowance in its daily ledger, the owner either accepts them or lowers the Workers'
Attempt allowances and re-pins.

## Check

`catalog.toml` and `contracts.json` are the shared catalog and contract fixtures of these
packages, so the staged copies are checked as they are:

```sh
af catalog test --source . --manifest fixtures/issue-triage/catalog.toml --json
af catalog digest fixtures/issue-triage/task-packages/backlog/triage-author
```

`crates/af/tests/it/task_triage.rs` performs the four steps on a copy of `.af/` (or, where a
human has already taken a step, verifies that what is installed is what is staged), runs that
command, and plans `example/task.json` with zero Attempts: the report shape with the two staged
Workers bound and the six captured sources of §6.2.

## Plan one

`example/` holds a Task for issue #432 and its sources, built by hand from this tree and the
issue on 2026-10-30 in the `af.document-sources/1` shape §6.2 gives: `issue`, `citations`,
`pulls`, `neighbours`, `excerpts` and `rubric`. Once the packages are installed, on a machine
whose Provider bindings admit `codex-main` and `claude-main`:

```sh
af task plan --file fixtures/issue-triage/example/task.json --json
```

The runner of step 3 writes such a sources file per issue and plans the Task the same way.

## Merge the two triages

After a triage Task finishes, `scripts/backlog-check-triage.py` reads the author's report (as
`af task output TASK --port report --format markdown` writes it, or its
`af.document-draft/2`) and the verifier's `af/ReportEvaluation@1` payload, validates both
documents against `schemas/issue-triage-v1.json`, applies the hard rules of §6.3 and the merge
rule, and prints one `af/issue-triage-merged@1` (`schemas/issue-triage-merged-v1.json`):

```sh
python3 -I scripts/backlog-check-triage.py --document report.md --evaluation evaluation.json \
  --repo . --main CAPTURED_MAIN --pulls merged-pulls.json --label bug
```

It exits 0 with the record, 1 with a refusal naming the rule on stderr, or 3 with a hold record
(a `security` class or a `class/security` or `triage/held` label) that sets no label. A
`fixed_candidate` or `partial` resolution needs a commit reachable from `--main` or a pull
request `--pulls` lists as merged. `--owner-label` records an owner override;
`--hand-triage hand-triage-2026-10-30.json` also refuses an agreed resolution state that the
hand triage of the six issues it records contradicts.

The two schemas are not listed by `af schema show`, which ships only the documents `af` itself
prints; no `af` command prints these. The check reads them from `schemas/`.
