# ADR-0254: Name the tree of a Worker's rules, and deliver the Diff Subject as a typed input

**Status:** accepted (2026-10-26). Amends
[ADR-0212](0212-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md)
clauses 4 and 5. Keeps
[ADR-0028](0028-prioritize-wise-token-use-and-minimum-worker-context.md),
[ADR-0117](0117-bind-task-inputs-to-recorded-task-outputs.md),
[ADR-0134](0134-bind-any-declared-root-port-to-recorded-task-outputs.md),
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md) and
[ADR-0207](0207-key-warm-check-layers-by-content-lock-them-split-the-build-deadline-and-seal-without-generated-files.md).
GitHub issue #331 items 2, 3 and 6.

## Context

ADR-0212 lists what a Worker reads first (`.af/rules.md` and a Task file's `context`) by digest
in an `af/TaskReadFirst@1` record. The rules came from the Authority Snapshot and the context from
the tree the Task reads. A continued Task (`af task run NEW --from TASK`) reads the candidate TASK's
review round reviewed, so when that candidate changed `.af/rules.md`, its Worker was told to obey
bytes its tree did not hold, under a heading that said the tree held them. Nothing in the record
said which tree it meant.

The Diff Subject (`af/diff-subject@1`) reached judging Workers only as `../diff-subject.json`
beside the sandbox. No contract declared it, so nothing checked before dispatch that a judge got
it, or got the right one. A Worker whose instructions relied on the file simply failed when it was
missing.

Judging Attempts materialize their tree and the base from the CAS each time. `review-sandbox`
gained `attach_base_template` so that judges of one change could clone one materialized base. The
Task side that used it was reverted during the GA polish, and #331 asks for it again, keeping
read-only isolation, the sealed-candidate guarantees and the #266 rule (materialized files get a
fresh mtime, so cargo never links a stale crate).

## Decision

1. **`af/TaskReadFirst@2` names its tree.** The record (`af.task-read-first/2`,
   `schemas/task-read-first-v2.json`) carries `snapshot_id`, the Snapshot of the tree the Task's
   Worker reads. Every entry, the rules included, is a path of that tree with its content digest
   there. The kernel reads the entries from the Snapshot the record names, read back from the
   Store, so a listed byte is one that tree holds. The rules are that tree's `.af/rules.md`, no
   longer the Authority Snapshot's. A Task with a `source` port names its source Snapshot. A Task
   without one (document, optimization analysis) names a Snapshot of the checkout it captured. The
   Worker's requirements text names the Snapshot in its "Read first" heading.
2. **A continued Task reads its candidate's rules and says so.** Its tree is the candidate it
   continues, so its record lists the candidate's rules and context. Its plan prints on stderr
   `read first: from Snapshot S, the reviewed candidate of Task TASK this Task continues`. When
   the candidate's `.af/rules.md` is not what TASK's Worker was given, the plan also prints a
   `rules:` line with both digests, or says that the candidate added or removed the file.
3. **`@1` fails closed.** `af/TaskReadFirst@1` is neither written nor read: its schema is gone. A
   revision whose provenance holds one is refused by name when a continuation reads it, saying to
   plan the work again from its Task file. The Store keeps only `@2` records across selection and
   refresh.
4. **The Diff Subject is a typed input.** A judging Worker declares it on the optional port
   `diff_subject`, of artifact type `af/DiffSubject@1`, cardinality one, `same_as source`. The
   artifact's payload is the unchanged `af/diff-subject@1` document. The port is the kernel's
   (enforced in `review-graph`): only a Worker that reads its `source` without writing it may
   declare it, under that one name and shape, and the name `diff_subject` takes no other type: a
   port of another type under it is refused when the plan is compiled, bound or not, because
   invocation admission reads every `diff_subject` input as the Diff Subject. No Pipeline binds it, takes it as a root input or
   returns it, and no operator outputs one. For each judging node that declares it, the executor
   builds the document from the node's source and the base it reads at `../base` (the declared
   base, else the recorded one). It records the document as the kernel operation `diff-subject@1`
   of that node, with the source Snapshot as subject, and adds it to the invocation. The same
   records give the same artifact, so replay is exact. The Store admits that port only on a node
   whose contract declares it, and only as that node's kernel artifact about its source's
   Snapshot. Before a context is bound, and again before the sandbox is materialized, the
   pipeline rebuilds the document and refuses the Attempt when the port is absent while a base
   exists, present without one, or different. The runner delivers the document as any typed input
   is delivered, after checking that it is on its own port and about that port's Snapshot. A
   change too large for the Worker's request bound or token reservation fails before dispatch, as
   any oversized input does.
5. **`../diff-subject.json` is no longer written.** Every shipped judging Worker that read it
   (`kernel/bugs` 1.6.0, `kernel/correctness` 1.4.0, `kernel/evaluator` 1.3.0, `kernel/ux` 1.2.0,
   `kernel/uix` 1.6.0) now declares the port and reads it there, so no shipped instruction needs
   the file. These packages are re-pinned in `.af/task-catalog.toml` with `af catalog digest`.
   `../base` stays for every judging Worker, declared or not. A judging Worker that does not
   declare the port gets no Diff Subject.
6. **Judging Attempts keep materializing; item 6 is not wired.** A safe clone of a warm template
   needs two steps besides the copy. Every cloned file must be stamped now (#266), because
   `clonefile(2)` keeps the template's mtimes. And the template must be checked unchanged before
   it is cloned (one `stat` per entry), because a template is a writable tree beside the sandboxes,
   and a clone of a changed base passes the seal's base check, which compares the clone with
   itself. With both steps, measured on this repository with
   `cargo run -p review-pipeline --example judge-materialization` (macOS, APFS, 14 cores), four
   judges of one change took longer cloned than materialized. On `crates/` (824 files, 19.7 MB),
   a debug build at load average 40 to 60 took 2.8 to 3.0 s materialized and 3.4 to 3.5 s
   cloned, and a release build at load average 240 to 275 took 6.4 to 8.7 s and 9.8 to 14.2 s.
   On the whole tree (2,661 files, 35.1 MB) the debug build took 20.5 s and 26.1 s, and the
   release build 20.4 to 20.8 s and 29.9 to 30.7 s. The first judge pays for building both
   templates. Later judges are within about 10% of materializing in the debug build, and slower
   in the release build. The kernel therefore keeps materializing judging Attempts.
   `review-sandbox` keeps what a future wiring needs. `attach_base_template` now stamps the cloned
   base with the current time, within a stated budget. `SandboxTemplate::intact` tells a template
   that changed since it was materialized from one that did not. The example stays as the
   measurement to repeat.

## Consequences

- A Worker is never told to read rules whose bytes differ from the tree they name. A Task whose
  rules or context changed between two plans is a changed plan, as before, and every plan now
  differs from 0.13.0's in its requirements text, which names the tree.
- Continuing a Task planned by 0.13.0 or earlier is refused with the command that plans it again.
- A judging Worker's Diff Subject is in its request, so it counts toward the Worker's context:
  about 150 bytes per changed path. Workers no longer find it missing or stale at run time.
  Project-owned judging packages that read `../diff-subject.json` must declare the port and read
  the input instead.
- Judging Attempts cost what they cost in 0.13.0. A faster clone, for example one that clones a
  whole directory where the filesystem can, needs its own measurement and decision.

## Options considered

- Keep the Authority Snapshot's rules and name that tree in the record: rejected. The Worker
  still reads a different tree, so the rules it is told to obey are not the ones beside its code.
- Keep `../diff-subject.json` beside the port: rejected. No shipped Worker reads it, and a second
  copy would be a second place to disagree.
- Wire the template clone without fresh stamps or without the integrity check: rejected. Without
  the stamps the #266 rule fails, and without the check a judge could be given a base nobody
  verified.
