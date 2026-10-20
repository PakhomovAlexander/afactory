# ADR-0159: Carry a bounded Project Brief on a declared port

**Status:** accepted (2026-10-20); clauses 1, 3, 4, 5, 7 and 9 amended (2026-10-21) by the PJ3
close-out, before release: the Brief port is `af.pipeline/2`, the Survey is the resolved
source's Project's, author emails are excluded while repository text stays verbatim, the
plan-time cap uses the serialized request, and the preview registers nothing. Package PJ3 of the
[Project design note](../design/project.md). Builds on
[ADR-0157](0157-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)
(the Project and its Project Store),
[ADR-0158](0158-measure-a-project-into-surveys-with-a-token-free-surveyor.md) (Surveys),
[ADR-0117](0117-bind-task-inputs-to-recorded-task-outputs.md) and
[ADR-0134](0134-bind-any-declared-root-port-to-recorded-task-outputs.md) (root inputs resolved
at plan time, pinned, bound only from recorded result outputs),
[ADR-0028](0028-prioritize-wise-token-use-and-minimum-worker-context.md) (minimum Worker
context), [ADR-0132](0132-measure-and-compare-source-candidates-in-the-kernel.md) (the kernel
measures), [ADR-0135](0135-collect-finished-tasks-behind-a-tombstone-and-a-reachability-sweep.md)
(collection by reachability) and
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md).

## Context

A Survey (ADR-0158) says how big a Project is, what it decided, which terms it defines and
which checks it declares. A Worker still learns none of it unless it spends tool calls
rediscovering it in every Attempt; the kernel's implementer is told to read `CONTEXT.md` and the
ADRs whole. Giving every Worker the Survey would break minimum context (ADR-0028): a Survey is
tens of kilobytes, it names contributors, and most Workers need none of it. What is wanted is a
small, declared, bounded selection that only the Workers a pipeline names receive, fixed when
the Task is planned so a later Survey can never change what an admitted plan agreed to.

## Considered options

- **Hand every Worker the Survey, or a summary of it, as ambient context.** Rejected: context
  that no port declares is exactly what ADR-0028 forbids, and the manifest could not say why a
  Worker received it.
- **Read the newest Survey when a Worker is dispatched.** Rejected: a refresh between plan and
  run would change a Worker's input under an admitted plan, and replay could not reproduce it
  (ADR-0117 resolves every root input once, at plan time).
- **Let a model write the Brief.** Rejected for the reason ADR-0158 rejected a model-written
  Survey: the kernel measures. A model-written project summary is a later, separate decision.
- **Truncate an oversized Brief mid-section, or leave the port silently empty.** Rejected: a cut
  section misreports what it holds, and an empty port hides that the variant cannot fit. Whole
  sections are dropped with a reason, and a Brief that cannot fit at all fails the plan.
- **Record staleness in the Brief.** Rejected: one Brief bound into two Tasks is behind two
  different source commits by two different counts. Staleness is the consuming Task's receipt.
- **Add `brief` to `af.pipeline/1` in place, as `remote_checks` was added
  ([ADR-0140](0140-run-a-declared-check-through-a-gate-pull-request.md)).** Rejected: the port
  declaration is a persisted shape, and ADR-0145 clause 2 gives every changed shape a new version
  with the superseded one refused; an optional key is no exception, and ADR-0140's earlier
  addition does not make one.
- **Move every pipeline to `af.pipeline/2`.** Rejected: a pipeline without the port has the
  shape `af.pipeline/1` always had, so relabelling it would change every package digest and lock,
  the repository's own kernel packages among them, for no change of shape, and would give one
  pipeline two spellings.
- **Select the Survey by the invoking checkout.** Rejected: a bound `source` keeps the
  repository identity of the Snapshot it names, which need not be the invoking checkout's
  (ADR-0117), and a Task over repository B would receive repository A's vocabulary, decisions and
  checks. Reporting the staleness as unknown does not make that context related.
- **Admit the Brief at plan time by summing its payload with the instructions.** Rejected: the
  runner caps the serialized request, wrappers, protocol fields, output schemas and JSON escaping
  included, so a sum that fits can still be a request dispatch refuses every time.
- **Bump `af/TaskRevision@1` and `af/ExecutionPlan@1` to carry the pin and the receipts.**
  Rejected: both reach every part of the runtime. The revision's provenance already carries the
  one record no port carries (`af/TaskInputBindings@1`), and the plan pins the revision, so new
  typed records in that list need no change of either shape.

## Decision

1. **The record.** `af/ProjectBrief@1` (schema `project-brief-v1.json`) is canonical JSON with
   decimal-string numbers: `schema`, `survey_id`, `project_id`, `variant`, `head` (`commit`, and
   `tree_digest` when the Survey has one), `sections`, `dropped` and `rendered_bytes`. Every
   section is the pinned Survey's verbatim, except `decisions`, whose records are the variant's
   newest N (date descending, undated last, then number descending, then path), and
   `hot_paths`, the first N of `history.hot_paths`. `rendered_bytes` is the byte length of the
   record's own canonical JSON, that field included. A Brief holds no host path, Provider ID or
   author-identity email, and keeps repository text verbatim, under ADR-0158 clause 6: its
   sections are the pinned Survey's, in which no author email appears (an author name that holds
   one is the sanitized identity `h:` and the hash of that name, under both identity policies),
   while decision titles, paths, terms, check names and hot paths are recorded as committed, `@`
   included, so a decision title that holds an address enters a Brief unchanged.
   `contributors` is rendered only when a variant lists it, and `activity` never: a variant that
   lists it records it in `dropped` with the reason `not_in_survey`.

2. **Variants are project policy.** `[brief.<name>]` tables of `.af/af.toml`, in the project
   layer beside `[survey]` and deep-merged like it, each admit exactly `sections` (an ordered,
   duplicate-free list of `identity`, `tree`, `history`, `contributors`, `decisions`,
   `vocabulary`, `checks`, `hot_paths` and `activity`), `max_bytes` (default 8192), `decisions`
   (12) and `hot_paths` (20), both counts 0 to the Survey's `max_entries` (1,000). Built in:
   `orient` = `tree`, `vocabulary`, `decisions`, `checks`, `hot_paths` in 8,192 bytes, which a
   project may override key by key; and `none`, which constructs nothing. An unknown field, an
   unknown or repeated section, a count out of range, a `max_bytes` below 1,024 or above
   262,144, a `[brief.none]` table, a project's own variant without `sections`, and
   `contributors` or `activity` in `orient` are refused when the policy loads, naming the variant
   and the field: by `af project brief` and by `af task plan` alike.

3. **The port.** A pipeline asks for a Brief with a root port `artifact_type =
   "af/ProjectBrief@1"`, `cardinality = "one"`, `optional = true`, `affinity` `unbound`,
   `root_default = "project_brief"` and the new key `brief = "<variant>"`, allowed only with that
   default; `RootDefaultV1` gains `ProjectBrief`. Under ADR-0145 clause 2 that declaration is a
   new version of the pipeline shape: `af.pipeline/2` (schema `pipeline-definition-v2.json`, the
   definitions `pipelineV2`, `contractV2`, `inputPortV2` and `briefPort` of
   `task-contracts-v1.json`, parity fixture `fixtures/task-contracts/v2/`) is a pipeline whose
   contract declares at least one `project_brief` port, and the only version that may declare
   one. The superseded shape, an `af.pipeline/1` pipeline carrying `brief` or `project_brief`,
   is refused by its schema and when the pipeline loads, naming the version it needs, and so is
   an `af.pipeline/2` pipeline that declares no Brief port: each pipeline has one label.
   `af.pipeline/1` is the shape earlier releases wrote and read, with `empty_review_history` its
   only root default, so a pipeline without the port keeps its label, its bytes and its digest.
   Writers, readers, validation, the schemas and the parity tests move together. The containers
   that carry a pipeline's contract without its label stay at version 1 by never carrying the
   port: `af.planning-request/1` does not offer an `af.pipeline/2` pipeline to the Planner for
   composition; `af.catalog-contract-fixtures/1` neither exports nor admits one, so a shared
   catalog cannot hold one yet; and an `af.worker/1` manifest, like a compiled operator
   signature, that declares `project_brief` or `brief` on a port is refused, since a Worker
   admits the Brief as a plain optional input. A package and a proposal carry a pipeline's own
   bytes, label included, an execution plan names it by digest, and a compiled graph clears root
   defaults from its boundary, so no other persisted shape changes. A Worker admits the port in
   `[signature.contract.inputs.<name>]` and its `input.schema.json`, and the pipeline wires it
   per node with `kind = "input"`. A node that does not wire it sees nothing; the Worker
   that does sees one more context manifest entry: the port's name, `required_by` `port:<name>`,
   the Brief's artifact ID and type, its bytes and the runner's token estimate. No kernel
   package under `.af/task-packages/kernel/` wires the port in this package (clause 10).

4. **Plan-time pinning and construction.** A Task's initial capture, before any pipeline is
   selected and after the Task file's bindings are resolved, selects the Project of the resolved
   source Snapshot's origin: the `repository_id` of the tree captured from the checkout, or of
   the Snapshot a bound `source` names (ADR-0117 keeps that identity, which need not be the
   invoking checkout's), and the Project ID derived from it (ADR-0157). It pins the newest
   complete, uncollected Survey of that Project whose own `repository_id` is that one, never one
   of the invoking checkout's Project when the two differ; when that Project has no complete
   Survey it pins nothing and the automatic port stays empty (`no_survey`). The pin is an
   `af/ProjectSurveyPin@1` in the Task Store's CAS: its
   identity and sections verbatim, its Survey ID as provenance, and no `previous_survey_id` or
   other link, so ADR-0135's collector reaches nothing beyond it. It records an
   `af/ProjectBriefCapture@1` in the revision's provenance (kept there like the bindings record):
   the pin, the project's own variants (the planning checkout's policy, like the pipelines that
   name them) and the ancestry observation of clause 5. It does so only
   when the captured catalog can carry a Brief (a captured pipeline declares a `project_brief`
   port, or a Planner may generate one) or the Task file binds one, and records nothing at all
   when the capture would be empty, so every other Task keeps its revision identity. Full Survey
   records stay in the Project Store; a Project Store that cannot be read is reported on stderr
   and pins nothing, since a Survey never fails a Task. The `af` crate supplies the constructor
   to `review-config` as a `ProjectBriefResolver`, and every `normalize_root_inputs` site (the
   Task-file adapter, proposal validation, a generated pipeline's handoff) calls it for an
   unbound `project_brief` port with the revision being normalized: a pure function of that
   revision's capture and the CAS, never of a Survey, a policy file or git. It renders the
   port's variant, drops whole sections, last-declared first, until the exact canonical bytes,
   `dropped` and `rendered_bytes` included, are at or under `max_bytes`, recording each drop with
   the reason `max_bytes`; a Brief that still does not fit once every section is gone is a plan
   error naming the variant and both byte counts. It publishes the Brief in the Task Store's CAS,
   referencing its pin, and binds its artifact ID; the compiled plan pins it, and run, resume,
   retry and replay never resolve again. Without a pin the port stays empty and the plan goes
   on; variant `none` leaves it empty with or without one. A variant no policy or built-in
   defines is a plan error naming the variant and the nodes that wire the port.

5. **Staleness is the consuming Task's receipt.** The source commit is the resolved source's
   committed revision: the captured commit, a bound root capture's own, and none for a re-rooted
   bound tree. The capture observes, for the pinned head and the head of every Brief the Task
   file binds, whether it is an ancestor of the source commit (`git merge-base --is-ancestor`)
   and `git rev-list --count <head>..<source>`, through the Project's supervised git plumbing on
   the planning checkout when it is a checkout of the source's repository, or records a typed
   `unknown` reason: `not_ancestor`, `no_checkout`, `missing_objects`, `deadline` or
   `no_committed_revision`. After each normalization the revision records one
   `af/ProjectBriefReceipt@1` per Brief port of the selected root: the Brief's ID, the source
   commit and the observed staleness, or why the port carries none (`variant_none`,
   `no_survey`); the compiled plan pins that revision. Later compilation, a generated
   pipeline's handoff (whose new revision's receipts the Store admits beside its new inputs) and
   `af task explain` read the observation and never live git, so the receipt survives a deleted
   checkout. It is computed alike for a constructed and a bound Brief, never rewrites a Brief,
   and is reported, never a refusal.

6. **Binding.** The port binds like every other root input under ADR-0134: from a recorded Task
   result output of type `af/ProjectBrief@1` (a pipeline that exposes its constructed port as a
   public output records one, forwarded unchanged), never in the exact-artifact form, which is
   refused with ADR-0134's message; an explicit binding suppresses construction and no Survey is
   read for it. Because ADR-0134 grants provenance, not verification, a bound Brief is admitted
   only when the kernel's constructor produced it from one valid pin and its sections are that
   pin's rendering; anything else is refused with the typed reason `not_kernel_constructed`. A
   Worker output of that type is refused at output admission with the same reason (the generic
   Worker host before it publishes the reply, and `CodeTaskDomain` when it admits the output),
   and the Attempt's typed feedback is `output_admission_rejected`.

7. **The request cap.** At plan time, a Worker node wired to a Brief whose known request is
   over the Worker request renderer's cap of 1 MiB refuses the plan, naming the node and the
   bytes, before any dispatch. The known request is measured by the renderer's own accounting:
   the `af.worker-request/1` it would render for the node's first Attempt, serialized by the same
   code dispatch uses, so its protocol fields (`schema`, `reply_format`), the Worker's
   instructions, every root artifact wired to the node (the Brief among them) in its value
   wrapper, the output schemas it carries and all JSON escaping count. What is unavailable at
   plan time counts as absent, which can only make the request smaller: an input another node
   produces leaves its port out, retry feedback is empty, and an output the kernel may produce
   itself (a source implementer's `candidate`) carries no schema. A known request over the cap
   is therefore one dispatch refuses every time; the runner's check at dispatch still applies to
   every request, and a node whose request the generic renderer does not render (a derived
   Worker, a domain's own context) is checked there only.

8. **Preview and explain.** `af task plan` and `af task explain` print one line per Brief port
   under `IN`: `<port>: af/ProjectBrief@1 <id> (<variant>, <rendered_bytes> B, ~<tokens> tokens,
   survey <head>, behind <N|unknown>)`, with `bound` and a comma before the variant for a bound Brief, or
   `<port>: none (no Survey recorded; run af project refresh)`, or `<port>: none (variant none)`.
   The tokens are the runner's estimate for the manifest entry. The lines are text; no `--json`
   document changes.

9. **`af project brief [--variant NAME] [--survey ID] [--json]`** renders a variant (default
   `orient`) of the newest complete Survey, or of the complete one named, and prints exactly the
   canonical bytes a plan would bind, recording and registering nothing: it opens the Project
   Store to read only, so the Store registry (`af/stores.toml`) and the Store's last use are as
   they were. `--json` prints `af/project-brief-preview@1` (schema
   `project-brief-preview-v1.json`): the Brief's artifact
   ID, the Brief byte for byte as the text form, and the manifest entry; or `outcome: none` with
   `variant_none` or `no_survey`, exit 0. The text form exits 1 without a Survey, with the
   preview's sentence.

10. **The kernel's own implementer is not wired yet.** Whether `kernel/implementer` reads
    `orient` is decided on measured Attempt tokens with and without it, in its own pull request
    (design note §7): the Brief is opt-in per pipeline node, and the kernel's packages adopt it
    only once the numbers say it saves more than it costs.

11. **What `orient` renders here.** Measured with this package's `af` on this repository's tree:
    the Task's source Snapshot `sha256:d98dd7e5…` with this package applied, committed as the
    single commit `0ee227f921002ebf776390c7536c59dab8d5a85d` of a scratch repository, because
    the Task's sandbox carries no git history. The complete Survey took 2.4 s. Rendered whole,
    `orient` is 8,452 bytes, over its 8,192; so it drops `hot_paths`, its last-declared section,
    and binds 7,194 bytes (`rendered_bytes`: `tree` 1,286, `vocabulary` 1,914, `decisions`
    2,778, `checks` 706, the rest metadata and `dropped`), which the runner estimates at 1,833
    tokens for its manifest entry (7,330 bytes as a Worker value). With one commit every path
    is a hot path of one commit, so the dropped `hot_paths` were the first 20 paths by name
    (1,289 bytes); on the full history they are the 20 most-touched, of similar length. For
    comparison, `CONTEXT.md` alone is 35,616 bytes, about 8,904 tokens by the same estimate, and the
    kernel's implementer is told to read it whole today.

## Consequences

- A pipeline author decides which Workers see the Project, per node, in reviewed and locked
  bytes; the Worker's manifest names the Brief and its bytes.
- A Brief is a pure function of one pin and one variant: `af project brief` shows the exact bytes
  and artifact ID before any token is spent, and replay renders the same artifact.
- A Task whose catalog can carry a Brief writes a pin and a capture into its Store; collecting
  the Task releases them, while a later Task keeps its own.
- A pipeline that declares the port is `af.pipeline/2`, which an earlier release refuses by its
  label; every other pipeline, the repository's own kernel packages among them, keeps
  `af.pipeline/1` and its bytes. A Brief pipeline is not offered to the Planner for composition
  and cannot be shared through an exported catalog until those containers take a version that
  carries it.
- A Task over a source bound from another repository orients its Workers on that repository's
  Survey, or on nothing.
- A Survey head that is no ancestor of the source commit is reported as `behind unknown`;
  refusing a stale Brief is a later decision (design note §7).
