# ADR-0094: Bind Task Review assignments and readable inputs

Status: accepted for the authorized Task correction, 2026-09-14. Superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): Review catalog and policy generation one as
compatibility formats.

The generic Task Review adapter omitted required prior-Finding dispositions and used logical
reviewer labels as canonical artifact producers. Its Subject context also copied a base64 patch
into each initial Worker request, making the existing 1 MiB request bound reject otherwise
admissible Diffs. These corrections apply to generic Task Review; historical Campaign Review
keeps its established contracts and artifact identities.

## Versioned authority

New `af.task-catalog/2` captures `af.review-task-policy/2`. Its installed Review Bind emits
`af/TaskReviewSubject@2` and one `af/TaskReviewAssignment@1` port for each configured logical
reviewer. Each reviewer binds its own assignment and returns `review.kernel/ReviewerResult@2`.
The compiler/domain admission checks the exact assignment wiring before execution.

Assignments reuse canonical Finding projections and prior-row exclusions: rejected, wontfix,
and authority-diagnostic rows are excluded; fixed and out-of-scope claims retain their explicit
status and scope. The original logical source receives its own claims. Other reviewers' rows
and private transcripts are absent. Each assignment retains Subject, Round and prior-history
identities and the existing 64 KiB prior-Finding bound. It is never silently truncated.

Every assigned Finding requires exactly one corroborate, not_reproduced or dispute disposition.
Missing, duplicate and unassigned dispositions fail admission and are checked again at reduction.
A missing required reviewer still produces only an incomplete gather, with no partial Ledger.
Not reproduced does not resolve or erase the Finding. Canonical Report, Demand and disposition
producers retain the selected Worker's actual flattened node and Attempt identities. Logical
source names continue to determine Finding and Demand semantics.

Task execution is assessed before Review acceptance. A failed independent node retains any
completed passing Review receipt, but the Task is exhausted and its Review conclusion is
incomplete/inconclusive. Passing evidence cannot hide unfinished execution. This does not
replace the separate Implementation rule that preserves a genuine failed verification receipt.

Catalog and policy generation one remain explicit compatibility formats. Already persisted
Subject, result, context and reduction identities replay unchanged. Generation-one new
reservations refuse eligible nonempty prior Findings before paid dispatch; generation one does
not claim complete prior-Finding disposition coverage. Current packages must upgrade their
ports and result schemas together with the catalog generation, rather than reinterpret old pins.

## Readable bounded input

Subject generation two carries scope metadata and a declared patch file with an exact relative
path, content ID and byte length. The original ChangeSet remains immutable and bounded at 4 MiB
of serialized authority. The 1 MiB initial Worker request bound remains unchanged. File bytes
available through permitted tools are distinct from bytes rendered in that initial request.

Before dispatch the source environment freshly verifies the ChangeSet and patch bytes and
materializes the declared file in a disposable read-only baseline. Existing source entries
under `.af-review-inputs` cause a collision refusal. The original source Snapshot and its Tree
Digest exclude these host inputs. Read-only seal detects changes to either source or input
files; source-writing/candidate-producing Workers cannot receive this transport. No host input
can become candidate source or a delivered output. Native tool retrieval remains under the
existing supervisor, raw-evidence and response bounds; the file is not automatically injected
as an unbounded prompt.

Focused tests cover a two-stage common Task, exact source-scoped assignments, omitted/duplicate/
unassigned dispositions, nested producer identity, a readable Diff above the old base64 failure
threshold, and source/input-file preservation. Historical generation-one tests remain in place.
