- Security audit package D (process capture, containers, Worker environments, the remote gate).
  Each captured stdout and stderr of a check, a Worker or a container command is kept to 64 MiB
  (`review_process::MAX_CAPTURED_STREAM_BYTES`); the rest is read and discarded so the child never
  stalls, the total is recorded, and a check whose output passed the cap fails with a reason
  naming it, as does a Worker Attempt. On Linux af is a child subreaper, and a shell Attempt or a
  local check ends every descendant that left its process group (`setsid`, or a `set -m` job in
  another group of the same session) but kept the `AF_PROCESS_TREE` value it inherited from its
  leader, before its tree is sealed: that value alone, never how many runs are live, tells it
  from any concurrent run's, so a run never ends a process another live run started; when the
  subreaper cannot be set, af warns once on stderr and every check (`reason`) and Attempt
  (stored stderr) records that descendant cleanup fell back to the process-group kill. A
  descendant that clears or replaces that variable and leaves its process group can still
  outlive the run under `trusted_local`, on Linux as on macOS, where any such descendant can;
  `trusted_local` is no security boundary, and untrusted code belongs in a container
  ([ADR-0118](docs/adr/0118-let-review-workers-execute-checks-in-an-ephemeral-clone.md),
  [ADR-0120](docs/adr/0120-give-a-source-writing-worker-a-shell.md), amended). Container checks
  run with `--pids-limit 4096`, `--cap-drop=ALL`, `--security-opt=no-new-privileges` and
  `--init`, and with `--memory`/`--memory-swap` and `--cpus` when the `[gate]` binding sets the
  new optional `memory` and `cpus` keys. Attempts and local checks get a `PATH` without empty or
  relative entries; a Codex Attempt's `HOME` is a private empty directory, not the candidate
  tree; a Claude Worker's `CLAUDE_CONFIG_DIR` is redacted from stored output like `CODEX_HOME`,
  in its canonical spelling too. The Remote Check gate streams each blob into `git fast-import`,
  runs `git` and `gh` from an allowlisted environment (no `GIT_CONFIG_COUNT`, `GH_HOST` or
  `GH_TOKEN`), calls `gh api --hostname github.com`, redacts any URL's user information, also
  refuses a candidate that changes `.github` itself, `.gitmodules`, `.cargo/` or a root
  `action.yml`/`action.yaml`, and cleans up through the mapping entry of the Task's
  `repository_id`; the docs now say the workflow *file* is protected and everything it runs is
  candidate code
  ([ADR-0140](docs/adr/0140-run-a-declared-check-through-a-gate-pull-request.md), amended). Two
  edits outside this package were unavoidable: the `[gate]` keys in `review-config` (with the
  struct literals in `af/src/onboard.rs` and the Integration tests, the two provider sites in
  `review-pipeline/src/review_domain` and the container Gate argv test beside them), and the
  cleanup lookup in `af/src/storage/gate.rs`.
