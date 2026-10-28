# ADR-0253: The default Task pipeline lives in the catalog

**Status:** proposed (2026-10-28).

## Context

Every change to this repository is made through af Tasks (AGENTS.md). On 27 October 2026 the
owner decided that every Afactory issue runs through one default pipeline: one implementing
iteration by Opus 5.5 at high effort, two required reviewers (bugs on `gpt-5.6-terra`,
correctness on `gpt-6-sol`, both at high effort), one advisory reviewer for gate time and quality
in the `uix` slot (`gpt-6-astra` at medium effort) and an evaluator (`gpt-6-astra` at high
effort). Its five prompts were reviewed by `gpt-6-astra` at xhigh effort the same day. When they
were pinned here, three sentences of the implementer's prompt that were false about this tree
were corrected and nothing else: a test helper it named that does not exist, two test files it
called Linux-only that compile on macOS apart from one `#[cfg(target_os = "linux")]` block each,
and a nextest filter, `binary(af::it)`, that nextest refuses because the binary's name is `it`
(`binary_id(af::it)` selects it).

Until now that pipeline lived as local replacement packages in the owner's workstream, so only
one machine could plan it. The kernel's own Pipelines and Workers, `kernel/*` and `warm/*`,
already live in this repository's catalog, `.af/task-catalog.toml`, pinned by digest; AGENTS.md's
rule that project-specific pipelines belong in consuming repositories concerns what af ships to
other projects, not the Tasks that change af itself.

## Considered options

1. Keep the packages in the owner's workstream. Rejected: no other checkout can plan the
   pipeline every change runs through, and its bytes are reviewed nowhere in the repository.
2. Change the `kernel/*` packages in place to the default models, efforts and prompts. Rejected:
   the kernel packages serve campaign plans with other budgets and prompts, and the advisory slot
   here judges gate time and quality, not what `kernel/ux` judges.
3. Ship the default pipeline as a builtin starter. Rejected: starters are generic and
   credential-free for any repository, while this pipeline names this repository's gate, ADRs and
   vocabulary.

## Decision

The catalog carries the default pipeline under its own `default/` namespace: five Worker
packages and two Pipelines, each pinned by the digest `af catalog digest` prints.

- `default/implementation` is `kernel/implementation-ux` 1.2.0 with its five slots bound to the
  `default/*` Workers; `default/verification` is `kernel/verification-ux` with the same four
  judging slots and no implementer. The graphs, contracts and Attempt bounds are unchanged.

  | Slot | Package | Copied from | Model | Effort |
  |---|---|---|---|---|
  | `implementer` | `default/implementer` | `kernel/implementer-long` | `claude-opus-5-5` | `high` |
  | `bugs` | `default/bugs` | `kernel/bugs` | `gpt-5.6-terra` | `high` |
  | `correctness` | `default/correctness` | `kernel/correctness` | `gpt-6-sol` | `high` |
  | `uix` | `default/advisor` | `kernel/ux` | `gpt-6-astra` | `medium` |
  | `evaluator` | `default/evaluator` | `kernel/evaluator` | `gpt-6-astra` | `high` |

- Each Worker's `input.schema.json` and `outputs/*.schema.json` are byte copies of its kernel
  original's, and its signature's roles, types, contract ports, `evidence` and `retains` are the
  original's. Its `effects` are the original's or narrower: `default/advisor` declares
  `read-source` alone, where `kernel/ux` also declares `execute-checks`. Only the Attempt
  allowances differ: the implementer 3,000,000 tokens (1,500,000), bugs and correctness 500,000
  tokens and 40 minutes (400,000 and 30), the advisor 300,000 tokens and 30 minutes (900,000 and
  120), the evaluator 30 minutes (20).
- The `[providers]` table binds `default/implementer` to the neutral label `claude-main` and the
  other four to `codex-main`, as the kernel packages do; a machine whose registry carries other
  labels maps them in its local bindings file.
- The `uix` slot is advisory, as the catalog's `[review.reviewers]` already marks it: its
  reviewer is there for what the required reviewers are told to leave alone, above all the time a
  change adds to every future gate and pull-request validation, and the quality a maintainer
  cares about.
- The five prompts share these rules:
  - the judges never see the implementer's summary: the tree, `../base`, the requirements and
    the check receipts are their evidence, so everything the implementer wants judged must be
    visible in the tree;
  - one pre-existing rule: an issue is `minor` and "pre-existing" only when it lies outside the
    Task's obligations and the change neither introduces nor worsens it; one the goal requires
    fixing is an unmet requirement at its normal severity;
  - lost coverage, not deletion: a removed or replaced test is lost coverage only when no test in
    the tree carries its obligations; the judge weighs the mapping, not the deletion;
  - classes by behaviour: a measurement belongs in perf, and a functional, lifecycle or
    interaction test keeps its class whatever it costs, made cheaper instead of moved out;
  - honest severities: every finding at the severity its evidence supports, neither inflated to
    be heard nor deflated to stay out of the way;
  - the gate-time lens: what a change costs each schedule (the Task gate, pull-request
    validation, the full check), and the cheaper form that proves the same behaviour.

The catalog is proven honest by the existing integration test
`catalog_digest::every_published_package_pin_reproduces`
(`crates/af/tests/it/catalog_digest.rs`): it reads every `[packages]` pin of
`.af/task-catalog.toml`, the seven `default/*` pins among them, runs `af catalog digest` on each
pin's path and requires the printed digest to equal the pin.

## Consequences

- Any checkout plans the default pipeline from the repository's own catalog; `docs/tasks.md`
  ("The default pipeline") says how.
- The schemas are copies. When a kernel package's input or output schema, contract ports,
  evidence or retains change, the matching `default/*` package follows in the same change and
  both are re-pinned; a `default/*` manifest never declares wider effects than its original.
- In this release an advisory reviewer may stay unbound, but once bound its reports count toward
  convergence like any reviewer's, so the advisor's `blocker` or `major` keeps a round from
  converging. A separate kernel change that stops advisory reports from counting toward
  convergence is planned; until it lands the advisor's prompt says so.
- The owner's workstream now binds Providers only: it maps the five `default/*` packages to
  registry labels and carries no replacement packages.
- No persisted or printed shape changes.
