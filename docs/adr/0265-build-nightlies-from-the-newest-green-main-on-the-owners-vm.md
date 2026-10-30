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
   required. The walk never chooses a commit without judging its checks, the previous night's
   head included. Each red commit walked over is recorded with its reason. Nothing green within
   `max_walk` refuses the night. Every API request is bounded (5 s to connect, 30 s per read)
   and retried once; without `github_token_file` it is anonymous.
2. **Skip when nothing changed.** The previous night is the newest earlier
   `builds_root/<YYYY-MM-DD>/nightly.json` whatever its status, or the built or skipped earlier
   record `--previous PATH` names; its `head_sha` is the previous head. The plan's
   `previous_tag` is the tag of the last built night: the record's own `tag` when it is built,
   else the `previous_tag` the skipped record carries forward (none on the first nights). The
   walk chooses the newest commit whose checks are green and never goes behind the previous
   night's head, but it never chooses a commit without judging its checks, the previous head
   included. Reaching the previous head: green, the night is `skipped` with a reason naming the
   last built night's tag and the commit, and `build` writes a skipped record, builds nothing
   and exits 3; red (its checks were re-run and failed since), the night is refused with one
   sentence naming the previous head and why it is red, because a nightly never moves
   backwards; so is a night whose main was rewound below the previous head, which the walk
   then never meets (`make_plan` checks that the previous head is an ancestor of the chosen
   commit).
3. **Tag and version.** The tag is `nightly-YYYY.MM.DD`, never `v…`, so the release tools keep
   ignoring it. The version is `<core>-nightly.<YYYYMMDD>`, a SemVer pre-release `af self` can
   install beside releases and order: a pre-release workspace version keeps its core
   (`0.14.0-rc.3` → `0.14.0-nightly.20261029`), a plain release bumps its minor and zeroes its
   patch (`0.14.0` → `0.15.0-nightly.20261029`), so a nightly is always ahead of the release
   main last shipped. The baseline is the highest stable `vX.Y.Z` tag reachable from the chosen
   commit, exactly `nightly_baseline` of `scripts/performance/perflab/gitrefs.py`, which the
   build imports.
4. **The build is the release's.** `build` claims the night (`builds_root/<date>.active`,
   created exclusively, so two builds of one night never clear or sign each other's files),
   checks the chosen commit out into `builds_root/<date>/source/` as a clone of the mirror
   (so the built binary reports that commit, which `build` requires of it), rewrites `[workspace.package] version` in `Cargo.toml` and
   every workspace member's version in `Cargo.lock` to the nightly version, fetches the locked
   dependencies into the persistent `builds_root/cache/cargo`, then runs `cargo zigbuild
   --release --locked --bin af --target <target>` for `x86_64-unknown-linux-musl` and
   `aarch64-unknown-linux-musl` in the laboratory's musl image pinned by digest, without
   network. That image (`scripts/performance/images/Dockerfile`, stage `musl`) builds both
   targets: its one `rustup target add` line installs both, and `build_image` names the digest
   the performance executor pins as its `build_image` for the `linux_x86_64_musl_claw`
   environment. As release.yml's `Prove, plan, and package` step does, each binary must report
   `af <version>` and embed the release key, `af --version --json` is kept, the chosen commit's
   `scripts/release-smoke.sh` runs on it (the aarch64 one under `qemu_aarch64`; a missing runner
   refuses the night, it never skips the smoke), and `af-nightly-YYYY.MM.DD-<target>.tar.gz`
   holds `af`, `completions/` and `man/` as a release archive does.
5. **A separate key.** `SHA256SUMS` is written in the release's exact format and signed with
   the nightly key `sign_key`:
   `minisign -S -W -s <sign_key> -m SHA256SUMS -t "af nightly-YYYY.MM.DD"` for a key without a
   passphrase; a passphrase comes only from `AF_NIGHTLY_SIGN_PASSWORD` and is given on
   minisign's standard input, without `-W`. The night is refused unless the signature verifies
   against `public_key`. The build never reads the release key or touches the release
   workflow. The public key `crates/af/keys/nightly.pub` is added by the owner later and
   embedded by a later change; until then the configuration names it.
6. **The record.** `builds_root/<date>/nightly.json` is `af.nightly-build/1`, the plan
   `af.nightly-plan/1` and the configuration `af.nightly-config/1`, registered with the
   laboratory's contracts, with generated schemas and golden fixtures; the reader recomputes the
   tag, the version and the asset names. Exit codes: 0 built, 3 skipped, 1 refused with the
   reason on stderr. Nothing is pushed, published, posted or deleted outside
   `builds_root/<date>/` and the build's own claim `builds_root/<date>.active`.
7. **What a nightly is not.** It is Linux only: the VM cannot build macOS, so a nightly has no
   `aarch64-apple-darwin` archive. It is never pinned by `.af/af.lock`, never chosen by
   `install.sh` or by `af self` on the stable or rc channel, and never a baseline.

## Consequences

The owner runs one night by hand with the steps in [`docs/nightly.md`](../nightly.md); systemd
units, the measurement and publication steps, and an `af self` nightly channel are later
changes. The laboratory's `musl` stage now installs `aarch64-unknown-linux-musl` beside
`x86_64-unknown-linux-musl`, so its image carries both musl Rust targets and the workspace's
pinned toolchain; the build still refuses an image without both rather than installing anything
into it.

`scripts/performance/tests/test_nightly_build.py` proves the walk (a red previous head refuses
the night), the skip rule (after skipped nights too), that the `musl` stage installs exactly the
build's targets, the version
and tag rules, the Cargo rewrite (with `cargo metadata --locked --offline` on a rewritten copy
of this tree), the SHA256SUMS format against a release's own file, the record's contract and a
whole night against a fake host, and that `install.sh`, `scripts/pin-release.sh`,
`scripts/resolve-release.sh` and `af self` ignore a `nightly-…` tag (with the unit test
`newest_never_chooses_a_nightly_tag` in `crates/af/src/selfmgmt.rs`). No test runs Docker,
reaches the network or builds Rust; a real night on the VM is the owner's proof.
