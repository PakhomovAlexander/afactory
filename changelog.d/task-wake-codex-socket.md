Allow owner-owned Codex wake sockets with mode `0777` inside a canonical owner-private parent
directory, matching the demonstrated Codex 0.167.2 daemon layout. Preserve pinned socket identity,
peer authentication and all route authority checks; extend deterministic compatibility and
negative authority coverage. See the proposed
[decision](../docs/adr/2026-11-13-admit-codex-sockets-under-private-parents.md).
Final live host certification remains pending.
