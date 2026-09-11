# Captured local Worker bindings

Task-file commands accept an explicit `--bindings FILE`. The file uses
`schema = "af.task-bindings/1"` and may supply `slots`, `providers` and exactly pinned
`packages`. Omitted tables are empty. For an alternative Worker already in the committed
catalog, a local file can be as small as:

```toml
schema = "af.task-bindings/1"

[slots]
"root.slots.implementer" = "team/fast-implementer"

[providers]
"team/fast-implementer" = "codex-personal"
```

Use `af task plan --file ticket.json --bindings alice.toml --json` to inspect the effective
plan before execution. The same option works with `task start`, `review run --file` and
`review plan --file`. Model, effort and Attempt cost are properties of the replacement
Worker package; its Provider alias supplies the locally admitted account.

Additional packages must use `local/*` names, exact version/digest pins and paths beneath
the bindings file's directory. They use the same Worker manifest and input/output schemas
as shared packages. Capture rejects symlinks, namespace shadowing and excessive closure
size. A local file cannot overwrite the committed policy or change mandatory checks.

Qualified slot names come from `af task explain`. An embedded slot mapped to a parent uses
the parent's physical slot name. Unknown names, forbidden replacements, changed payload
schemas, weaker evidence/retention or wider effects fail before model admission. Attempt
limits and verification reserves remain owned by the Pipeline and Task. Verification
Workers must remain independent of source-writing Workers under the captured project policy,
even if the Pipeline omits an independence annotation.

The plan includes the effective package and every default package whose contract constrained
its replacement. Resume recompiles those captured bytes and settings; it does not read the
local file again. The integration fixture runs Alice and Bob against the same Pipeline,
changes their local files after planning, and verifies both captured executions.
