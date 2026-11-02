# Architecture decisions

The binding decisions behind the Review Kernel and the `af` command travel with the code.
Consuming repositories record their own decisions in their own logs.
The ADR files in this directory are the authoritative catalog. This guide has no per-decision
index, and no generated catalog is committed.

## Discover decisions

[Browse the decision files on GitHub](https://github.com/PakhomovAlexander/afactory/tree/main/docs/adr).
From the repository root, list titles, recorded statuses and relative file links deterministically:

```sh
python3 -I scripts/adr.py list
```

The command prints Markdown bullets to stdout in basename order; links are relative to the ADR
directory. It reads the files, including legacy records and their historical status wording,
without allocating an identity or writing anything. For another checkout or a fixture directory:

```sh
python3 -I scripts/adr.py list --directory /path/to/checkout/docs/adr
```

A missing status is shown as `unspecified`; discovery does not infer acceptance from a filename.

## Propose a decision

Add only the decision's own file under `docs/adr/`, named
`YYYY-MM-DD-descriptive-topic.md` using the proposal date and a short, specific kebab-case topic.
Use the whole basename without `.md` as its identity, including in the heading
(`# ADR-YYYY-MM-DD-descriptive-topic: Title`). Distinct topics on the same date have distinct
identities. Check basename uniqueness in the directory; for a collision choose a more specific
accurate topic. There is no next-number allocation, reservation, manifest or shared append point.
Do not edit this guide, a root index, scripts or test lists for each new decision.
These authoring rules supersede older instructions about assigned numbers and README entries,
including that wording in shipped Task prompts; those packages need no edit to add a decision.

Open with a status and date, state the context, list the considered options and why each was
rejected, record the decision, and end with consequences. Open a pull request; a proposal becomes
binding when merged with accepted status. Mark a rejected proposal `rejected` when retaining it
for its rationale. Reference the decision from the affected design, pull request and change note
as appropriate; these references are not a central catalog.

## Identity and lifecycle

An accepted decision's identity and body are immutable. A changed decision is a new ADR naming
what it supersedes. A partially superseded ADR keeps its body and gains a status-line note linking
the new ADR; that status line may be restated when the new ADR spends its transition wording.
A fully superseded ADR is deleted and git history keeps it, unless the superseding ADR retains
it with a superseded status line. There is no catalog entry to remove.
Links to a deleted ADR, or to a deleted internal record or completed design note, are rewritten
to the superseding ADR or plain text; this remains the only allowed edit to another accepted
ADR's body ([ADR-0113](0113-ga-reads-only-what-ga-writes.md), clauses 6 and 8;
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md), clause 8).

Legacy `NNNN-topic.md` identities stay unchanged and their numbers are never reissued.
Historical anomalies, including both accepted 0107 records and both accepted 0240 records,
retain their exact basenames and links; discovery does not enforce sequence uniqueness.
Accepted Scenarios ADR-0274 and accepted ADRs 0270–0273 keep their identities. Existing proposals
also keep their identities unless an explicit change authorizes a rename before acceptance.
The date-topic identity stays stable after acceptance; a later amendment does not change its date.
