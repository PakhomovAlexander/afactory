# ADR-0242: Classify first-run refusals and suggest only explicit trusted review commands

**Status:** accepted (2026-10-27).

## Context

Common first-run failures reached the CLI as prose. Agents had to parse a sentence to find a
Task, authority file, Provider or selector mistake. The error document had five causes, and a
review plan could suggest a binding even when more than one Provider of a kind was registered.
ADR-0041 and ADR-0042 require explicit selectors and bindings.

## Decision

`af/error@2` adds stable causes for missing Tasks, catalogs and authority, non-repositories,
missing Providers, unknown review nodes, plan mismatch, dirty sources, and missing review
selectors or bindings. Each classified refusal carries structured `details`; `next_action` is
present only when one command can be given. `af help errors` and `af help errors CAUSE` render
the same cause and explanation table used by the document. Version 1 is neither written nor
accepted by this release.

Review suggestions remain explicit commands. The CLI reads local Git metadata without fetching:
`refs/remotes/origin/HEAD`, then `init.defaultBranch`, then a local `main` or `master`. On the
default branch with an uncommitted change, it suggests
`--policy-rev HEAD --base HEAD --uncommitted`. Elsewhere it resolves the default branch's
commit for both policy and Base and
uses `--candidate HEAD`. When the branch cannot be resolved, the caller must choose trusted
policy authority; there is no command to copy. The candidate's own policy is never read to make
that choice. A missing Provider binding is suggested only when exactly one enabled registry
entry has the Worker's kind. No suggestion supplies an implicit binding: the command spells
`--provider NODE=ID` and all selectors, and the ordinary validation still runs.

## Considered options

- Add named Subject presets. Rejected by the owner: they add another selector vocabulary while
  the exact flags already express the trusted revisions.
- Put Provider bindings in policy. Rejected by the owner: a repository cannot choose the
  operator's machine-local Provider identity.
- Infer policy from the candidate. Rejected: candidate code would select its own authority.
- Auto-bind the only Provider. Rejected: it would bypass the explicit authority captured by the
  review invocation.
- Suggest the first of several Providers. Rejected: registry order is not user intent.
- Keep prose-only errors. Rejected: agents cannot branch on stable failure causes.

## Consequences

A first-run agent can inspect a cause and execute an unambiguous `next_action`. A human sees the
same command at the end of the text refusal. Unknown or ambiguous authority and Providers ask
for a choice without fabricating one. Plan and run still require explicit trusted input.
