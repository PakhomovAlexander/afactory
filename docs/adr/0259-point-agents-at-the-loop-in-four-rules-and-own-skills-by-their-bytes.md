# ADR-0259: Point agents at the loop in four rules and own skills by their bytes

**Status:** accepted (2026-10-28). Supersedes in part
[ADR-0241](0241-install-an-agent-entry-point-from-the-help-page.md): the pointer's content and
the rule that decides which skill af owns. Amends
[ADR-0242](0242-classify-first-run-refusals.md): the binding refusal names
`af provider detect` when a Worker's kind has no registered Provider, and `af review plan`'s
refusal carries the plan it computed; its selectors refusal names the selectors af would choose
whenever it finds the default branch. Supersedes in part
[ADR-0247](0247-generate-a-machine-readable-command-catalog.md): under `--json`, words that name
no command but a help page print that page as `af/help-topic@1`. Keeps
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md) and
[ADR-0148](0148-discover-provider-models-and-validate-every-binding-before-a-charge.md).

## Context

Issue #363's acceptance is an agent experiment: with what `af setup agents --write` installs, a
fresh Codex session does a token-free Task job in 6 commands or fewer and a review job in 10 or
fewer. On 28 October 2026 it took 16 and 18 commands for the Task job and 20 and 16 for the
review job; a hand-written four-rule pointer had taken 5 and 13 on 0.13.0. The traces named the
causes:

- The pointer between the `af-agent-help` markers prescribed six commands before any work:
  `af help agents`, `af help json`, `af help exit-codes` and `af help task-file` "for machine
  contracts", then `af provider status --json` and `af onboard` "to inspect readiness". Every
  session ran all six. Inside an agent's sandbox the first readiness command exits 8
  (`probe_denied`), and `af onboard` exits 1 in a Task starter directory, so two failures came
  before any work.
- The page's rule "read a command's `effects` before you run it for the first time" made the
  sessions run `af help COMMAND --json` for commands the page itself describes.
- With no Provider registered, `af review plan` refused with `review_binding_missing`, a null
  `next_action` and a candidate map in Rust debug form (`{"architecture": Array []}`), and
  dropped the token-free plan and `model` lines it had just computed (#434). The sessions read
  `.af/` by hand to learn the reviewers and their models.
- Codex CLI 0.164.0 refused both generated skills: a skill must open with YAML frontmatter, and
  af wrote an `af-generated-skill` digest line before it (#394 finding 1). The digest marker was
  also forgeable: any file whose first line was the digest of the rest counted as af's, so
  `--write` replaced a foreign skill that carried a recomputed marker (#394 finding 2).
- `af provider detect` printed "every authenticated login found here is registered" when an auth
  directory existed but was logged out: it counted only rows waiting to be registered (#394
  finding 3).

The experiment then ran again with decisions 1 to 5 installed: every answer was complete and
correct, but the Task jobs took 13 and 9 commands and the review jobs 19 and 19. Three defects
caused it:

- `af help agents --json` exited 2: under `--json` every word was a command path (ADR-0247).
  The pointer's second rule says to add `--json` to each command, so every session ran it first,
  and three of the four concluded the page did not exist and looked commands up one by one.
- On the default branch with an uncommitted change and no Provider registered,
  `af review plan --json` refused with `review_selectors_missing`, a null `next_action` and the
  sentence that the default branch cannot be determined, although af had found it: the whole
  suggested command was dropped whenever a binding was not unique.
- `af review plan --uncommitted --policy-rev HEAD --json` failed with an internal capture error
  (`committed snapshot has no resolved tree authority`): the dirty worktree's Snapshot was
  published before the check that refuses a diff plan without `--base`, which `af review run`
  passes correctly.

## Options considered

- Keep the six commands and make each succeed inside a sandbox: the readiness commands are
  right to refuse there, and a refusal under `--json` already names its fix; the steps cost
  commands before work either way.
- Keep the digest marker and also require frontmatter: the marker stays forgeable, since a
  foreign file can always recompute a digest of its own bytes.
- Bind a Provider implicitly when the machine has exactly one: ADR-0042 and ADR-0242 require an
  explicit binding, and the suggested command already spells it.
- Print the plan beside the refusal as a second document: under `--json` a command prints
  exactly one document, so the plan travels inside the error's `details`.
- Print a help page under `--json` as the command catalog's document, or resolve a topic before
  a command: the catalog describes commands, and `af help COMMAND --json` must not change for
  the words that are both (`config`, `self`, `storage`, `telemetry`).
- Suggest bindings for the Workers that have one candidate and leave the rest out: the command
  would bind part of the review, and ADR-0242 names bindings only when all of them are known.

## Decision

1. **Four rules.** The block between `<!-- af-agent-help:start -->` and
   `<!-- af-agent-help:end -->` of `docs/agents.md` holds four rules in at most 6 lines of at
   most 100 columns: run `af help agents` first, it is the whole loop; add `--json`, with which
   each command prints one versioned document; branch on `state.outcome` and run
   `state.next_action`, never guess a next command; never add `--login` in an agent session, a
   login is the person's and is done privately. It names no other command. It is still
   extracted from the page, so `af help agents --snippet`, the installed `AGENTS.md` block and
   both skills keep one source.
2. **The page.** `af help agents` starts the loop at step 1 with no readiness check first. The
   references (`af help json`, `af help exit-codes`, `af help task-file`, `af help task-states`,
   `af help errors CAUSE`) are read when a document, an exit code, a Task file, a state or a
   refusal needs them; the readiness commands are for when a refusal points there or the person
   asks. A command the page describes, or one a `next_action` names, needs no `effects` lookup
   first; any other command's `effects` are read before its first run. A review outside a Task
   starts with `af review plan --json`.
3. **Skills.** Both `SKILL.md` files open with YAML frontmatter on line 1, `name: af` and a
   `description` saying when to use the skill; the note that `af setup agents` generated the
   file, with the skill's version, follows the frontmatter. Ownership is by exact bytes:
   `--write` replaces an existing skill only when its bytes equal the current skill or one
   entry of the versioned list of earlier generated skills kept in code, which holds the bytes
   the ADR-0241 generator wrote (version 1). Any other content, a body under a recomputed
   `af-generated-skill` line included, is refused as foreign before any write. A change to the
   skill's bytes takes the next version and adds the bytes it replaces to the list. An
   `AGENTS.md` block between the markers is replaced whatever it held, its foreign text kept.
4. **Detect.** `af provider detect` words its text summary from the authenticated rows: with
   none, it never says every authenticated login is registered; with an auth directory that is
   logged out, it says no authenticated login was found and points to that row's private login
   step, which it never runs.
5. **The binding refusal.** For `af review plan` and `af review run`: when a missing node's kind
   has no enabled registered Provider, `next_action` is `af provider detect` and the message
   names each such kind with its nodes in words and how to register one; when every missing
   node's kind has exactly one candidate, `next_action` stays the whole command with
   `--provider NODE=ID` (ADR-0242); otherwise `next_action` is null and the message lists the
   choices as `NODE: ID, ID`. No refusal prints a candidate map as Rust debug output or JSON,
   the `review_selectors_missing` candidates included. `af review plan`'s refusal carries the
   token-free plan it computed: `details.plan` is that `af/review-plan@1` document, unchanged
   in shape, and its `model` lines go to stderr as on the success path (ADR-0148); the text
   form prints the plan, with the reviewers' model lines, the gates, the convergence and the
   budgets, before the refusal, whose last line is the next command when there is one. `af
   review run`'s refusal carries no plan. `af/error@2` is unreleased, so `schemas/error-v2.json`
   gains the optional `plan` without a version bump; nothing binds a Provider implicitly.
6. **Help pages under `--json`.** `af help WORDS --json` prints the command catalog when the
   words name a command, as before. Words that name no command but a page `af help` prints as
   text, a topic or `errors CAUSE`, print exactly one new document, `af/help-topic@1`
   (`schemas/help-topic-v1.json`): the page's `name`, its one-line `description` (null for
   `errors CAUSE`, whose text form has no title line) and `text`, the page below its title line
   and the blank line after it (all of it for `errors CAUSE`), without the final newline: the
   text form prints exactly the title line, a blank line, `text` and a newline. One function
   renders both forms, so they cannot drift. Words that name neither, `errors` and a cause af
   does not know among them, exit 2 in both forms; under `--json` they print one `af/error@2`
   document, which for unknown words names both the pages and the commands. `af help --json`,
   `af help COMMAND... --json` and `af help agents --snippet` are unchanged.
7. **The selectors refusal names its selectors.** When af finds the default branch and the
   selectors it would choose resolve, the `review_selectors_missing` refusal's `next_action` is
   the command with them: `--policy-rev`, `--base` when not given, `--uncommitted` or
   `--candidate` as before, and `--provider NODE=ID` for the unbound model Workers only when
   every one of them has exactly one enabled candidate. The message names that command and,
   while bindings remain open, each reviewer that still needs one in words; that command's own
   `review_binding_missing` refusal then names the fix (decision 5). The message says the
   default branch cannot be determined only when it cannot, and the policy revision stays the
   person's choice then; when the selectors af would choose do not resolve, it gives the
   reason, such as a clean worktree on the default branch, with no command.
8. **A diff plan without a Base.** `af review plan --uncommitted` without `--base` is refused
   before the worktree's Snapshot is captured, as `af review run` refuses it:
   `review_selectors_missing`, `details.selector` `--base`, and a `next_action` that adds
   `--base` to the command as given. No capture error reaches the person. Routing reads the
   changed paths, and without a Base there are none: when `routing.unmatched = "refuse"` would
   refuse that empty set and every pipeline a route selects is a diff pipeline, `af review plan`
   and `af review run` give this same refusal, since the Base is what is missing. With a
   whole-tree pipeline among the routes, or with no route, the routing refusal stands.

## Consequences

An agent's first command is `af help agents`, and its next ones are the loop's. A refusal is
where readiness is learned, and the commands it names are the fix. A review refusal shows what
the review would run without a second command. A skill af did not write byte for byte is never
overwritten, and a new skill version must carry the one it replaces in code, or an installed
skill is refused as foreign on upgrade. `af help errors review_binding_missing` states the three
cases. The pointer's `--json` rule holds for `af help agents` too, and `af/help-topic@1` is one
more shipped schema `af schema show` prints. A review preview with missing selectors and
bindings takes two refusals, each naming the next command, instead of a dead end.
