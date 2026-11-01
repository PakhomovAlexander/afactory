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

Each triager has 40,000 tokens and 500,000 ms per Attempt. The author returns its
`af/issue-triage@1` document as the one fenced `json` block of its report's `Triage` section,
beside a `Rationale` section and the `Findings` and `Recommendation` sections the installed
report policy requires of every report. The verifier triages the issue itself before it reads
the author's, and ends its `af/ReportEvaluation@1` summary with its own document whatever its
outcome. Both instructions quote §4 and §5 of the design verbatim and treat the issue text as
untrusted data.

## Budget

A triage Task has §6.1's budget: 150,000 chargeable tokens, 1,200,000 ms (20 minutes) and the
report pipeline's 8 Attempts, as `example/task.json` sets them. The planner reserves two
Attempts per Worker and both Provider admissions within it: 2 × 40,000 + 2 × 32,768 = 145,536
tokens, and 2 × 500,000 ms, two 45,000 ms admissions, the policy's 60,000 ms report check and
5,000 ms for the start of the run, 1,155,000 ms. Its verification reserve is 72,768 tokens,
3 Attempts and 605,000 ms. A triager call in the backfill of 2026-10-30 cost about 20,000 tokens
and 20 seconds.

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
`af task output TASK --port report` writes it with `--format markdown` or
`--format canonical-markdown`, or its `af.document-draft/2`) and the verifier's
`af/ReportEvaluation@1` payload, validates both documents against
`schemas/issue-triage-v1.json`, applies the hard rules of §6.3 and the merge rule, and prints one
`af/issue-triage-merged@1` (`schemas/issue-triage-merged-v1.json`):

```sh
python3 -I scripts/backlog-check-triage.py --document report.md --evaluation evaluation.json \
  --repo . --main CAPTURED_MAIN --pulls merged-pulls.json --label bug \
  --task-id TASK --document-id DOCUMENT_ID --evaluation-id EVALUATION_ID \
  --bundle-sha256 BUNDLE_SHA256 --policy-version POLICY_VERSION --event-id EVENT_ID
```

It exits 0 with the record, 1 with a refusal naming the rule on stderr, 2 with a usage error
when one of the seven identity flags is missing (nothing on stdout), or 3 with the private hold
record `af/issue-triage-hold@1` (`schemas/issue-triage-hold-v1.json`), which needs no identity
flag and sets no label. A `class/security` or `triage/held` label, given with `--label` or
`--owner-label`, holds the issue before anything else is read or checked, as does a `security`
class from either triager. Every other outcome needs the verifier's own `Triage` block in its
evaluation; the author's document never stands in for it. A `fixed_candidate` or `partial`
resolution needs a commit reachable from `--main` or a pull request `--pulls` lists as merged; a
commit written as `commit <sha>`, as a code span or alone in an evidence entry counts whatever
its mix of digits and letters. `--owner-label` records an owner override;
`--hand-triage hand-triage-2026-10-30.json` also refuses an agreed resolution state that the
hand triage of the six issues it records contradicts.

`af schema show` prints the three schemas, and `af help json` names them.
