# ADR-0125: Accept reports bound to an exact source Snapshot

Status: accepted, 2026-09-28.

Implements package R3 of [`docs/design/research-pipelines.md`](../design/research-pipelines.md)
under that plan's §2 ("a report is accepted by an independent verifier, not by its author, and
its acceptance is bound to the exact source Snapshot it cites"), on top of
[ADR-0058](0058-verify-document-artifacts-through-the-common-task-runtime.md) (Document Tasks),
[ADR-0118](0118-let-review-workers-execute-checks-in-an-ephemeral-clone.md) (a shell in a
clone that seals nothing back), [ADR-0124](0124-measure-and-compare-source-candidates-in-the-kernel.md)
(the Measurements and comparisons a report may read) and
[ADR-0002](0002-event-payload-changes-bump-the-type-version.md) (new payload shapes are new
types).

## Context

Research ends in a report as often as in a candidate. Nothing in the runtime accepts one. The
Document profile has no `source` port, so its author cannot read the repository, and its
acceptance names no tree, so "this file says that" is unverifiable after the fact. An implement
Task must end in a `SourceTree` a code policy verified. A model that writes a report about the
repository and a second model that approves it would, without a kernel contract between them,
leave nothing a later reader could check: which tree was read, whether the files it cites exist,
and whether the approval judged that report.

## Considered options

- **Widen the Document profile in place.** Rejected: a Document carries no code Snapshot by
  design (ADR-0058), its sources must hold at least one entry and 256 KiB of text, and its
  author is data-only. Giving it a `source` port, empty and larger sources and a shell would
  change what every existing Document Task captures, compiles and accepts, and what its receipts
  mean. The Document profile stays byte-identical.
- **An implement Pipeline with a report side output.** Rejected: acceptance would still be about
  the sealed candidate, a report Task would need a write-source Worker it does not want, and it
  would be deliverable to a worktree although it changed nothing. The report would be an
  unverified by-product of a code Task rather than the thing accepted.
- **A free-form `research` profile with model-judged acceptance.** Rejected: acceptance would be
  whatever a model said, with no kernel-checked shape, citation or Snapshot binding. §2 requires
  the kernel to check what it can — the citations resolve against the exact tree — and an
  independent verifier on another principal to judge the rest.
- **A report profile reusing the Document renderer and Task shape, with its own versioned
  sources, check and verification artifacts that all name the source Snapshot (chosen).**

## Decision

1. A new installed profile, `TaskKindProfile::Report`: the built-in kind string `report`, also
   reachable through an `af.task-kind/1` package. Root inputs are `requirements`, `source`,
   `sources` (`af/ReportSources@1`), and the optional `comparison`
   (`af/MeasurementComparison@1`, one) and `measurements` (`af/Measurement@1`, many). Required
   outputs are `report: af/Document@1` and `verification: af/ReportVerification@1`, which covers
   `verified`. The Task's allowed effects are `read-source` and `execute-checks`; a Worker that
   declares `write-source` is refused at planning. There is no `snapshot` output.
2. The profile's captured policy is `af.report-task-policy/1`, named by `report_policy` in
   `.af/task-catalog.toml` (a new declared `.af/report-policy.toml`): the Document policy's
   fields plus `require_repository_citations`. Its identity is the evidence policy of every
   report receipt.
3. `af/ReportSources@1` holds zero to 256 entries of `{ title, uri, revision, text }`, each text
   at most 256 KiB and at most 512 KiB in total — so a report's context fits the 1 MiB Worker
   request beside its other inputs — in a file of at most 640 KiB, captured from a Task file's
   `report_sources = "<path>"` in the `af.document-sources/1` file shape. A Task without
   `report_sources` captures the empty set, so every report names the exact sources it was
   checked against; a file over a bound is refused at capture, before a Task exists.
   `af/DocumentSources@1` is unchanged.
4. `af/DocumentDraft@2` is `af/DocumentDraft@1` plus `repository_citations`: a sorted set of at
   most 64 `{ path, line? }`. `path` is compared byte for byte with the Manifest's own
   `review_core::encode_path` spelling — no second alphabet, normalization or case folding — and
   `line` is 1-based. The report profile accepts drafts of either version; the Document profile
   does not accept the second.
5. Three installed operators, closed members of `TaskOperatorV1`, mirror the Document ones.
   `report_seal` renders the draft with the Document renderer's exact bytes, appends one line per
   repository citation as a Markdown code span `path` or `path:line`, and records the Document
   with the source Snapshot as its subject and the Snapshot's Manifest among its inputs.
   `report_check` holds one protected, zero-token Attempt: it runs the Document checks and
   resolves every citation against that exact Manifest — a regular or executable file whose
   first 8 KiB hold no NUL byte and, when a line is cited, has that many lines — and records
   `af/ReportCheckReceipt@1` with the document, sources, policy, source Snapshot and Manifest
   identities and each failed citation's reason (`absent`, `directory`, `symlink`, `binary`,
   `line_out_of_range`). `report_accept` records `af/ReportVerification@1` with the acceptance
   invocation, exact Document, policy, check receipt, selected `af/ReportEvaluation@1` and the
   source Snapshot.
6. Every report port past the author is `same_as` the `source` input, so the compiler proves one
   lineage and the Store proves one Snapshot ID for the draft, the Document, the check receipt,
   the evaluation and the acceptance. Admission of a verifier re-reads its check receipt and
   refuses it unless the receipt passed and judged the Snapshot, document and sources the
   verifier is about to read; an evaluation naming another Snapshot is refused at output
   admission; acceptance recomputes both.
7. `worker_access` grants `ExecuteChecks` — ADR-0118's ephemeral-write clone and shell — to a
   Worker whose `roles` contain `author` and whose effects hold `read-source` and
   `execute-checks` without `write-source`. Its declared source must seal byte-identical;
   anything it adds is discarded; a changed or removed entry fails the Attempt with ADR-0118's
   message. Any other non-writing, non-review Worker keeps its read-only source.
8. A report's author is whoever returns a draft of either version, whatever it declares, so no
   Pipeline can make the author its own verifier (ADR-0058's rule for Document drafts).
9. `af task deliver` refuses a report Task before any delivery lease, prepared record or Git
   mutation, naming `af task output <id> --port report --format markdown`, whatever its acceptance.
   `af task output --port report --format markdown` writes the Document exactly as it does for a
   Document Task; `af task show` prints the report's title, the verifier's outcome and the cited
   Snapshot.
10. `af catalog init --profile report` emits the credential-free `builtin/report` starter: a
    command author that reads the committed tree and cites two paths and one `path:line`, and an
    independent command verifier that reads the same Snapshot. This repository's `kernel/report`
    Pipeline with `kernel/analyst` and `kernel/report-verifier` is staged under
    `fixtures/kernel-report/`, because a Task Worker may not edit `.af/`.

## After the first verification

Verification of the package (Task `research-r3`'s review) changed four rules, and the text above
reads as amended:

- **The `sources` root port is optional**, in the Pipeline, the starter and both Workers, and so
  are the seal's and the check's `sources` inputs. A Pipeline that binds nothing there seals the
  report against the empty set, which the seal records as an artifact it names in the document,
  and the check requires that named set to be empty. The Task-file adapter still captures an
  empty set when `report_sources` is absent, so a report planned from a Task file always names
  its exact sources.
- **An execute-checks Worker may add beside the source, never inside it.** An addition under a
  top-level name the source Manifest holds fails the Attempt as an edit of the declared source;
  build output, a harness and `HOME` dotfiles under new top-level names are scratch the clone
  takes with it. This tightens ADR-0118's grant for reviewers too.
- **Sources are bounded so that every admissible report renders.** 512 KiB of text in a file of
  at most 640 KiB, checked on the file's bytes before it is parsed; the design's 4 MiB could not
  reach a Worker through the 1 MiB request.

## Consequences

- A report's acceptance says which tree it is about. Every receipt names the source Snapshot,
  and none of a check, an evaluation and an acceptance of different Snapshots can be combined.
- The kernel checks the shape of a citation — the file exists, is text, has the line — and the
  verifier judges whether the line says what the report claims. Neither check is a claim that
  the report is right.
- `comparison` and `measurements` reach the author and the verifier as exact artifacts in their
  context manifests when a Task binds them, and nothing when absent; binding them from another
  Task is package R4. A Worker port carries one Snapshot ID, so a bound `measurements` whose
  artifacts measured different Snapshots (a baseline and a candidate) cannot be rendered into
  one context as it stands; R4 decides how such a binding is admitted.
- A Worker's rendered context is bounded at 1 MiB; the 512 KiB sources bound keeps every
  admissible report inside it. Sources beyond that need bounded retrieval of the exact captured
  artifact by its ID rather than the whole payload inline — a later decision, recorded for R6.
- `schemas/report-sources-v1.json`, `document-draft-v2.json`, `report-check-receipt-v1.json`,
  `report-evaluation-v1.json`, `report-verification-v1.json` and `report-task-policy-v1.json`
  are pinned against the Rust types in both directions; the three operators are closed members
  of the Pipeline operator schema.
- Document, implement and every other Task are unchanged: their revisions, plans and results
  serialize exactly as before, and the existing fixtures are byte-identical.
