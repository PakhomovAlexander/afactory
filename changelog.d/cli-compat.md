- af now checks, token-free, that the installed `claude` or `codex` accepts its invocation before
  `af provider status`, `af provider setup` or Task Provider admission asks about a login
  ([ADR-0146](docs/adr/0146-refuse-a-provider-cli-that-does-not-accept-afs-invocation.md)). It
  reads `claude --help` or `codex exec --help` and refuses a CLI that does not declare every
  option af passes, such as Claude Code before 2.1.248, which has no `--restricted`. For Codex it
  also starts the bare `codex app-server` and asks it to `initialize`; only an answer whose
  `result` is a JSON object proves it, so an app-server that exits first, rejects it, answers
  `{"id":1}` or first writes a line that is neither a response nor a notification (`{"foo":"bar"}`,
  refused at once rather than waited on) is `incompatible` on plain status, `--usage`, setup and admission alike. No isolation flag
  is ever dropped to make a CLI run. The report names the Provider, the path, the version, every
  missing option and the upgrade command. `af provider status` shows the new state `incompatible`
  and exits 4, and `--json` now emits `af/provider-status@2`. Task admission refuses before any
  Worker runs or any token is charged. The Codex app-server is now started without `--stdio`, so
  codex-cli 0.121.0 works again. An app-server that exits early is reported with its exit status
  and its own first error line, and one that rejects or malforms its `initialize` answer with its
  exit status (or `was stopped by af`) and the line it answered with. A Worker that exits without a parsable result (for Claude, anything but
  an object with a boolean `is_error` and its `result`, `{}` included; for Codex, anything but the
  `-o` final message after a clean exit) now reports its exit status and a bounded stderr excerpt
  whatever its failure is classified as, and one whose CLI refused
  af's arguments is classified `unsupported_invocation`, even beside an authentication hint or
  after unrelated Codex events. One shared filter replaces any Provider
  line, the Codex rate-limit names `status --usage` shows among them, that looks like credential material (`API_KEY=…`, `Bearer …`, `Authorization:`, `sk-…`,
  32 or more hex characters, 32 or more base64 characters mixing cases and digits, a JWT of three
  whole base64url segments whose header and signature are 16 or more characters long) with `[line withheld: looks like credential material]`.
