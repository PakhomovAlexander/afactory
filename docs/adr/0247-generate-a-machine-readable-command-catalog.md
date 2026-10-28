# ADR-0247: Generate a machine-readable command catalog and ship the schemas

**Status:** accepted (2026-10-28). Adds the printed document `af/command-catalog@1`
([ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)) and the
`af schema show` command; a command's `--help` stays its contract
([ADR-0146](0146-refuse-a-provider-cli-that-does-not-accept-afs-invocation.md)).

## Context

af's documents are versioned and indexed in `af help json`, but the commands themselves had no
machine-readable description. An agent learned their flags by reading help text, and whether a
command is token-free, charges tokens, writes state or needs a person was stated only in prose
(issue [#374](https://github.com/PakhomovAlexander/afactory/issues/374)). The schemas of the
documents af prints lived only in the repository's `schemas/`, which an installed af does not
carry.

## Decision

1. **`af help --json` prints the catalog.** One `af/command-catalog@1` document
   (`schemas/command-catalog-v1.json`): `af_version`, `scope` (the command it starts at, `af`
   for the whole tree) and `commands`, the scope's command and every visible command below it,
   depth first in definition order. `af help COMMAND... --json` is the same document scoped to
   that command, an alias naming its command (`af help providers --json` is `af provider`).
   Under `--json` every word is a command path, never a topic; a path that names no visible
   command is one `af/error@2` document, exit 2, as the text form exits.
2. **Generated, never written by hand.** `command_catalog` walks the clap tree the binary parses
   (`cli::Af::command()`, built so global arguments propagate). Each command carries `command`
   as typed, `path`, `about`, visible `aliases`, `usage` (the lines `--help` prints),
   `runnable`, `subcommands` and `arguments`; each argument its `name`, `long`, `short`,
   `positional`, `value_names`, `takes_value`, `repeatable`, `required`, `global`, `default`,
   `possible_values`, `env`, `heading` and `help`. Hidden commands and arguments and `--help`
   are left out. A command is `runnable` when it has no visible subcommand, or when it takes
   arguments of its own and does not require a subcommand (`af`, `af review`, `af storage`).
3. **One effects table, one entry per runnable command.** `command_catalog::EFFECTS` names each
   runnable command as typed and says what it may do, never that it will:
   - `tokens`: `free` (it never calls a model) or `charges` (it may dispatch model Workers,
     charged against a budget), with `charges_when`, null when any run may.
   - `writes`: each state it may write. `store`: a Task Store or a review Campaign's Store.
     `files`: files outside af's own state, in the repository, a worktree, a branch or a path
     the command names. `machine`: af's machine-local state, the Provider and Project
     registries and Project Stores, configuration, installed releases, shell completions and
     man pages, telemetry and caches. `remote`: off this machine, the gate branches and draft
     pull request of a Remote Check
     ([ADR-0140](0140-run-a-declared-check-through-a-gate-pull-request.md)).
   - `human`: each thing it may need from a person. `login`: a Provider's official login, at a
     private terminal or through the host's private channel. `approval`: a developer's
     signature over an exact decision payload, made outside af. `terminal`: an interactive
     terminal, for a screen or an editor.
   - Every write and need carries `when`: null for always, else the flag as typed (`--apply`) or a
     short phrase naming the condition (`a pipeline with remote checks`).
   Housekeeping every command shares is not an effect: telemetry, the storage sweep after a run
   ([ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md)), the release check's schedule, a
   pinned release installed on demand, and the lazy registration of the repository's Project and
   checkout. The vocabulary is the schema's enums; `af help json` explains it.
4. **The table is held to the tree.** A unit test fails when a runnable command has no entry,
   when an entry names no runnable command or is listed twice, and when a `when` names a flag its
   command does not take; another proves each of those is detected. A new command therefore
   cannot ship without saying what it does.
5. **`af schema show ID` prints a shipped schema.** The schemas of every document `af help json`
   names are compiled into the binary, so `af schema show af/task-status@2` prints
   `schemas/task-status-v2.json` byte for byte, and `af schema show` alone lists the IDs with
   their titles. An ID it does not ship exits 1, naming the versions of the same document it does
   ship. A unit test holds the shipped set to exactly the IDs the `json` topic names, both ways,
   and each schema's `schema` constant to its ID. Like `af help`, `af schema` describes the
   running af: neither dispatches to a project's pinned release.

## Considered options

- **Hand-write the catalog as a JSON file.** Rejected: it drifts from the parser the first time a
  flag is added, which is the problem the issue names.
- **Derive effects from the help prose (`token-free`, `--apply`).** Rejected: prose is for people,
  and a missing word would silently classify a command as harmless.
- **Annotate effects on each clap definition.** Rejected: clap carries no field for them, and
  spreading them through `cli.rs` hides the one place a reviewer compares them. One table beside
  the generator, held to the tree by a test, is reviewable at a glance.
- **List lazy Project registration and the storage sweep as effects of every command.** Rejected:
  they would mark nearly every reading command as a writer and hide the distinction an agent
  needs; they are stated once, in the schema and in `af help json`.
- **Ship every file under `schemas/`.** Rejected: most describe persisted records no command
  prints; the shipped set is the documents `af help json` lists, and a document added there must
  ship its schema.

## Consequences

- An agent reads `af help --json` once instead of scraping help pages, and checks a command's
  `effects` before it first runs it; `af help task run --json` answers for one command.
- Adding a command means adding its effects entry; adding a document to `af help json` means
  shipping its schema.
- The binary carries the shipped schemas, about 200 KiB.

Tests: af's `command_catalog::tests` (`every_runnable_command_has_one_effects_entry_and_every_entry_names_one`,
`a_missing_entry_an_unknown_command_and_an_unknown_flag_are_problems`,
`the_catalog_is_generated_from_the_command_tree`) and `shipped_schemas::tests`
(`the_shipped_schemas_are_exactly_the_documents_af_help_json_lists`,
`every_shipped_schema_describes_the_document_of_its_id`); `cli_surface::help_json_prints_the_generated_command_catalog`
and `cli_surface::schema_show_prints_every_id_af_help_json_lists`.
