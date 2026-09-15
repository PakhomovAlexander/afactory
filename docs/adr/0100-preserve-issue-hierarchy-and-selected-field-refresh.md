# ADR-0100: Preserve issue hierarchy and selected-field refresh semantics

**Status:** proposed

## Context

Issue requirements must retain their meaning and exact selected-field identity under
[ADR-0061](0061-capture-read-only-issue-sources-outside-execution-authority.md). Explicit refresh
must invalidate an affected plan without changing its original authority or resources under
[ADR-0062](0062-refresh-issue-revisions-without-resetting-execution-authority.md). Three input
handling defects violated those rules: nested ADF list lines lost indentation, an explicit
relative source filename used the repository instead of the caller's directory, and Jira's
unselected update timestamp invalidated a plan even when all selected fields were unchanged.

## Decision

Indent every continuation line of an ADF list item by that item's rendered marker width.
Nested lists and subsequent blocks remain inside their parent item, including ordered markers
whose width changes. Retain existing output, depth and node bounds. Original field values and
normalized text remain separately captured; replay does not renormalize earlier captures.

Resolve an explicit `--source-file` relative to the process working directory. An omitted
override continues to select the original project-relative issue path. Resolve the explicit
path lexically before the existing regular-file, symlink and bounded held-file checks;
canonicalization must not erase a leaf symlink before those checks. This grants no write access
and does not recapture the live repository as Task source or policy.

A Jira refresh is unchanged when adapter, issue ID/key, locator and the complete selected
field value/text identities match. Ignore `updated` for that comparison because Jira changes
it for unrelated edits. The no-op keeps the original observation, Task revision, plan, approval,
outputs and accounting. Selected-field changes still invalidate the affected plan even when
the timestamp is unchanged. Explicit local issue revision labels retain their existing meaning.

No artifact or event schema changes. Existing captures, decisions and completed Task history
remain immutable; resource and currentness checks are unchanged.

## Alternatives and verification

Refusing all nested lists would discard supported requirements instead of preserving their
structure. Resolving every path against the repository would keep an explicit CLI filename
ambiguous. Ignoring source revision labels for all adapters would change local issue semantics.
These broader alternatives are unnecessary.

Source fixtures distinguish nested and flat lists through their actual Jira captures and text
IDs, including multi-block items and ordered marker widths. A real CLI fixture selects an
external relative file from the caller's directory despite a conflicting repository filename,
while retaining the source Snapshot and limits. Recorded Jira captures exercise timestamp-only
and unselected-field changes, changes to every selected field under the same timestamp, identity
and locator substitutions, and the unchanged local revision rule. Existing refresh tests cover
approval preservation for no-op observations and invalidation for changed requirements. No live
Jira or model account is required.
