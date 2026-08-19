# Case: hostile Git configuration during capture

**Phase 2.** Discharges: *"Git hooks, filters, external helpers, and implicit submodules cannot
execute during offline capture/materialization"* and *"Committed and uncommitted Review Targets
are immutable and reproducible by digest."*

## Why there is nothing to capture

`bundle.sh` runs `git diff` in the user's checkout with the user's configuration. Filters and
hooks fire as they normally would — that is not a bug in the harness, it simply has no
capture boundary. The kernel does, and its snapshot must be a function of repository content
alone.

## Setup

A repository whose configuration is weaponized in the ways a candidate branch can influence:

- a `clean`/`smudge` filter and a `diff` textconv that execute an arbitrary command
- `core.hooksPath` pointing at hooks that run during the commands capture uses
- `.gitattributes` (in the *candidate* tree) enabling those filters for the changed paths
- a submodule entry pointing at a local path, plus one at a network URL
- `core.fsmonitor` set to an external program
- a `.git/config` `alias` shadowing a plumbing command the adapter might call
- a symlink and a path differing only by case, to catch materialization writing outside the root

## Required behavior

- Capture and materialization run with filters, hooks, external helpers, fsmonitor and aliases
  **disabled**, using sanitized plumbing (`ls-tree`, `cat-file`, `hash-object --no-filters`,
  diff with external helpers off). No planted command executes — proven by a marker file that
  remains absent and untouched.
- No implicit submodule fetch occurs; the network URL is never contacted during capture.
- The resulting `SourceSnapshot` digest is **identical** to one captured from the same content
  with a clean configuration. Configuration cannot change content identity.
- The uncommitted (dirty-tree) capture detects concurrent index/path/content change or monitor
  overflow and either retries or fails — it never admits a torn synthetic tree.
- Materialization refuses to write outside the sandbox root; the symlink and case-colliding
  path are handled without escaping it.
- The source checkout is **unmodified** afterward: same HEAD, same index, same working tree,
  same untracked set.

## What failure looks like

Two reviewers inspect "the same snapshot" and see different bytes, or the capture step executes
candidate-controlled code with the operator's privileges before any sandbox exists. Both are
invisible in the resulting review — the findings look ordinary.
