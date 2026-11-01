# Confirming quarantined tests on Claw

How the owner settles the entries of `.config/test-quarantine.json`
([ADR-0268](adr/0268-quarantine-flaky-tests-on-gate-evidence-and-confirm-them-on-claw.md),
[`test-classes.md`](test-classes.md), "Quarantine"): `scripts/test-quarantine.py confirm` runs
the quarantined tests 50 times under load on the Claw VM and writes a report, and
`scripts/test-quarantine.py apply` turns that report into the file change and the issue bodies on
the owner's machine. Nothing here runs in a gate or in CI.

## What `confirm` does

```sh
python3 scripts/test-quarantine.py confirm --report FILE [--runs N] [--only IDENTITY ...] \
    [--test-threads N] [--load functional|none] [--cargo PATH] [--inventory FILE]
```

- It confirms every entry of the file, or only the `--only` identities (repeatable).
- Each of the N runs (`--runs`, default 50) is one whole
  `cargo nextest run --workspace --locked --profile ci --no-fail-fast --retries 0` of an exact
  filter that selects just those identities, at `--test-threads` (default: the CPU count), with
  its own JUnit, through `scripts/nextest-gate.py`'s run. Every run runs: a failure stops
  nothing and is never rerun alone.
- With `--load functional`, the default, the functional class's own selection (the filter
  `scripts/test-classes.py` generates for the gate, which leaves every quarantined identity out)
  runs in a loop beside the runs until they end. Its own failures never count. `--load none`
  turns it off. The default inventory is
  `cargo nextest list --workspace --locked --message-format json`, which builds every
  default-feature test binary before the first run.
- Each identity is `flaky` when it failed in some runs but not all, `always_fails` when it failed
  in every run, and `clean` when it never failed. A run that ends without a JUnit, or whose JUnit
  has no result for an identity, stops `confirm`: it names the run and writes no report,
  because a missing result is not a pass.
- `--report FILE` must not exist; it is never overwritten. The report is one
  `af.test-quarantine-confirm/1` document (`schemas/test-quarantine-confirm-v1.json`). It holds
  the git commit, today, the host (`platform.system()`, machine, CPU count, thread count), the
  load and the number of load runs, N, and per identity its failure count, the runs that failed,
  its classification and the first failure's JUnit output, bounded at 64 KiB.

One run takes about as long as the slowest selected test and nextest's start, so 50 runs take
the selected tests' time × 50, with the load running beside them the whole time. The first
build of the test binaries comes before that.

## Run it on Claw

Claw is `ssh claw-3`: Linux x86_64, 12 vCPU, Docker 29, and no Rust toolchain on the host, so
the run happens in a container. The image is built once on Claw from the Rust image of
`rust-toolchain.toml`'s version, `rust:1.88.0`. It adds Python, git and the prebuilt
cargo-nextest 0.9.132 release binary: `cargo install cargo-nextest` does not work there, because
current cargo-nextest releases require a newer rustc than 1.88.0. It also adds the unprivileged
user `tester` (uid 1000), and the clone and the confirmation run as that user, never as root:
tests run as root behave differently, for example root ignores the read-only fixtures. Build it
once, tagged `af-quarantine-confirm:1.88.0`:

```sh
ssh claw-3 bash -s <<'CLAW'
set -e
docker build -t af-quarantine-confirm:1.88.0 - <<'DOCKERFILE'
FROM rust:1.88.0
RUN apt-get update && apt-get install -y --no-install-recommends python3 git curl ca-certificates && rm -rf /var/lib/apt/lists/*
RUN curl -LsSf https://get.nexte.st/0.9.132/linux | tar zxf - -C /usr/local/cargo/bin && cargo-nextest nextest --version
RUN useradd -m -u 1000 tester
USER tester
DOCKERFILE
CLAW
```

The source is a fresh clone of the commit to confirm, mounted in, so the report's commit is that
commit. On the owner's machine, pick the commit (the `main` commit whose
`.config/test-quarantine.json` holds the entries):

```sh
COMMIT=$(git rev-parse origin/main)
```

Then start the confirmation on Claw. The commit is passed to the remote shell, which reads the
commands from the quoted here-document, so nothing in it is expanded on the owner's machine. The
mounted directory `$WORK/run` must be owned by uid 1000 (use `sudo chown` when the ssh user is
not uid 1000), and the host's `git` needs `-c safe.directory='*'` to read a clone another uid
owns:

```sh
ssh claw-3 COMMIT="$COMMIT" bash -s <<'CLAW'
set -e
WORK="$HOME/af-quarantine-$COMMIT"
mkdir -p "$WORK/run" && chown 1000:1000 "$WORK/run"
docker run --rm -v "$WORK/run":/work -w /work af-quarantine-confirm:1.88.0 sh -c "
  git clone https://github.com/PakhomovAlexander/afactory.git src &&
  git -C src checkout --detach $COMMIT"
test "$(git -c safe.directory='*' -C "$WORK/run/src" rev-parse HEAD)" = "$COMMIT"
nohup flock -n /tmp/af-perf-machine.lock docker run --rm --name afq-confirm \
    -v "$WORK/run":/work -w /work/src af-quarantine-confirm:1.88.0 \
    python3 scripts/test-quarantine.py confirm --report /work/confirm.json \
    > "$WORK/confirm.log" 2>&1 &
CLAW
```

Claw also hosts the performance laboratory, whose experiments take a non-blocking exclusive
`flock` on `/tmp/af-perf-machine.lock` (`scripts/performance/perflab/hostlock.py`,
[`performance-local-executor.md`](performance-local-executor.md)). The confirmation holds that
lock for its whole run, so an experiment started meanwhile refuses to start rather than measure
under load, and the confirmation does not start while an experiment holds the lock: `flock -n`
then exits 1 at once, no `afq-confirm` container appears and `confirm.log` stays empty; start it
again later. `nohup` and the background job detach the confirmation, so an ssh disconnect does
not kill it. Wait until `docker ps --filter name=afq-confirm` no longer lists the container,
then read the end of the log:

```sh
ssh claw-3 'docker ps --filter name=afq-confirm'
ssh claw-3 "tail -n 20 af-quarantine-$COMMIT/confirm.log"
```

Back on the owner's machine, copy the report and the log:

```sh
scp "claw-3:af-quarantine-$COMMIT/run/confirm.json" "$TMPDIR/confirm.json"
scp "claw-3:af-quarantine-$COMMIT/confirm.log" "$TMPDIR/confirm.log"
```

`confirm` prints each identity's classification and `confirm: wrote /work/confirm.json`, which
is `run/confirm.json` on Claw. It exits 1, with no report, when the file fails a guard, a run
lacks a result, or the report file exists.

The first run on Claw (12 vCPU, 2026-10-30, `main` 10b6db0f) built the test binaries in 1 min
35 s. Its 50 runs of nine tests, with the functional class looping beside them for 6 load runs,
then finished within minutes, because the slowest of the nine took 6.6 s. All nine were clean on
Linux, so ADR-0268's platform rule kept every entry with macOS evidence `quarantined`.

## Apply the report

On the owner's machine, in a checkout of `main` at or after the confirmed commit:

```sh
python3 scripts/test-quarantine.py apply "$TMPDIR/confirm.json"     # print what it would do
git switch -c test-quarantine-confirm
python3 scripts/test-quarantine.py apply "$TMPDIR/confirm.json" --write \
    --issues "$TMPDIR/quarantine-issues"
```

- A `flaky` entry becomes `state: flaky`, `since` today, `expires` today + 14 days, its
  `evidence` and `issue` kept, and gets the body of an issue titled `flaky: <identity>`.
- An `always_fails` entry is removed, so the test fails every gate again, and gets a P0 issue
  body: main is broken.
- A `clean` entry is removed only when the report's host platform is the evidence platform: the
  entries' evidence comes from macOS gates (`Darwin`), because this repository's af gates run
  locally on macOS and the list rows carry no platform. `--evidence-platform linux` says the
  evidence came from Linux gates. A clean Claw (`Linux`) report therefore keeps a macOS entry
  `quarantined` with its dates untouched, and `apply` prints why.

Each issue body holds the failing output, the failing runs out of N, the commit, the host and
the evidence Tasks. `apply` refuses, and writes nothing, a report from a later day than today, a
report naming an identity the file no longer holds, a body that already exists under `--issues`,
and a result any quarantine guard refuses, as `scan --write` does. Without `--write` it changes
nothing. It never runs `gh`: with `--write` it prints one `gh issue create` line per body. File
each one:

```sh
gh issue create --title 'flaky: <identity>' --label flaky \
    --body-file "$TMPDIR/quarantine-issues/flaky-<identity slug>.md"
gh issue create --title 'P0: <identity> fails in every run' --label P0 \
    --body-file "$TMPDIR/quarantine-issues/p0-<identity slug>.md"
```

Optionally write a flaky issue's number into its entry's `issue` field. Then check the file and
commit it; `make check` validates it again:

```sh
python3 scripts/test-classes.py audit
git commit -am 'test quarantine: apply the confirmation of <commit>'
```

Like every change to this repository, the pull request carries an `af task report` block
([ADR-0142](adr/0142-carry-the-af-task-report-in-every-pull-request.md)) and the lines-added block
([ADR-0195](adr/0195-state-the-lines-a-pull-request-adds-per-directory-and-check-them.md)), which the PR report
check requires, and the quarantine block when the commit adds an entry to
`.config/test-quarantine.json`. `apply` changes and removes entries but adds none, so for its
commit alone `scripts/pr-quarantine.py` says no block is needed; the body below adds the block
only when the script prints one. The commit is a hand-made candidate, so an af verification Task judges it
(`default-light/verification`,
[ADR-0262](adr/0262-offer-a-light-default-pipeline-for-small-changes.md)), with `../base` the commit before it and the
confirmation report as its evidence. Then build the body from that Task and open the pull request:

```sh
{
  echo '## What'; echo
  echo 'Apply the Claw confirmation of <commit> to .config/test-quarantine.json.'; echo
  echo '## af task report'; echo
  af task report <verification Task ID>
  echo; echo '## Lines added'; echo
  python3 scripts/pr-lines.py --base origin/main
  python3 scripts/pr-quarantine.py --base origin/main > "$TMPDIR/quarantine-block.md"
  if [ -s "$TMPDIR/quarantine-block.md" ]; then
    echo; echo '## Test quarantine'; echo
    cat "$TMPDIR/quarantine-block.md"
  fi
} > "$TMPDIR/quarantine-pr.md"
git push -u origin test-quarantine-confirm
gh pr create --title 'test quarantine: apply the confirmation of <commit>' \
    --body-file "$TMPDIR/quarantine-pr.md"
```

## A macOS-only entry

A clean Linux confirmation never releases an entry whose evidence comes from macOS gates. To
release one, run `confirm` locally on macOS, on a clean checkout of the commit to confirm, and
apply that report:

```sh
python3 scripts/test-quarantine.py confirm --report "$TMPDIR/confirm-macos.json" --only '<identity>'
python3 scripts/test-quarantine.py apply "$TMPDIR/confirm-macos.json" --write \
    --issues "$TMPDIR/quarantine-issues"
```

Otherwise the entry stays `quarantined` until a fix removes it or it expires.
