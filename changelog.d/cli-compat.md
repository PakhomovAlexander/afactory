- af now checks, token-free, that the installed `claude` or `codex` accepts its invocation before
  `af provider status`, `af provider setup` or Task Provider admission asks about a login
  ([ADR-0146](docs/adr/0146-refuse-a-provider-cli-that-does-not-accept-afs-invocation.md)). It
  reads `claude --help` or `codex exec --help` and refuses a CLI that does not declare every
  option af passes, such as Claude Code before 2.1.248, which has no `--restricted`. No isolation
  flag is ever dropped to make a CLI run. The report names the Provider, the path, the version,
  every missing option and the upgrade command. `af provider status` shows the new state
  `incompatible` and exits 4, and `--json` now emits `af/provider-status@2`. Task admission
  refuses before any Worker runs or any token is charged.
- The Codex app-server is started without `--stdio`, so codex-cli 0.121.0 works again. For Codex
  the check also starts the bare `codex app-server` and asks it to `initialize`; only an answer
  whose `result` is a JSON object proves it. An app-server that exits first, rejects the request,
  answers `{"id":1}`, or writes a line that is neither a response (an `id` and exactly one of
  `result` or `error`) nor a notification or request is `incompatible` at once on plain status,
  `--usage`, setup and admission alike, reported with its exit status (or `was stopped by af`) and
  the line it answered with. The identity and usage probes judge every line by the same grammar
  and accept only an answer read after af wrote the request, so a response written early never
  binds an account or shows limits.
- A Worker that exits without a parsable result (for Claude, anything but an object with a
  boolean `is_error` and its `result`; for Codex, anything but the `-o` final message after a
  clean exit) now reports its exit status and a bounded stderr excerpt whatever its failure is
  classified as, and one whose CLI refused af's arguments is classified `unsupported_invocation`
  whatever its exit status, even beside an authentication hint or after unrelated Codex events.
- One shared filter replaces any Provider-authored line that looks like credential material
  (`API_KEY=…`, `Bearer …`, `Authorization:`, `sk-…`, 32 or more hex characters, 32 or more
  base64 characters mixing cases and digits, a JSON Web Token of three whole base64url segments)
  with `[line withheld: looks like credential material]`, the Codex rate-limit names shown by
  `status --usage` included.
