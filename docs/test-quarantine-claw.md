# Confirming quarantined tests on Claw

How the owner settles the entries of `.config/test-quarantine.json`
([ADR-0264](adr/0264-quarantine-flaky-tests-on-gate-evidence-and-confirm-them-on-claw.md),
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
the run happens in a container. The image is the Rust image of `rust-toolchain.toml`'s version,
`rust:1.88.0`; cargo-nextest is installed in it. The source is a fresh clone of the commit to
confirm, mounted in, so the report's commit is that commit. On the owner's machine, pick the
commit (the `main` commit whose `.config/test-quarantine.json` holds the entries):

```sh
COMMIT=$(git rev-parse origin/main)
ssh claw-3
```

On Claw, with the same `COMMIT` set there:

```sh
WORK="$HOME/af-quarantine-$COMMIT"
mkdir -p "$WORK"
docker run --rm -v "$WORK":/work -w /work rust:1.88.0 sh -c "
  git clone https://github.com/PakhomovAlexander/afactory.git src &&
  git -C src checkout --detach $COMMIT"
docker run --rm -v "$WORK":/work -w /work/src rust:1.88.0 sh -c '
  set -e
  apt-get update && apt-get install -y python3 git
  cargo install cargo-nextest --locked
  python3 scripts/test-quarantine.py confirm --report /work/confirm.json'
exit
```

Back on the owner's machine, copy the report:

```sh
scp "claw-3:af-quarantine-$COMMIT/confirm.json" "$TMPDIR/confirm.json"
```

`confirm` prints each identity's classification and `confirm: wrote /work/confirm.json`. It
exits 1, with no report, when the file fails a guard, a run lacks a result, or the report file
exists.

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
open the pull request that carries it; `make check` validates it again:

```sh
python3 scripts/test-classes.py audit
git commit -am 'test quarantine: apply the confirmation of <commit>'
gh pr create --fill
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
