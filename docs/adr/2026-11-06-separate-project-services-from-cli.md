# ADR-2026-11-06-separate-project-services-from-cli: Separate Project services from CLI handlers

**Status:** accepted (2026-11-06).

## Context

Project identity, registration, Workspace management and Project Store replay lived in the `af`
binary alongside command parsing and presentation. Survey measurement, collection and Brief
construction also called concrete machine configuration and Task frontend helpers. These services
need the existing machine rules, but do not own the configuration ladder or the Task CLI.

## Considered options

- Keep services in the binary and re-export them: rejected because another library cannot depend
  on a binary, and the application dependencies remain implicit.
- Move the complete configuration ladder, Task commands and TUI into Project: rejected because
  those application responsibilities extend beyond Projects and would invite dependency cycles.
- Copy policy parsing, Git isolation or Task binding readers: rejected because duplicate readers
  and security rules could disagree about authority, cleanup ownership or diagnostics.
- Extract services behind explicit application dependencies: chosen to retain the existing
  operation order and contracts while making their ownership independent of clap and presentation.

## Decision

1. `af-project` owns Project and Workspace identities, Project Stores and log replay, path records,
   registration and selection, Workspace creation/adoption/removal and identity-bound recovery.
   Survey measurement, pending records, collection inventories and execution, and Brief policy,
   rendering, capture, captured-input resolution and receipt assembly belong to this library too.
2. `af_project::Host` supplies state-root lookup, default Task Store paths, filesystem path
   resolution, best-effort machine Store registration, full declaration validation, effective
   Survey/Brief configuration tables and a diagnostic sink. The binary implements it through its
   existing application services. Lookup remains lazy: pure identity, replay, rendering and
   read-only Store opening do not consult machine configuration or register a Store.
3. `project_policy` remains a separate CLI module that validates `.af/af.toml`. The library does
   not acquire the full configuration ladder, TUI or Task frontend. Routing and Survey patterns
   share the existing bounded matcher in `af_pipeline_config::path_glob`. Brief receipt assembly
   takes the application's existing recorded-input-bindings reader as an explicit dependency;
   execution-time Brief resolution still reads only the captured Task revision and its CAS.
4. Project Git operations retain their explicit directory, private home, isolated environment,
   deadlines and `af_process` supervision. The unchanged isolated command constructor and empty
   linked-worktree check live in `af_project::worktree_git`, shared with Task delivery. No process
   supervision or sandbox policy is copied. Cleanup still owns only the objects and identities
   that the existing creation/removal records authorize.
5. `af` retains clap dispatch, terminal and JSON output, lazy-registration warnings and command
   selection, and concrete machine/Task adapters. Workspace services return operation results for
   that presentation. Store registry writes keep their previous best-effort semantics and timing;
   read-only Brief previews keep creating and registering nothing.
6. Existing domain unit tests move with their implementation; CLI integration tests continue
   through the isolated binary helpers. Two in-process dependency tests prove that registration
   uses injected state paths, registry and declaration validation, and that Survey/Brief policy
   loading uses injected configuration with the same refusal wording. Test classification and
   generated protected-harness inventories include the new crate.

## Consequences

Runtime behavior, CLI spellings and outputs, IDs, on-disk formats, captured authority, event names,
schema IDs and JSON shapes remain unchanged. There is no migration, compatibility reader or alias.
The workspace gains one internal library and no third-party dependency. Existing gates, budgets,
process deadlines, cleanup ownership and isolation remain unchanged; the extraction introduces no
new ambient effect or publication authority.
