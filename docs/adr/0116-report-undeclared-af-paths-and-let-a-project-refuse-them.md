# ADR-0116: Report undeclared `.af/` paths from the manifest, and let a project refuse delivery

Status: accepted, 2026-09-22.

## Context

[ADR-0115](0115-declare-the-af-layout-and-keep-task-files-out-of-git.md) made the `.af/` layout
data: one table in `review_config::layout`, and a `classify` that turns any repository-relative
path into `Outside`, `Root`, `Declared` or `Undeclared`. It warns about one thing only — a Task
file sitting in the checkout — and it learns that by asking git about a path on disk.

That leaves the case the table was written for unanswered. The pull request that started this
work carried 244,008 added lines because `.af/tasks/<workstream>/` held candidate patches,
reviewer results and captured Task inputs; every one of them was already committed, so no
question about a Task file's location would have mentioned them. They were in the Snapshot, they
were in every later candidate, and they were delivered into a worktree where `git add -A` swept
them into the branch. Nothing in the kernel ever said their names out loud.

A project also needs to be able to say *stop*. Advice that is only advice is right for a first
release and wrong for a repository that has decided its authority directory holds declarations
and nothing else.

## Options

- **Strip undeclared paths from the captured Snapshot.** Rejected, again and for the same reason
  ADR-0115 rejected it: a Snapshot's identity is the kernel's foundation, and a reviewed object
  that differs from the repository is worse than a large diff.
- **Exclude them from the delivered worktree.** Rejected. Delivery writes the verified derived
  Snapshot and nothing else; a worktree that silently lacks paths the Snapshot has would break
  the ADR-0031 guarantee that the delivered tree is exactly the verified result, and would hide
  the problem at the one moment the operator is looking.
- **Walk the working tree, or the sandbox, to find them.** Rejected. Two machines would then
  disagree about the same Snapshot, and the answer would depend on what happened to be on disk
  after the capture. Classification reads the manifest, which is the only thing that is fixed.
- **Add `git check-ignore` to the classification, as the Task-file warning does.** Rejected: the
  paths under discussion are tracked. Being ignored is irrelevant to a path that is already in
  the Snapshot, and a subprocess would make a pure function machine-dependent.
- **Refuse by default.** Rejected: every consumer with a `.af/tasks/` habit would stop working on
  upgrade, for something the report already makes visible. `warn` is the default and `refuse` is
  the project's explicit choice.
- **Read the policy from `.af/af.toml` at delivery time.** Rejected. Delivery would then be
  governed by a file that can change between the plan a developer approved and the delivery they
  confirmed. The policy is captured with the rest of the project policy.
- **Classify the manifest, report it everywhere it matters, and give the project one switch.**
  Chosen.

## Decision

`review_config::layout::classify_manifest` groups every path of a captured Snapshot manifest into
`declared` and `undeclared`, each a `PathGroup` of manifest path spellings and a byte total. It
is a pure function of the manifest and `LAYOUT`: it reads recorded entries, never a working tree,
a sandbox or a host path, so the same Snapshot answers the same on every machine. Paths are
judged by their *decoded* bytes, because a manifest path is a lossless rendering of raw bytes —
`.af/tasks/50%25-off.patch` is one path named `50%-off.patch` — while the group keeps the
manifest spelling, which is the spelling `ignored_paths` already uses. A path that is not UTF-8
declares nothing, because every declared segment is ASCII. The byte total saturates: a
classification is total over any manifest, including a hostile one.

`af task plan` and `af task start` classify the Snapshot they just captured and print one
advisory line on stderr — the count, the byte total and up to ten paths — and the `--json`
document carries the whole group in a typed `undeclared_af_paths` field. The field appears only
when there is something to report, so a document that had no such field still has none, and the
advisory changes no exit code and no other byte of stdout. It is the Task-file adapter that
classifies, so every command capturing a source Snapshot through it reports the same thing about
the same Snapshot — `af review plan --file` and `af review run --file`, `af self optimize`, and
the fixed v1 `af task start --goal` — which is the point of having one table.

`af task deliver` records the same group in `af/task-delivery@1` beside `ignored_paths` and
prints it in the delivery summary. That is where the field belongs: `ignored_paths` is the
receipt's existing statement about what the delivered worktree carries that git will overlook,
and this is its statement about what the delivered worktree carries that the layout does not
name. Both are advisory, both name the same path spellings, and neither removes anything. Old
receipts have no such field and deserialize as an empty group; a delivery whose authority tree is
fully declared writes no field, so existing receipts are byte-identical.

`[delivery] undeclared_af_paths = "warn" | "refuse"` in `.af/af.toml` is the project's switch,
defaulting to `warn`. Under `refuse`, delivery fails as soon as it has read the Snapshot's
manifest — before the prepared delivery record, before `validate_branch`, and before any Git
mutation — naming every undeclared path, and leaves no branch, no worktree and no record.

The policy is part of the captured project policy identity. `af task plan` reads `[delivery]`
out of the Authority Snapshot's `.af/af.toml` and stores it in the run authority whose content
address is the Task's `authority.policy_id`; `af task deliver` reads it back from that recorded
artifact and never from disk. Two projects that differ only in this value compile to different
policy identities, so a later edit cannot change what an admitted plan agreed to. The default is
deliberately not written down — a project that never heard of the knob keeps the exact policy
identity it had — and the fixed v1 `af task start --goal` path passes the value from the project
file its authority was already admitted from, because its compatibility authority is synthetic
and carries no `.af/af.toml`.

Reading `[delivery]` on its own ignores unknown *tables*, because a Task captures it before
anything has admitted the whole project file and must not start requiring more of that file than
it did. An unknown key inside `[delivery]`, or a value that is neither `warn` nor `refuse`, is an
error in both readers.

## Consequences

The kernel can now name the bytes that made a 244,008-line pull request, from the Snapshot alone,
at the two moments an operator can act: when the plan is made and when the result is delivered.
A consumer that has not thought about this sees two extra lines of stderr and one extra field;
one that has, sets `refuse` and finds out before a branch exists rather than after a review.

Snapshot identity, the delivered tree and `ignored_paths` are unchanged by this decision, and no
path is removed from a Snapshot or a worktree. The report is evidence, not enforcement; the only
enforcement is a refusal to deliver at all.

The report is a pure function of the source Snapshot's Manifest and the layout table, so every
presentation of one Task carries the same field: `af task plan` and `start` classify the manifest
they just captured, and `af task run`, `show` and `explain` derive the identical group from the
recorded source Snapshot in the Store. A `start --execute` document and the `run` document that
follows it are therefore byte-equal, as the Task-starter fixtures require. The report says nothing
about the derived Snapshot: a Worker that writes under `.af/` is a different question, for whoever
reviews its candidate.
