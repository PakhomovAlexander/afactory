# ADR-0145: Run the markdownlint gate from a pinned offline closure

**Status:** accepted (2026-10-10).

## Context

This repository's code policy runs its required `markdownlint` check as
`npx --yes markdownlint-cli2@0.22.1 **/*.md`. A Task check gets a private, empty `HOME`, so every
gate makes npx resolve and download the package and its dependencies from registry.npmjs.org
again. Issue #230 lists the spans of that check in 17 Task gates on 2026-10-07 and 2026-10-08:
from 5.8 s to 449.7 s. On 2026-10-09 Task tp206v-2 lost a passed kernel check because npx failed
after 160 s with `getaddrinfo ENOTFOUND registry.npmjs.org`. The tool's version was pinned; its
transitive dependencies, the registry and the network were not.

A diagnostic run (issue #230 M0) compared a cold private-`HOME` npx run, 6.5 s for 229 files, with
the same CLI preinstalled and run directly with no network, about 1.8 s. Those were not paired
measurements of a candidate against its base, and this record claims no speedup from them; they
show that the download is separable from the lint.

The Warm Check Cache (ADR-0131) has Cargo kinds only, and the sandbox passes a host-local check
the inherited `PATH`, a private `HOME`, `TMPDIR` and `XDG_CACHE_HOME`, and nothing else.

## Considered options

1. **An npm kind in `[warm]`.** Rejected: it would add a generic cache API to the engine for one
   tool, an npm cache is a mutable store npx still checks against the registry, and the gate would
   still depend on npm's resolution at run time.
2. **Commit the installed tree.** Rejected: 1,526 files and 15 MB of third-party code in the
   repository, for one lint.
3. **`npm ci` from a committed lock into a cached directory.** Rejected: npm reads the operator's
   `.npmrc`, registry and proxy settings, keeps its own cache under `HOME`, runs lifecycle scripts
   unless told not to, and writes files that vary with the npm version, so the installed tree
   could not be pinned by one digest.
4. **Mount a tool directory into the check sandbox.** Rejected: a new sandbox boundary and engine
   change for what a file on the inherited `PATH` already reaches.
5. **A committed lock and tree digest, an explicit installer that is the only networked step, and
   a gate entry point that verifies the installed tree on every run and fails closed.** Chosen.

## Decision

1. **The pin is in the repository.** `scripts/markdownlint/package.json` depends on exactly
   `markdownlint-cli2` `0.22.1`; `scripts/markdownlint/package-lock.json` is its whole transitive
   closure (lockfileVersion 3, 86 packages, `markdownlint` 0.40.0), each package an exact version
   with its registry tarball URL and sha512 integrity; `scripts/markdownlint/tree.sha256` is the
   digest of the tree an install of that lock produces. The lock is refused unless every entry is
   reachable from the root, every dependency resolves by Node's lookup to an entry, every URL is
   `https://registry.npmjs.org/<name>/-/<file>-<version>.tgz` for the entry's own name and version,
   and no entry carries a field the installer does not interpret (`link`, `hasInstallScript`,
   `optional`, `os`, `cpu`, bundles).
2. **Installing is explicit and is the only step that uses the network.**
   `python3 scripts/markdownlint-tool.py install [--prefix DIR] [--from DIR]` (`make
   markdownlint-tool`) runs no npm and reads no npm configuration or cache. It fetches each tarball
   the lock names from registry.npmjs.org (30 s per request, 16 MiB per tarball, 64 MiB in all),
   or reads it from a local mirror in the registry's layout with `--from`, and refuses it unless it
   matches the lock's sha512 and holds the package and version the lock names. It extracts regular
   files and directories only, refusing links, special files, absolute or parent-relative paths
   and duplicate entries, and runs nothing it extracted. It gives every directory mode 0555 and
   every file 0444, adds a launcher `bin/markdownlint-cli2` (0555) and an `af-tool.json` manifest,
   refuses the tree unless its digest is the committed one, and renames it into place as
   `<prefix>/markdownlint-cli2-0.22.1-<first 16 hex digits of the tree digest>`. The default
   prefix is `$XDG_DATA_HOME/af-tools`, outside the `$XDG_DATA_HOME/af` that af's Storage Budget
   manages (ADR-0144). Installs racing for one prefix leave one tree and no staging directory. An
   existing tree that verifies is kept without fetching; one that does not is refused unless
   `--replace` is given. Installing is an operator's act before a gate, never part of one.
3. **The gate entry point verifies and then becomes markdownlint-cli2.**
   `python3 scripts/markdownlint-tool.py run ARGS...` finds the tree through a `<root>/bin` entry
   on the inherited `PATH`; a tree of another pin is skipped, and the first tree of this pin is
   used or refused, never passed over for a later one. It refuses the tree unless the tree and
   every directory above it belong to this user or root and no one else can write them (a sticky
   directory such as `/tmp` excepted), every entry below it is a directory or a single-link regular
   file owned by this user with exactly the installed mode, the manifest names this lock and tree,
   and the tree digest is the committed one. It then replaces itself with
   `node <root>/node_modules/markdownlint-cli2/markdownlint-cli2-bin.mjs ARGS...` in the same
   working directory, with the inherited environment less `NODE_*` and `npm_*` variables. The
   repository's `.markdownlint-cli2.jsonc`, the `**/*.md` glob and the exit status (0 clean, 1
   lint errors, 2 a failure of markdownlint-cli2 itself) are therefore markdownlint-cli2's own. A
   missing, stale or refused tool, or a missing `node`, exits 2 before markdownlint-cli2 starts;
   there is no fallback to npx or the network. The run reads nothing from `HOME` and writes
   nothing.
4. **The policy change is separate.** The check this enables is `program = "python3"` with the
   literal arguments `scripts/markdownlint-tool.py`, `run` and `**/*.md`, and the same name and
   `required = true`. This change does not edit `.af/code-policy.toml`: Task Workers do not write
   `.af/`, and the authority's existing npx check keeps running until a reviewed change switches
   it, after each gate machine has run the install and put the printed `bin` directory on the
   `PATH` af runs with. The candidate entry point needs its own offline verification on such a
   machine; the existing check does not exercise it.
5. **Tests.** `make check`'s `preflight-check` runs `scripts/test-markdownlint-tool.py` offline
   against a synthetic two-package closure and a stand-in `node`: arguments, working directory and
   exit status passed through; a missing tool or `node` refused without a fallback; a changed,
   missing, writable, linked or hard-linked file, a manifest of another pin and a prefix others can
   write refused; other pins skipped; parallel runs and racing installs; tarballs that differ from
   the lock, hold another package, or carry links or traversal refused; and locks that are not an
   exact registry closure refused. The same file checks that the committed pin is consistent.
   `make markdownlint-tool-e2e` (`AF_MARKDOWNLINT_E2E=1`) installs the real closure and lints
   fixtures through `run` under a private `HOME`: valid Markdown exits 0, an MD025 violation in a
   nested file exits 1, the repository configuration's disabled rules and ignores hold, a
   `.markdown` file is not selected, parallel runs keep their results, a changed copy is refused,
   and, given `sudo -n` with `unshare` and `setpriv` or a command prefix in
   `AF_MARKDOWNLINT_OFFLINE`, the same lint passes and fails inside a network namespace as this
   user.

## Consequences

- Each machine that runs gates installs the tool once and adds one `PATH` entry. A new lock is a
  new tree name, so it needs a new install and `PATH` entry; until then the gate refuses rather
  than run an older closure.
- `node` is the host's, as it is for npx today; markdownlint-cli2 0.22.1 requires Node 20 or
  newer. The closure is pinned; the interpreter is not.
- Every run reads and hashes the whole tree (1,526 files, 15 MB). That takes about 0.4 s on the
  machine that produced this change, unmeasured elsewhere; the gate's own timing is for the paired
  measurement that issue #230's acceptance still needs.
- Verification binds the bytes the gate runs to the repository. It does not defend against a
  process of the same user that rewrites the tree between verification and Node's reads; such a
  process can already change anything the user runs.
- A check that runs in a container does not see the host's `PATH`, and therefore not this tool;
  this repository's code policy does not require a container.
- To bump the version, regenerate the lock with `npm install --package-lock-only --ignore-scripts`
  under an empty npm configuration and cache, then run `install` once: its refusal names the tree
  digest to commit in `tree.sha256`.
