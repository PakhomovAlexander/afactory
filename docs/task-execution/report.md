# Report Tasks

A report Task produces a Document about one exact source Snapshot. This repository's
local Task gate runs the fast validation profile; CI gate pull requests and releases run full. An author reads that
Snapshot and may run commands in a clone of it; the kernel renders the draft and resolves every
repository citation against the Snapshot's Manifest; an independent verifier reads the same
Snapshot and judges the report against the Task's requirements. A report is never delivered:
`af task output` is its only exit. The decision is
[ADR-0133](../adr/0133-accept-reports-bound-to-an-exact-source-snapshot.md); the Document
profile it reuses is described in [Document Tasks](document.md).

```text
Task: report where the cycle's time goes
  inputs: requirements + source Snapshot + captured sources
          (+ comparison, measurements when bound)
                       |
                author Worker              Attempt 1: reads the Snapshot, may run commands
                       |                   in a clone that seals nothing back
             af/DocumentDraft@1 or @2
                       |
                report_seal                renders the Document, bound to the Snapshot
                       |
                report_check               protected Attempt 2: Document checks + every
                       |                   repository citation against the exact Manifest
                  checks passed?
                    /       \
                  yes        no
                   |          |
          independent verifier    negative receipt (no verifier call)
          Attempt 3, same Snapshot
                   \          /
                report_accept              af/ReportVerification@1
                       |
                Task Result: report + verification, no snapshot
```

## Try it

```sh
af catalog init --profile report --destination report-demo --json
cd report-demo
git init
# Review the generated definitions, policy and sources.
git add .
git commit -m 'Configure the report Task starter'
af catalog test --source . --json
af task start --execute --file report.json --json
af task show starter-report
af task output starter-report --port report --format markdown --output report.md
```

The starter runs three command Attempts — author, checks, verifier — with no credential. Its
author reads `README.md` and `report.json` from the committed tree and cites `README.md`, the
last line of `README.md` and `report.json`. Its verifier re-reads the same files from the same
Snapshot. `af task show` prints the report's title, the verifier's outcome and the Snapshot the
citations were checked against.

## Inputs

The Task file names the kind `report`. The built-in kind needs no kind package; an
`af.task-kind/1` package with `profile = "report"` maps another business kind to it. The
catalog names the captured policy with `report_policy`:

```toml
# .af/task-catalog.toml
report_policy = ".af/report-policy.toml"

# .af/report-policy.toml
schema = "af.report-task-policy/1"
max_document_bytes = 262144
required_sections = ["Findings", "Recommendation"]
require_citations = false
require_repository_citations = true
check_wall_ms = 60000
require_container = false
```

| Root input | Type | Where it comes from |
|---|---|---|
| `requirements` | `af/Requirements@1` | the Task file's goal and `requirements` |
| `source` | `af/SourceTree@1` | the committed Snapshot the Task is planned from |
| `sources` | `af/ReportSources@1` | `report_sources = "<path>"`, or the empty set |
| `comparison` | `af/MeasurementComparison@1`, optional | a bound recorded comparison |
| `measurements` | `af/Measurement@1`, many, optional | bound recorded Measurements |

`report_sources` names a project-relative JSON or TOML file in the `af.document-sources/1` shape
a Document Task reads: entries of `title`, `uri`, `revision` and `text`. A report accepts zero
to 256 entries, each text at most 256 KiB and at most 512 KiB in total, in a file of at most
640 KiB; a file over a bound is
refused when the Task is planned, before a Task exists. Without `report_sources` the Task
captures the empty set. Captured text is data; it never becomes execution authority.

`comparison` and `measurements` reach the author and the verifier as exact artifacts in their
context manifests when a Task binds them, and nothing when absent. Binding them from another
Task's outputs is package R4 of the research plan.

## Workers

| Worker | Declares | Gets |
|---|---|---|
| author, `roles = ["author"]` | `read-source`, `execute-checks` | an ephemeral-write clone of the Snapshot and a shell ([ADR-0118](../adr/0118-let-review-workers-execute-checks-in-an-ephemeral-clone.md)) |
| author | `read-source` only | a read-only materialization |
| verifier, independent of the author | `read-source` | a read-only materialization of the same Snapshot |

The author's declared source must seal byte-identical: build output and anything else it adds
is discarded, and a changed or removed file fails its Attempt with
`Execute-checks reviewer changed its declared source: <paths>`. A report Task allows no
`write-source`; a Worker that declares it is refused when the Task is planned. Any Worker that
returns a draft is the author for verifier independence, whatever it declares.

The author returns one `draft`, `af/DocumentDraft@1` or `af/DocumentDraft@2`. The second
version adds `repository_citations`, a list sorted by path and then line:

```json
{"schema": "af.document-draft/2", "title": "Where the time goes",
 "sections": [{"heading": "Findings", "body": "…"}, {"heading": "Recommendation", "body": "…"}],
 "citations": ["r1"],
 "repository_citations": [{"path": "Makefile", "line": 12}, {"path": "scripts/verify.sh"}]}
```

A path is spelled exactly as the Snapshot's Manifest spells it (`review_core::encode_path`),
relative to the repository root. The verifier declares `requirements`, `document`, `checks` and
`source`, and may declare `sources`, `comparison` and `measurements`; it returns one
`af/ReportEvaluation@1` naming the document, sources, requirements, check receipt and source
Snapshot it judged.

## Checks and acceptance

`report_check` runs the Document checks — `size`, `required_sections`, `citations` and
`source_locations` — and `repository_citations`: every cited path must be a regular or
executable file of the exact Manifest whose first 8 KiB hold no NUL byte, and a cited line must
be at most the file's line count. A failure names its reason in the receipt: `absent`,
`directory`, `symlink`, `binary` or `line_out_of_range`. With
`require_repository_citations = true` a report must cite at least one file. Failed checks
suppress the verifier; the report is then `changes_requested`.

Every artifact past the author is bound to the source Snapshot: the draft, the Document, the
check receipt, the evaluation and the acceptance. The compiler proves one lineage through
`same_as` ports, the Store proves one Snapshot ID for every value, a verifier whose checks judged
another Snapshot is refused before it runs, and an evaluation that names another Snapshot is
refused when it is admitted. A negative verdict stays negative; a missing verifier leaves the
report `incomplete`.

## Output

Give a person the human presentation; keep the other two for audit:

```sh
af task output starter-report --port report --format markdown --output report.md
af task output starter-report --port report --format canonical-markdown --output report-canonical.md --json
af task output starter-report --port verification --format json --output verification.json --json
```

| Format | What it writes | For |
|---|---|---|
| `markdown` | a readable presentation: the sections with lists and examples, sources, repository citations, the result in words and the reviewer's summary (`human-markdown` writes the same) | a person |
| `canonical-markdown` | the canonical Document text the checks and the verifier judged, escaping only what is Markdown markup where it stands (`af.document/2`) | audit, exact bytes |
| `json` | the recorded artifact envelope | machines and audit |

The canonical report renders as the Document profile renders it, followed by a `Repository
citations` section with one `path` or `path:line` code span per citation.

The human presentation is derived, never recorded or reviewed
([ADR-0151](../adr/0151-export-a-human-presentation-of-a-report-beside-its-canonical-markdown.md)).
It is written only when the recorded draft and sources still render to the canonical text byte
for byte. It opens with `Result: accepted`, `changes requested` or `incomplete` and the reason.
Its `Review` section is shown only from an acceptance receipt, check receipt and evaluation that
name this exact report, its Snapshot and the Task's requirements, recorded under a plan of the
Task's own revision for a verifier policy that revision names, with the Manifest of the Task's
source Snapshot and every input those receipts retain. When the Task captured sources, the
evaluation must retain them, because only the empty set may go unbound at the verifier. Other or
stale evidence is never attached; an inconsistent chain refuses the export. The closing note
says what judged the canonical report: the checks and the reviewer, only the checks when the
checks failed or no review was recorded, or nothing when no receipt judges the output.
Authored Markdown, HTML, links and headings stay text. Only a captured `https://` source the
canonical renderer would link becomes a link, and its destination writes each `&` as `&amp;`,
so a parser reads back exactly the captured location: `?q=&copy;` stays `&copy;`. Ordinary punctuation, paths and non-ASCII text
read as written. Only ASCII spaces and tabs are layout: a no-break, em or ideographic space is
text wherever it stands, including after the indentation of an indented example. Another output
type is refused, and an existing file is never overwritten.

`af task deliver` refuses a report Task before any Git mutation and names `af task output`.

This repository's `kernel/report` Pipeline, with `kernel/analyst` (Claude Opus 5.5) and
`kernel/report-verifier` (GPT-6 Sol), is staged under
[`fixtures/kernel-report/`](../../fixtures/kernel-report/README.md) with its install steps.
