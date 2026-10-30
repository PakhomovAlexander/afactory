# Nightly builds

The owner's Linux VM builds a signed nightly af from the newest green commit of main
([ADR-0265](adr/0265-build-nightlies-from-the-newest-green-main-on-the-owners-vm.md)). This page
covers the build step, `scripts/nightly/build.py`; the measurement, report and publication steps
come later. Nothing here runs on GitHub Actions, and the build never pushes, publishes, posts or
deletes anything outside `builds_root/<date>/`.

A nightly is tagged `nightly-YYYY.MM.DD` and versioned `<core>-nightly.<YYYYMMDD>`: the workspace
version of the chosen commit without its pre-release part (`0.14.0-rc.3` → `0.14.0`), or with its
minor bumped and patch zeroed when it is a plain release (`0.14.0` → `0.15.0`). It is built for
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` only, is never pinned by
`.af/af.lock`, and `install.sh` and `af self` on the stable and rc channels never choose it
([self-management](self-management.md#nightlies)).

## The host

An x86_64 Linux host (Debian) with Python 3.9 or later, `git`, Docker, `minisign`,
`qemu-aarch64-static` and:

- a bare mirror of the repository (`git clone --mirror`) that the host keeps fetched;
- the laboratory's musl image (`scripts/performance/images/Dockerfile`, stage `musl`), which
  carries the workspace's pinned toolchain (`rust-toolchain.toml`) and builds both targets: its
  one `rustup target add` line installs `x86_64-unknown-linux-musl` and
  `aarch64-unknown-linux-musl`. It is the image the performance executor pins by digest as its
  `build_image` for the `linux_x86_64_musl_claw` environment (`/etc/af-perf/executor.toml`,
  [the local executor](performance-local-executor.md)); the nightly's `build_image` names that
  same pinned digest. The build still refuses an image without both targets instead of
  installing them. A night never pulls an image;
- the nightly minisign key pair, separate from the release key.

## Configuration

One TOML file, `af.nightly-config/1`; [`nightly.example.toml`](../scripts/nightly/nightly.example.toml)
is a commented copy to start from. Unknown keys refuse the file.

| Key | Meaning |
|---|---|
| `schema` | `"af.nightly-config/1"`. |
| `repository` | `owner/name` on GitHub, whose check runs decide which commit is green. |
| `mirror` | Absolute path of the bare mirror; the night reads `refs/heads/main` and the tags. |
| `builds_root` | Absolute path holding one directory per night and the Cargo cache; apart from `mirror`. |
| `build_image` | The musl build image, `sha256:<image ID>` or `name@sha256:<digest>`. |
| `qemu_aarch64` | The user-mode runner the aarch64 smoke runs under; a missing one refuses the build. |
| `sign_key` | The nightly minisign secret key file. |
| `public_key` | The matching minisign public key file; the signature must verify against it. |
| `required_checks` | Check-run names that must be present and green, beside every `check /` run. A release-workflow job is refused here. |
| `max_walk` | How many first-parent commits back from main's tip may be inspected (1 to 200, default 20). |
| `github_token_file` | Optional file holding a read-only GitHub token; without it the API is asked anonymously, which works for a public repository. |

The key's passphrase, if it has one, is read from `AF_NIGHTLY_SIGN_PASSWORD` and given to
`minisign` on its standard input, never as a flag; without it the key is used with `minisign -W`
(a key without a passphrase).

## What a night does

`plan` chooses the commit and prints the plan (`af.nightly-plan/1`) without building:

1. The previous night is the newest `builds_root/<YYYY-MM-DD>/nightly.json` with an earlier
   date, whatever its status, or the built or skipped earlier record `--previous PATH` names.
   Its `head_sha` is the previous head. The plan's `previous_tag` is the tag of the last built
   night: the record's own `tag` when it is built, else the `previous_tag` the skipped record
   carries forward (none on the first nights).
2. Walk the first parents of the mirror's `refs/heads/main` from the tip, at most `max_walk`
   commits. A commit is green when its check runs (every page) include at least one `check /`
   run, every `check /` run is completed with `success` or `skipped`, and every `required_checks`
   name is present and green. The release workflow's jobs (`resolve the release`, `build …`,
   `sign and publish`, `pin the release in .af/af.lock`) are ignored. The walk chooses the
   newest commit whose checks are green and never goes behind the previous night's head, but it
   never chooses a commit without judging its checks, the previous head included. Each red
   commit is recorded under `rejected` with its reason. Reaching the previous head: green, the
   night is `skipped`; red (its checks were re-run and failed since), the night is refused with
   one sentence naming the previous head and why it is red, because a nightly never moves
   backwards. No green commit within `max_walk` refuses the night.
3. The baseline is the highest stable `vX.Y.Z` tag reachable from the chosen commit.
4. When the chosen commit is the previous night's head, the plan is `skipped` with a
   `skip_reason` naming the last built night's tag and the commit.

`build` then, unless the plan is skipped:

1. exports the commit into `source/` and sets `[workspace.package] version` in `Cargo.toml`
   and every workspace member's version in `Cargo.lock` to the nightly version;
2. fetches the locked dependencies into `builds_root/cache/cargo` (the only step with network),
   then runs `cargo zigbuild --release --locked --bin af --target <target>` for both targets in
   `build_image` without network, as the release workflow builds;
3. for each binary, as the release's `Prove, plan, and package` step does: `af --version` must
   print `af <nightly version>`, `af --version --json` must report the embedded release key and
   is kept as `version-<target>.json`, the chosen commit's `scripts/release-smoke.sh` must pass
   (the aarch64 binary runs under `qemu_aarch64`), and `dist/af-nightly-YYYY.MM.DD-<target>.tar.gz`
   holds `af`, `completions/` and `man/` rendered by that binary;
4. writes `dist/SHA256SUMS` in the release's format (`<sha256>  <archive>` per line), signs it
   with `minisign -S -W -s <sign_key> -m SHA256SUMS -t "af nightly-YYYY.MM.DD"` (without `-W`
   when `AF_NIGHTLY_SIGN_PASSWORD` gives a passphrase), and refuses the night unless
   `minisign -V` verifies it against `public_key`;
5. writes `nightly.json` (`af.nightly-build/1`).

A skipped night writes only its `nightly.json`, with status `skipped`, and builds nothing.

## The night's directory

```text
builds_root/
  cache/cargo/                 the Cargo home every night reuses (registry and git sources)
  2026-10-29/
    nightly.json               af.nightly-build/1, written last
    source/                    the exported, version-rewritten tree, with its target/
    dist/
      af-nightly-2026.10.29-x86_64-unknown-linux-musl.tar.gz
      af-nightly-2026.10.29-aarch64-unknown-linux-musl.tar.gz
      SHA256SUMS
      SHA256SUMS.minisig
    version-<target>.json      `af --version --json` of each binary
    logs/                      each container step's and smoke's output
    work/                      wrappers, staging, the smoke scratch and private homes
```

`nightly.json` records `date`, `tag`, `version`, `head_sha`, `head_committed_unix`, `rejected`,
`previous_head_sha`, `previous_tag`, `baseline_tag`, `baseline_sha`, `cargo_version_on_main`,
`build_image`, each target's `asset`, `sha256`, `bytes`, `smoke` and `version_json`, the
`sha256sums` paths, `timings` (unix milliseconds per phase: `plan`, `export`, `fetch`,
`build-<target>`, `package-<target>`, `sign`), `status` and `skip_reason`. Its schema is
`schemas/performance-nightly-build-v1.json`; the reader recomputes the tag, the version and the
asset names from the date and `cargo_version_on_main`.

A directory without `nightly.json` is an attempt that was refused; the next `build` of that date
replaces it. A night that already has `nightly.json` is never built again.

## Exit codes

| Exit | Meaning |
|---|---|
| 0 | Built (`build`), or planned (`plan`, `build --only-plan` of a night that would build). |
| 3 | Skipped: main's newest green commit is the previous night's (`build`, `build --only-plan`). |
| 1 | Refused, with the reason on stderr as `nightly: refused: …`; a malformed command line too. |

## Running one night by hand

```sh
git -C /var/lib/af-nightly/afactory.git fetch --prune origin
python3 -I scripts/nightly/build.py plan --config nightly.toml --date 2026-10-29
python3 -I scripts/nightly/build.py build --config nightly.toml --date 2026-10-29
```

Run it from a checkout of the trusted repository, not from the night's `source/`. `plan` prints
the commit it would build and every red commit it walked over; `build --only-plan` prints the
same plan and exits as `build` would (0 or 3) without writing anything. `--previous PATH`
compares with a built or skipped record other than the newest one.
