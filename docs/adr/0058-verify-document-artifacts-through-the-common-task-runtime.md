# ADR-0058: Verify document artifacts through the common Task runtime

**Status:** accepted

## Context

P13 requires a non-code workflow on the same Task lifecycle. Manufacturing a source Snapshot or
an irrelevant code check would make a document appear compatible without establishing document
acceptance. A data-only author can also omit filesystem effects, so source-writing effects alone
cannot identify every author that must remain independent of a verifier.

## Decision

Install a document domain with its own captured policy, required outputs and acceptance rules.
Code and document policy references are optional individually; a Task captures exactly the policy
for its installed kind profile. Existing code-policy serialization remains unchanged when present.
Document Workers use a fresh data-only environment that enforces the captured isolation policy
and refuses filesystem changes. Scheduling, Provider admission, reservations, accounting, retries,
approval, execution history and finalization remain in the existing Task runtime and Store.

Capture bounded document sources and requirements as typed data inputs. Authors return structured
plain-text drafts and named citations. An installed pure renderer creates Markdown tied to exact
draft and source artifacts. An installed check operation uses a protected Attempt to enforce size,
required sections, citation membership and supported source locations. Link checks are explicit
syntax/captured-reference checks; rendering does not fetch remote URLs or promote source text to
execution authority.

An independent Verify Worker runs only after current checks pass. Its evaluation identifies the
Document, sources, requirements and check receipt. Acceptance revalidates that evidence and binds
it to the actual public Document output and Task inputs. Missing evidence is inconclusive; negative
evidence remains negative. Compiler `same_as` lineage carries document identity across typed ports
without inventing Snapshot IDs. Runtime validation additionally checks exact artifact identities.

The trusted compiler accepts installed authored-artifact types. The document profile registers
DocumentDraft: any Worker producing that type is an author for mandatory verifier independence,
regardless of its role/effect declarations. Existing source-writer independence remains intact.

Provide a token-free starter factory from supported typed definitions and schemas. Calculate all
pins from emitted bytes, keep local Worker schema resolution closed, and require the developer to
review and commit the resulting authority before using it. The supplied command substitutes have
a bounded, explicit tutorial goal and do not claim general natural-language verification.

Recorded results can be saved to absent files without changing Task acceptance or history.
Markdown output requires a typed Document; generic JSON output retains the artifact envelope.

## Consequences

Document Tasks need no code policy or source-tree artifacts. The CLI fixture executes three actual
Attempts, preserves two verifier reservations, uses captured inputs across restart, and saves the
recorded Markdown without overwriting an existing file. Failed checks suppress evaluator spend;
stale, missing and negative evaluations retain their appropriate domain outcomes on replay.
Model/source substitutions and Jira revision refresh remain separate increment evidence.
