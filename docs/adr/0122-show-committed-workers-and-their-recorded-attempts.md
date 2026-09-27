# ADR-0122: Show committed Workers and the Attempts their recorded plans bound

Status: accepted, 2026-09-26.

## Context

[`docs/design/tui.md`](../design/tui.md) §5.3 plans the browser's Workers pane, and §7 step 4
delivers it as package M4. The bar lists this scope's Worker packages. One Worker's pane shows
its identity, the state of its Attempts in this scope's Task Stores, and its prompt.
[ADR-0119](0119-open-a-read-first-browser-on-bare-af.md) binds the browser as a projection that
never writes a Store and never turns working-tree bytes into authority.
[ADR-0121](0121-show-recorded-tasks-in-the-browser.md) reads Tasks through their inspection
documents.

Five facts shape the answer:

- A reviewer Worker is `.af/workers/<name>/reviewer.toml`. `af.lock` pins it by name under
  `[workers]` and records no path; the reviewer registry reads `.af/workers/<name>`. A Task
  Worker package is a `worker.toml` directory, and `.af/task-catalog.toml` pins it by name with
  its `path`.
- An Attempt names its node only through its reservation's invocation artifact, which names the
  node and the plan. The document behind `af task explain --json` carries the current plan's
  compiled graph. The graph gives a node's operator, and the operator its slot, and
  `graph.slots` binds each slot to a Worker by name. An earlier plan's graph is reachable
  through that plan's `compiled_graph_id`.
- An owned child, such as the Review shard `parent.slice1`, is not a node of the graph. The
  document's `owned_child_sets` name the invocation of the node that registered it, and
  `graph.owned_children[that node]` records the operator every child of that node runs.
- A Review-domain operator runs a reviewer package and a primitive runs a Task Worker package.
  The two may share a name and are still two Workers, so STATE matches the kind as well as the
  name.
- A plan binds each slot to an exact package: its `bindings[slot].package_digest`. STATE counts
  only the Attempts whose plan bound the digest the committed pin records, so a package changed
  since a Task ran does not inherit that Task's Attempts. Attempts of the same name at another
  digest are counted on one `other` line, so the history does not silently vanish.
- The recorded package artifact carries a name, a version and a digest, but no path. The
  kernel resolves a slot's Worker by name through the committed pin.
- The kernel's own prompt conventions are fixed. The reviewer adapters send `reviewer.md`, and
  a Task model Worker receives `instructions.md`. No declaration names a prompt file.
- The browser's existing goldens paint the bar with an empty `workers/` folder, and the hub
  fixture always commits two Task Worker packages. A Workers folder filled on start would
  change every other pane's golden render.

## Options

- **Match Attempts to Workers by node or slot name.** This was rejected. A node named
  `implementer` may run the evaluator's slot, and a name says nothing about which package ran.
- **Index Attempts by Worker in the Store** (§8's first open question). This was rejected for
  this package, because it changes the Store's schema for a display concern. The scan stays,
  and it runs only when the pane is opened.
- **Guess a prompt from any Markdown file beside the declaration** (§8's second open
  question). This was rejected. The pane would then show a file the kernel never sends.
- **Resolve every reservation through the recorded plan the invocation ran under, then the
  Worker through the committed pin.** This was chosen.

## Decision

### Discovery

The Workers pane reads the one commit `HEAD` resolves to, with git in a cleared environment,
through the Pipelines pane's own helpers (`head`, `git`, `committed`, `differs`, `declared`,
`pinned_paths`):

- It lists `.af/workers/*/reviewer.toml` exactly one level deep.
- It lists every `worker.toml` under `.af/task-packages/`, `.af/packages/` and `.af/vendor/`,
  however deep.
- Sources come in that order, and entries within a source come by path. When more than one
  source has Workers, each source is a folding group named after its directory.
- A declaration or prompt whose working-tree file differs from `HEAD` marks the bar label with
  `*`, and the pane names the file. `HEAD` is what is shown. A prompt `HEAD` does not commit
  differs when the working tree has one, even untracked, since `git diff` does not see it.
- A failed read of `HEAD` is shown above the last good entries of the same repository. Their
  drift is still read, against the commit they were read from.
- A scope change drops everything read for the old scope.
- The user scope lists nothing and says that Workers belong to a project.

The pane reads nothing until its folder or one of its entries is first opened. After that, a
load, `R` and every open read `HEAD`, the working tree's drift and the Stores again, so a
reopened Worker shows the Attempts settled since it was last opened.

### Identity

The Identity section reads the committed declaration. It shows the name, version, schema,
package directory, pin, runner, args, `signature.attempt.tokens` and `wall_ms`, effects and
roles. A field the declaration does not carry is `-`.

The pin is the committed catalog's entry for the declared name when its `path` is this package,
or `af.lock`'s `[workers]` entry when this is `.af/workers/<name>`. It shows the lock file, the
version and eight hex digits of the digest. When the lock places the name at another
directory, the pin is `pinned elsewhere`, and without an entry it is `unpinned`.

The runner line shows the kind, then the program or the provider kind, then the model and the
effort. A reviewer declares its model and effort in its args, and they are read with
`review_config::lock::reviewer_runner_settings_from_manifest`, the kernel's own reader. The
args are shown as declared, joined as shell words.

### State

STATE reads the Task Stores the Tasks pane reads for the scope, with the same refusals.
`tasks::targets`, `tasks::not_a_store`, `task_execution::list_common` and
`task_execution::inspection_document(…, true)` give each Task's `af task explain --json`
document. For each `reserved` record:

1. `tasks::node_of` resolves its invocation to a node and a plan.
2. The graph of that plan comes from the document for its current plan, or from the recorded
   plan's `compiled_graph_id`.
3. The node's single slot is read from a primitive operator's `slot` or a Review operation's
   `slot`. `graph.slots[slot].worker` names the Worker.

A node with no slot, or with several (a Provider admission, an optimization experiment), is
credited to no Worker. The Worker name then reaches exactly one listed package, the one the
committed pin places at that name. A package the pin does not place here says that no plan
binds it.

The accounting is the Tasks pane's:

- Each reservation is open (`reserved`), settled ok, settled failed, or released.
- A released reservation is not an Attempt. It is counted as released, and never charged or
  timed.
- An Attempt's charge is the highest charge its settlement or a usage observation records.
- The wall is the sum of its recorded `attempt_walls`, with how many Attempts recorded one.

A Task whose records cannot be read refuses its Store, named with the Task, as in the Tasks pane.

### Prompt

PROMPT is `reviewer.md` for a reviewer Worker and `instructions.md` for a Task Worker package,
as `HEAD` commits it, shown as plain scrollable text. A package without that file says so, and
nothing else is guessed. `gf` at or below the PROMPT rule opens the prompt's working-tree
path, even when `HEAD` commits no prompt; above it, `gf` opens the declaration. `gf` on a bar
entry opens the prompt when `HEAD` commits one, and the declaration otherwise. `y`, in the pane
or on a bar entry, copies the Worker's declared name, never the drift marker; a declaration
without a name yanks nothing, since its label is only its directory.

## Consequences

The Workers pane cannot disagree with `af task show --json`. A pseudo-terminal test drives bare
`af` at 100x30 and 80x24 over a Task that `af task start --execute` recorded, and it compares
the STATE counts with counts it derives from that document and the Store's artifacts. A golden
at 100x30 pins the bar and one Worker's pane, and the other panes' goldens are unchanged.

- The Workers folder is empty until it is opened, and `/` in the bar finds no Worker before
  then.
- STATE scans every Task of the scope's Stores on each read. An index by Worker in the Store
  remains the fix if that scan grows slow.
- Attempts of an operator that serves several slots are credited to no Worker.
- No wire contract, schema, fixture or `--json` document changes. `CONTEXT.md` gains no term.
