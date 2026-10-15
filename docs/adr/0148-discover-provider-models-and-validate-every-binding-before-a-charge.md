# ADR-0148: Discover each Provider's models, and validate every binding before anything is charged

**Status:** accepted (2026-10-15). Extends
[ADR-0147](0147-stop-the-claude-model-fallback-and-name-the-model-that-answered.md): grammar G and
the hint function stay the one spelling af prints and the one place its refusals are worded; the
static rules gain the alias rule and the effort vocabulary, a catalog read token-free from each
harness adds the catalog rules, and every refusal names what the Provider accepts. Builds on
[ADR-0146](0146-refuse-a-provider-cli-that-does-not-accept-afs-invocation.md) (the bare `codex
app-server`), [ADR-0112](0112-refuse-agent-mediated-provider-logins.md) (provider exit
codes), [ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)
(version every changed shape) and [ADR-0042](0042-require-provider-bindings-and-isolate-claude-reviewers.md)
(provider bindings).

## Context

[Issue #258](https://github.com/PakhomovAlexander/afactory/issues/258). A person or a coding agent
configuring an af Worker had to read Anthropic or OpenAI documentation to guess a model ID and
effort level, and learned the accepted spelling from a failed, charged run. `af provider` had
nothing that listed models; the only check was `Task Model binding requires an explicit
versioned model ID`, with no hint, and any effort level passed.

What the harnesses offer, verified 2026-10-15:

- Codex (codex-cli 0.162.1): after `initialize` and `initialized` on a bare `codex app-server`,
  `{"jsonrpc":"2.0","id":2,"method":"model/list","params":{}}` answers in under a second,
  token-free, with `data[]` entries carrying `id`, `displayName`, `hidden`, `isDefault`,
  `defaultReasoningEffort` and `supportedReasoningEfforts[].reasoningEffort` (`gpt-6.1-sol`,
  `gpt-6-astra`, `gpt-6-sol`, `gpt-6-luna` without `ultra`, `gpt-5.6-sol`, …). The CLI caches the
  catalog in `$CODEX_HOME/models_cache.json` with `fetched_at`. `-m` is free text; af passes the
  effort level as `-c model_reasoning_effort="…"`. `collaborationMode/list` needs an experimental
  capability and is not used.
- Claude Code (2.1.300): a `claude -p --input-format stream-json --output-format stream-json
  --verbose` session answers control requests before any turn in about 0.2 s, token-free:
  `list_models` returns the account's selectable catalog (what `/model` offers), where an alias
  carries `value` ≠ `resolvedModel`, and `get_binary_version` returns the version.
  `supported_models` does not exist. Exact IDs the API still accepts but the picker does not
  offer (`claude-opus-5`, `claude-opus-4-8`, …) are absent. Claude Code's documentation resolves
  `fable` to Fable 5.1, `opus` to Opus 5.5, `sonnet` to Sonnet 5.5, `haiku` to Haiku 5.5, and
  documents `--effort` as low, medium, high, xhigh, max; an effort above a model's maximum is
  silently run at the highest level the model supports. `claude -p --model claude-fable-5.1`
  fails as `There's an issue with the selected model`.

## Decision

1. **`af provider models [ID …] [--json] [--resolve SPELLING[/EFFORT]]`.** For each registered
   Provider (the named ones; the CLI default contexts when none is registered) af locates the CLI,
   runs the capability check of ADR-0146, and reads the catalog through the harness's own local
   protocol, under that Provider's auth directory, in the private probe directory and cleared
   environment `af provider status` uses, bounded by its probe timeout and cancellable. Claude:
   the two control requests on the stream-json session above, whose stdin af closes once both are
   answered, so no turn ever starts. Codex: `model/list` on the bare app-server, the version from
   `codex --version`, and the freshness from `fetched_at` of `models_cache.json` under the auth
   directory when it is a bounded regular file whose value af may print, else `live` (Claude is
   always `live`). Per Provider it prints a header (`source claude list_models` or `source codex
   app-server model/list`, the harness version, the freshness), one row per exact ID (display
   name, the aliases that resolve to it, its effort levels with the default marked `*`), and one
   sentence: af recognises these spellings for this Provider; they are what its harness offers,
   not every ID its API accepts; whether this account can run one is proven only by the charged
   `af provider doctor`. Claude rows are grouped by `resolvedModel`, named by their description's
   family and version (`Fable 5.1`), and gain the documented aliases the picker leaves out. An
   entry whose ID is outside grammar G is left out and counted; display names pass the
   credential filter and are bounded. `--resolve` reads an exact ID (confirmed), an alias (the
   exact ID it resolves to, and that af binds exact IDs), a display name case-insensitively, a
   family-and-version form with a space (`Fable 5.1`, `GPT-6 Sol`), a misspelling (the nearest
   catalog IDs: the same family or at most two edits away), or an exact ID the catalog does not
   list (`passes af's spelling rule` or fails it with the rule; usability only by `af provider
   doctor`); then checks the effort level against that model's levels; and, only for a listed
   pair, prints the review package's `[runner]` args for `reviewer.toml`, the Task package's
   `[runner]` block (`kind = "model"`, `provider_kind`, `model`, `effort`) for `worker.toml`, and
   the bindings line (`[providers] "PACKAGE" = "ID"`, and `--provider NODE=ID` for a review).
   `--json` prints `af/provider-models@1` (`schemas/provider-models-v1.json`). Exit 0; 4 when a
   CLI cannot start or is incompatible (outranks 7); 7 when a catalog is unavailable, in which
   case the static rules are printed and nothing is invented.
2. **One validation authority, `review_runner_claude::models`, beside grammar G.** Static rules,
   in one order: G on the model and on the effort level (unchanged, so a value outside G is
   refused with G's words and its byte length, never echoed); an alias the harness documents,
   refused with the exact ID it resolves to; a case or separator variant of a known exact ID
   (`claude-fable-5.1`), refused with that ID; the versioned-ID rule `execution()` applied (kept,
   with its words); the effort vocabulary of the kind, Claude `low medium high xhigh max`, Codex
   `low medium high xhigh max ultra`. Catalog rules when a catalog is at hand: an alias the
   catalog lists is refused with the exact ID it resolves to today, a near miss of a listed ID is
   refused with the nearest IDs, and an effort level the model does not list is refused with the
   levels it lists. An exact ID the catalog does not list that passes the static rules is
   accepted as `unverified against the catalog`. af never substitutes a model or an effort level.
   The module replaces `TaskProviderIdentity::check_spelling`; `check_static_rules` now takes the
   Provider ID and kind and delegates to it. It is applied in `TaskProviderIdentity::execution`,
   in the first pass of `bind_named_models` (`af task plan`, `af task start`/`run`, the Task path
   of `af review`) and of `review_task::local_workers` (`af review run`), in `af review plan`
   where the captured reviewer settings are read, in `af onboard` (its defaults and `--runner`
   profiles pass it, by test), in the package argument parsers of both adapters (so `af review
   render` and every Task Attempt refuse an alias before any process starts), and in the TUI. The
   Codex adapter depends on the Claude adapter crate for it.
3. **Plan time reads the catalog; runs do not.** `af task plan` and `af review plan` (with or
   without `--file`) apply the static rules over every Model Worker first, before any Provider
   process starts, then read each bound Provider's catalog once, without the capability check,
   and print one line per Model Worker: `model WORKER MODEL EFFORT  ok | unverified against the
   catalog | refused: HINT`, on stderr for a Task plan (before the Provider identity probes) and
   in the human `af review plan` output after the `provider` lines (on stderr under `--json`).
   A catalog the harness cannot give is skipped and the binding is `unverified`. A refusal
   refuses the plan. When a Provider is bound, the `af review plan` effects line says that its
   catalog was read token-free instead of claiming no Provider calls. `af task run`, `af task start --execute` and `af review run` apply the static
   rules only, so a run starts no Provider process the plan did not.
4. **Every refusal through the one hint function.** `hint::compose` gains
   `ModelOutcome::NotAccepted` (`model_not_accepted`) and `BoundModel::provider`. A refusal names
   the Provider ID and kind (a package parser knows only the kind: `a claude Provider`), the
   rejected spelling (as G prints it), the rule or catalog fact, the nearest accepted spellings
   when there are any, `af provider models <id>` (token-free) and a fixed next step that
   `hint::find` keeps finding. The runtime `model_unavailable` hint of ADR-0147 now names the
   Provider (the Claude Task adapter is told its ID), the nearest exact IDs the static rules
   know, and `af provider models <id>`, before its fixed next step. No secret, prompt text or
   path outside the repository is printed.
5. **TUI.** The Providers pane's discovery job reads each probed Provider's catalog after the
   inventory, bounded and cancellable, and `R` reads it again with the usage windows. Under each
   Provider a `models` section shows the header line; it unfolds to one row per exact ID (ID,
   display name, effort levels, aliases) when the Provider is opened or `m` unfolds it, and a
   missing catalog says why. The Workers pane's `runner` row ends with the verdict of 2 (`ok`,
   `alias -> ID` in the browser's printable ASCII, `effort not listed: …`, `unverified against
   the catalog`, or the refusal), from the Providers pane's catalog of that kind when it has
   one, from the static rules otherwise. The Tasks pane is unchanged.
6. **One new public document.** `af/provider-models@1` and its schema; every other document,
   record and event keeps its shape. The plan lines are text, and `af/review-plan@1` is
   unchanged.

## Considered options

- **Ship a model table in af.** Rejected. It goes stale with every harness release and would
  present af's guess as the Provider's catalog. Only the documented aliases are static, and only
  to refuse them with their exact IDs before any process starts.
- **Use Claude's `supported_models` control request.** Rejected: it does not exist in 2.1.300
  (`Unsupported control request subtype`).
- **Resolve an alias to its exact ID and bind that.** Rejected. af never substitutes; a binding is
  what a human approved. The refusal names the exact ID to copy.
- **Accept only catalog IDs.** Rejected. Claude's picker leaves out exact IDs the API accepts, and
  deployments name their own; such an ID is accepted as unverified and `af provider doctor`
  proves it.
- **Read the catalog at run time too.** Rejected. A run would start a Provider process its plan
  did not, and the plan already said what the catalog said; the static rules still guard the run.
- **Add the verdicts to `af/review-plan@1` or the Task plan document.** Rejected. That changes a
  published shape (ADR-0145) for what one text line per Worker says.

## Consequences

- `af provider models` tells a person or an agent the exact IDs, aliases and effort levels each
  Provider accepts, and `--resolve` prints the configuration to copy, without spending a token or
  reading a credential.
- A binding to an alias, a misspelling of a known exact ID or an effort level the Provider does
  not accept is refused at plan time, before any Provider process starts, and again by the
  adapters before any process starts; Claude Code never silently runs another effort level.
- Reviewer packages that named an alias (`--model opus`) are refused until they name the exact
  ID; the repository's own `.af` packages already do.
- `af task plan` and `af review plan` start one more token-free Provider process per bound
  Provider, bounded by the probe timeout.
