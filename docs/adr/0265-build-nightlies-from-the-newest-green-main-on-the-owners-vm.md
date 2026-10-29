# ADR-0265: Build nightlies from the newest green main on the owner's VM

**Status:** proposed (2026-10-29).

## Context

Releases are cut only through the release train
([ADR-0045](0045-one-release-train-and-a-pin-that-binds-bytes.md)): a version-bump pull request,
the release workflow on GitHub Actions, the release key, a pin in `.af/af.lock`. Between two
releases main moves for days, and nothing builds what it holds as the binaries a user installs.
On 2026-10-29 the owner decided on a nightly train: each night a nightly af is built, measured
and published from the owner's Linux VM, the machine that already executes performance
experiments ([ADR-0240](0240-execute-performance-experiments-independently-on-a-locked-vm.md)),
never from GitHub Actions. This decision is the build step; measurement, the daily report and
publication are later steps.

Main's tip is not always green: today `f8bb1dd9` failed `check / lint`, `check / check` and
`check / class perf`, while its parent `e9830a1d` passed.

## Considered options

- Build nightlies in a scheduled Actions workflow: rejected. The owner wants the nightly train
  off Actions, beside the VM's measurements, and a scheduled workflow would be a second release
  workflow holding a signing secret.
- Build main's tip every night: rejected. A red tip would ship a binary its own checks refused.
- Build every night whether or not main moved: rejected. Two nightlies of one commit differ
  only in their date, and every later step (measurement, report, publication) would repeat
  itself for nothing.
- Tag nightlies `vX.Y.Z-nightly.N`: rejected. Every release tool reads `v…` tags: the release
  workflow's tag trigger, `scripts/resolve-release.sh`, `scripts/pin-release.sh`, `install.sh`'s
  ranking and `af self`'s channels would each need a new exclusion.
- Sign nightlies with the release key: rejected. The release key signs only what the release
  train built; a nightly is built elsewhere, so a leaked nightly key must never sign a release.

## Decision

`scripts/nightly/build.py` (stdlib Python, `python3 -I`) runs on the owner's x86_64 Linux VM
with Docker, `git`, `minisign` and `qemu-aarch64-static`, reading one `af.nightly-config/1` TOML
([`docs/nightly.md`](../nightly.md) lists its keys).

1. **The newest green main.** `plan` walks the first parents of `refs/heads/main` in the host's
   bare mirror from the tip, at most `max_walk` commits (20 by default), and chooses the first
   commit whose GitHub check runs (every page of `GET
   /repos/{repository}/commits/{sha}/check-runs`) are green: at least one `check /` run, every
   `check /` run completed with conclusion `success` or `skipped`, and every name in
   `required_checks` present and green. The release workflow's own jobs (`resolve the release`,
   `build …`, `sign and publish`, `pin the release in .af/af.lock`) are ignored and cannot be
   required. Each red commit walked over is recorded with its reason. Nothing green within
   `max_walk` refuses the night. Every API request is bounded (5 s to connect, 30 s per read)
   and retried once; without `github_token_file` it is anonymous.
2. **Skip when nothing changed.** The previous night is the newest built
   `builds_root/<date>/nightly.json` before this date, or `--previous PATH`. The walk never goes
   behind the previous night's head: reaching it chooses it. When the chosen commit is the
   previous night's head, the plan is `skipped` with a reason naming that night's tag and
   commit, and `build` writes a skipped record, builds nothing and exits 3.
3. **Tag and version.** The tag is `nightly-YYYY.MM.DD`, never `v…`, so the release tools keep
   ignoring it. The version is `<core>-nightly.<YYYYMMDD>`, a SemVer pre-release `af self` can
   install beside releases and order: a pre-release workspace version keeps its core
   (`0.14.0-rc.3` → `0.14.0-nightly.20261029`), a plain release bumps its minor and zeroes its
   patch (`0.14.0` → `0.15.0-nightly.20261029`), so a nightly is always ahead of the release
   main last shipped. The baseline is the highest stable `vX.Y.Z` tag reachable from the chosen
   commit, exactly `nightly_baseline` of `scripts/performance/perflab/gitrefs.py`, which the
   build imports.
4. **The build is the release's.** `build` exports the chosen commit into
   `builds_root/<date>/source/`, rewrites `[workspace.package] version` in `Cargo.toml` and
   every workspace member's version in `Cargo.lock` to the nightly version, fetches the locked
   dependencies into the persistent `builds_root/cache/cargo`, then runs `cargo zigbuild
   --release --locked --bin af --target <target>` for `x86_64-unknown-linux-musl` and
   `aarch64-unknown-linux-musl` in the laboratory's musl image pinned by digest, without
   network. As release.yml's `Prove, plan, and package` step does, each binary must report
   `af <version>` and embed the release key, `af --version --json` is kept, the chosen commit's
   `scripts/release-smoke.sh` runs on it (the aarch64 one under `qemu_aarch64`; a missing runner
   refuses the night, it never skips the smoke), and `af-nightly-YYYY.MM.DD-<target>.tar.gz`
   holds `af`, `completions/` and `man/` as a release archive does.
5. **A separate key.** `SHA256SUMS` is written in the release's exact format and signed with
   the nightly key `sign_key` (`minisign -S -W … -t "af nightly-YYYY.MM.DD"`; a passphrase comes
   only from `AF_NIGHTLY_SIGN_PASSWORD`, on minisign's standard input), and the night is refused
   unless the signature verifies against `public_key`. The build never reads the release key
   or touches the release workflow. The public key `crates/af/keys/nightly.pub` is added by the
   owner later and embedded by a later change; until then the configuration names it.
6. **The record.** `builds_root/<date>/nightly.json` is `af.nightly-build/1`, the plan
   `af.nightly-plan/1` and the configuration `af.nightly-config/1`, registered with the
   laboratory's contracts, with generated schemas and golden fixtures; the reader recomputes the
   tag, the version and the asset names. Exit codes: 0 built, 3 skipped, 1 refused with the
   reason on stderr. Nothing is pushed, published, posted or deleted outside
   `builds_root/<date>/`.
7. **What a nightly is not.** It is Linux only: the VM cannot build macOS, so a nightly has no
   `aarch64-apple-darwin` archive. It is never pinned by `.af/af.lock`, never chosen by
   `install.sh` or by `af self` on the stable or rc channel, and never a baseline.

## Consequences

The owner runs one night by hand with the steps in [`docs/nightly.md`](../nightly.md); systemd
units, the measurement and publication steps, and an `af self` nightly channel are later
changes. The build image must carry both musl Rust targets and the workspace's pinned
toolchain: the laboratory's current `musl` stage adds only `x86_64-unknown-linux-musl`, and the
build refuses an image without both rather than installing anything into it.

`scripts/performance/tests/test_nightly_build.py` proves the walk, the skip rule, the version
and tag rules, the Cargo rewrite (with `cargo metadata --locked --offline` on a rewritten copy
of this tree), the SHA256SUMS format against a release's own file, the record's contract and a
whole night against a fake host, and that `install.sh`, `scripts/pin-release.sh`,
`scripts/resolve-release.sh` and `af self` ignore a `nightly-…` tag (with the unit test
`newest_never_chooses_a_nightly_tag` in `crates/af/src/selfmgmt.rs`). No test runs Docker,
reaches the network or builds Rust; a real night on the VM is the owner's proof.
