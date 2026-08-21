# Compute the Change Set with `git diff`, reachable only through a typed method

**Status:** accepted (2026-08-20)

Supporting a `diff` Subject means the kernel must produce a Change Set — the changed path set
and the patch between the Base Snapshot and the head Snapshot. Capture's git adapter refuses
`diff` outright today, and for a reason the test suite proved rather than assumed:
`hostile_git_config.rs` found that `git status` hashes worktree files, and hashing runs the
`clean` filter the candidate's own `.gitattributes` selected — so a read-only *check* was
executing attacker-chosen code with the operator's privileges, before any sandbox existed.
`diff` was refused alongside `status` because the worktree-vs-index forms do the same thing.

We decided to let git compute the diff, but to make the unsafe forms unrepresentable rather
than merely disallowed. `SAFE_SUBCOMMANDS` stays closed. A new `Repo::tree_diff(base, head)`
builds the entire argv itself from typed tree identifiers and calls a private unchecked runner.
The runner uses a kernel-owned bare administrative directory and attaches only the resolved
object database. It has no worktree, index, or candidate attribute/configuration source.
Tree-to-tree diff therefore reads neither the candidate worktree nor its local Git configuration;
and because no caller can supply a flag, pathspec, or operand that is not a resolved tree id, no
caller can reach a worktree form either.

## Considered options

- **Compute the diff ourselves.** Changed paths are a set-difference of two capture manifests,
  which the kernel already builds; hunks would come from `cat-file` (already allowlisted) plus
  a unified diff in Rust. Adds no attack surface at all and makes the Change Set a pure
  function of two content digests. Rejected for cost: it means owning a diff implementation —
  a ninth direct dependency or a few hundred lines of Myers — plus rename detection, binary
  detection, and mode changes, all of which git already does correctly.
- **Path list only, no hunks.** Free, and needs neither a diff implementation nor a git call.
  Rejected because both shipped reviewer prompts ask comparative questions — performance is
  told to find "code the change makes hotter", architecture to find "public interfaces changed
  without their consumers" — and neither is answerable without the prior text.
- **Add `diff` to `SAFE_SUBCOMMANDS`.** Smallest edit. Rejected because the allowlist is
  checked against `args[0]` alone, while `bytes`/`text`/`line` are public and take arbitrary
  argv: admitting the subcommand admits *every* form of it workspace-wide, including bare
  `git diff`, which is the worktree-vs-index form that runs the clean filter. The allowlist's
  own doc comment says it is enforced centrally "so a future edit cannot reintroduce the hole
  by reaching for the obvious command" — this would have been that edit.
- **Add `diff` to the allowlist, plus an argv validator in `run_raw`.** Keeps one central gate
  with no private bypass, which is genuinely easier to audit. Rejected because the validator
  becomes the load-bearing part and must stay exhaustive against git's flag surface, including
  unambiguous prefixes: git accepts `--ext-d` for `--ext-diff`, so the validator must allowlist
  flags rather than deny them, and must keep doing so as git evolves.
- **A typed `Repo::tree_diff` with the allowlist untouched (chosen).** The safe invocation is
  the only one that can be constructed, which is the same move the Check layer already makes
  with literal-versus-untrusted argument slots. The bypass is one private function with one
  call site, which a reviewer can check in a single read.

## Consequences

- `SAFE_SUBCOMMANDS` keeps its current five entries, and `repo.bytes(&["diff", …])` keeps
  failing with `UnsafeSubcommand` from everywhere in the workspace.
- A private unchecked runner now exists in the git adapter. It is the one thing a future editor
  could misuse, so it must stay private, keep its single call site, and carry a comment saying
  why it exists. A test should assert the generic path still refuses `diff`.
- The tree identifiers `tree_diff` accepts must be constructible only from resolved revisions,
  never from a caller-supplied string — otherwise the operand position becomes an injection
  point and the typed method buys nothing.
- The typed result carries the exact `git --version` text and uses a fixed rename-candidate
  limit. M2.4 records the Git version and diff policy with the Change Set so upstream behavior
  and bounded rename work are visible rather than ambient.
- `hostile_git_config.rs` gains a case: a candidate-controlled `.gitattributes` selecting a
  `textconv` driver must not execute during a Change Set computation, and the resulting patch
  must equal a clean repository's for the same content.
- We accept that git, not the kernel, decides what the patch looks like — rename detection
  thresholds and binary handling become upstream behavior the Change Set inherits. The Change
  Set is therefore reproducible only against a given git version, which is weaker than the
  content-digest reproducibility the rest of capture provides. Record the git version in the
  Change Set artifact so a mismatch is visible rather than silent.
