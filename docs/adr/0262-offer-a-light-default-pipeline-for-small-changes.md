# ADR-0262: Offer a light default pipeline for small changes

**Status:** proposed (2026-10-29).

## Context

Until this decision every change to this repository ran through the default Task pipeline the
catalog pins ([ADR-0253](0253-the-default-task-pipeline-lives-in-the-catalog.md)): one
implementing iteration by Opus 5.5 at high effort, two bound reviewers whose findings count
toward the round (bugs, correctness), one advisory reviewer for gate time and quality (`uix`) and
an evaluator. For a small, local issue (one function, one
sentence of a document, one Task package) that is more judging than the change needs: four
Provider calls after the implementer and Attempt allowances sized for work that spans crates.

On 29 October 2026 the owner asked for a light variant for such issues: one implementing
iteration by Opus 5.5 at medium effort, the correctness reviewer (`gpt-6-sol` high) and the
evaluator (`gpt-6-astra` high), no bugs or advisory reviewer, and smaller Attempt allowances.

The Review policy is one per catalog: `[review.reviewers]` names every reviewer and marks each
`required` or `advisory`. The kernel refuses a review reducer that omits a `required` reviewer, at
planning (the reduce signature's `unbound_allowed` in `review_signatures`) and again when the
Review domain is captured (`ReviewTaskDomain::captured`,
`crates/review-pipeline/src/task/review.rs`). With `bugs = "required"`, no pipeline of this
catalog can leave the bugs reviewer out.

## Considered options

1. A second catalog for light Tasks. Rejected: a Task plans against one catalog, and the second
   would duplicate every pin the default pipeline shares.
2. Make the light pipelines bind `bugs` to a Worker that always answers clean. Rejected: a fake
   reviewer is a recorded review that never happened.
3. Mark `bugs` advisory in the catalog so a pipeline may leave it unbound, and pin light packages
   under their own namespace. Chosen.

## Decision

The catalog carries the light default pipeline under `default-light/`: three Worker packages and
three Pipelines, each pinned by the digest `af catalog digest` prints.

- `default-light/implementation` is `default/implementation` without the `bugs` and `uix` slots,
  calling `default-light/review`; `default-light/verification` is `default/verification` without
  them. `default-light/review` is `kernel/review-light` with only the `correctness` slot and node.
  Both implementation Pipelines declare `findings`, so `af task run NEW --from TASK` continues
  them.

  | Slot | Package | Copied from | Model | Effort | Attempt allowance |
  |---|---|---|---|---|---|
  | `implementer` | `default-light/implementer` | `default/implementer` | `claude-opus-5-5` | `medium` | 1,000,000 tokens, 2 h |
  | `correctness` | `default-light/correctness` | `default/correctness` | `gpt-6-sol` | `high` | 300,000 tokens, 30 min |
  | `evaluator` | `default-light/evaluator` | `default/evaluator` | `gpt-6-astra` | `high` | 200,000 tokens, 20 min |

- Each Worker's `input.schema.json` and `outputs/*.schema.json` are byte copies of its `default/*`
  original's, and its signature's `effects`, `evidence` and `retains` are the original's. The two
  judges' prompts are their originals' except where a sentence would be false of a pipeline
  without a bugs reviewer: the correctness prompt says it is the only reviewer and owns, beside
  its cross-cutting view, the concrete functional defects on the paths the change touches (a
  sixth item to judge, and such a defect counts as `major`); the evaluator prompt says the one
  correctness reviewer reviewed the candidate for defects. The implementer's prompt says that no
  bug-bounty or gate-time reviewer follows, so testing the edge cases of every path it changes
  and counting the gate time it adds are its own job; it asks for a reply within 90 minutes of
  its 2 h Attempt wall, since a wall-ended Attempt fails and its edits are kept only as an
  unverified partial candidate for a later continuation
  ([ADR-0257](0257-seal-the-implementers-tree-when-its-attempt-wall-ends-it.md)); and it names
  the findings ids a continued Task may carry.
- The `[providers]` table binds `default-light/implementer` to `claude-main` and the two judges
  to `codex-main`, as for the default pipeline.
- The catalog's `[review.reviewers]` marks `bugs` `advisory` (it was `required`). Its exact
  effect:
  - a pipeline may leave `bugs` unbound: the planner admits a reduce without it, and the
    captured Review domain admits the graph and leaves `bugs` out of the round's missing
    reviewers;
  - a pipeline that binds `bugs` (every other review Pipeline of this catalog: `default/*`,
    `kernel/review-light`, `kernel/review-code` and the Pipelines that call them,
    `warm/review-light`) counts its findings toward convergence and records a missing result as
    a missing reviewer, which leaves the round incomplete, exactly as before;
  - a benchmark Demand the bound bugs reviewer files is now recorded with requirement
    `advisory`: the reduce passes each reviewer's policy requirement into its canonical stage, and
    `Ledger::required_open_demands` counts only required Demands, so an open one no longer keeps
    the round from converging or fails a repair acceptance.
- Light Tasks fit these limits; the local gate alone reserves 2 h 10 min of the wall:
  `tokens` 2500000, `max_attempts` 7, `wall_ms` 18600000; `verification` `tokens` 1200000,
  `attempts` 5, `wall_ms` 11100000.

The proof:

- `catalog_digest::every_published_package_pin_reproduces`
  (`crates/af/tests/it/catalog_digest.rs`) reads every `[packages]` pin of
  `.af/task-catalog.toml`, the six `default-light/*` pins among them, and requires
  `af catalog digest` on each pin's path to print the pin.
- `advisory_reviewer_tests::an_advisory_bugs_reviewer_admits_a_correctness_only_review_graph`
  (`crates/review-pipeline/src/task/review.rs`) builds one review graph that binds only
  `correctness`: under a policy marking `bugs` advisory the reduce signature lets it stay unbound
  and `ReviewTaskDomain::captured` admits the graph; under one marking it required the signature
  does not, and `captured` refuses the same graph.

## Consequences

- A small, local issue runs with one implementing Attempt at medium effort and two judges; a
  change that is not small and local still runs through the default pipeline. `docs/tasks.md`
  ("The light default pipeline") says how to choose and run it.
- The light pipeline gives up the bug-bounty and the gate-time reviewer. The correctness
  reviewer's prompt takes over the hunt for concrete functional defects on the changed paths,
  and the implementer's prompt asks it to test their edge cases; the implementer alone counts
  the gate time a change adds, and nothing checks it did.
- The schemas are copies. When a `default/*` judge's schema, contract ports, evidence or retains
  change, the matching `default-light/*` package follows in the same change and both are
  re-pinned; so does a change to a `default/*` judge's prompt, apart from the sentences above
  about which reviewers run.
- The bugs reviewer's benchmark Demands no longer block convergence in any pipeline of this
  catalog; its findings do.
- No kernel code and no persisted or printed shape changes.
