- Security audit fixes (redaction, provider login, telemetry): the shared credential heuristic now
  also withholds a URL carrying a password or an `http(s)` user, `password <value>` after whitespace
  (`--password x`, `.netrc` lines), `mysql -u … -p…`, AWS access key ids, device codes beside
  `code`, `/login/device`, one-time codes and `#code=`, `Bearer: …`, JSON Web Tokens with a `+` or a
  short signature, hex runs of 20 or more mixing letters and digits, lower-case runs of 40 or more,
  the account a login names, and two long words that are one secret split by a space; a `sha256:`
  digest and a 40- or 64-hex git object id are no longer withheld. JSON values are judged whole
  whatever their member's name (`pw` now counts as a secret name), and a granted path is replaced in
  its home-relative, canonical, JSON-escaped, percent-encoded and non-normalised spellings; blobs
  record `af-redact/2`. Provider login processes disable core dumps (non-dumpable on Linux) and zero
  the challenge and the returned code when dropped (new dependency: `zeroize`). The terminal-only
  login is documented as protection against accidental capture, with a pseudo-terminal caller out of
  scope and a test pinning it ([ADR-0112](docs/adr/0112-refuse-agent-mediated-provider-logins.md),
  amended). At telemetry level `metrics` the segment header keeps only `argv[0]` and the subcommand
  path and no working directory; `-p` and `--pw` values are withheld at `spans`. `sink = "otlp"`,
  `sink_endpoint` and `sink_headers_file` are refused at load as not implemented; `[telemetry]
  max_bytes` is documented as advisory and `af telemetry status` warns when over it; its `--json`
  paths (and `reindex --json`'s) are `~`-prefixed like the text output; no help or status text names
  an overhead gate; `docs/security/containment-probes.md` records the green `container-probes` job
  and closes live Gate routing. Fixtures outside this package's files changed only where they pinned
  a 40-hex run or a bare digest as a secret (`review-runner` CLI tests, the Claude and Codex
  auth-failure tests, one `record.rs` test), plus the `af telemetry` text output, the `af telemetry
  status` help in `cli.rs` and the `nix` features in `crates/af/Cargo.toml`.
