# Nightly builds

The owner's Linux VM builds a signed nightly af from the newest green commit of main
([ADR-0265](adr/0265-build-nightlies-from-the-newest-green-main-on-the-owners-vm.md)) and
publishes it as a GitHub pre-release
([ADR-0268](adr/0268-publish-nightlies-as-pre-releases-and-prune-them.md)). This page covers the
build step, `scripts/nightly/build.py`, and the publish step, `scripts/nightly/publish.py`; the
measurement and the daily report are their own steps, and the systemd chain that runs them in
turn comes later. Nothing here runs on GitHub Actions. The build never pushes, publishes, posts
or deletes anything outside `builds_root/<date>/` and its own claim `builds_root/<date>.active`;
only `publish` writes to GitHub and Telegram ([Publishing](#publishing)).

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
| `[publish]` | The table only `publish` reads, below; `build` ignores it, and `publish` refuses a file without it. |

The `[publish]` table:

| Key | Meaning |
|---|---|
| `github_token_file` | File holding a GitHub token with contents write on the repository, separate from the read-only `github_token_file` (the same path refuses the file). |
| `telegram_token_file` | Optional file holding the Telegram bot token; set together with `telegram_chat_id` or not at all. |
| `telegram_chat_id` | Optional chat the message goes to: a numeric chat id or `@channel`. Without the two keys nothing is sent and the receipt says so. |
| `keep_nights` | How many nightly pre-releases stay on GitHub (3 to 60, default 14). |

A token file holds the token alone on its first line. Neither token is ever printed, logged,
written to the receipt or passed as an argument.

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
   backwards. A main rewound below the previous head (a force-push) never reaches it: that
   night is refused too, naming both commits. No green commit within `max_walk` refuses the
   night.
3. The baseline is the highest stable `vX.Y.Z` tag reachable from the chosen commit.
4. When the chosen commit is the previous night's head, the plan is `skipped` with a
   `skip_reason` naming the last built night's tag and the commit.

`build` first claims the night, `builds_root/<date>.active` with its pid, created exclusively,
and refuses while another build holds it (a claim a dead build left is removed by hand); the
claim goes when the record is written. Then, unless the plan is skipped, it:

1. checks the commit out into `source/` (a clone of the mirror, objects hardlinked, so the
   built `af --version --json` reports that commit, which `build` requires) and sets
   `[workspace.package] version` in `Cargo.toml`
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
  2026-10-29.active            the pid of the build that holds the night, while it runs
  2026-10-29/
    nightly.json               af.nightly-build/1, written last
    source/                    the checked-out, version-rewritten tree, with its target/
    dist/
      af-nightly-2026.10.29-x86_64-unknown-linux-musl.tar.gz
      af-nightly-2026.10.29-aarch64-unknown-linux-musl.tar.gz
      SHA256SUMS
      SHA256SUMS.minisig
    version-<target>.json      `af --version --json` of each binary
    publish.json               af.nightly-publish/1, written by each `publish` of the night
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

## Publishing

```sh
python3 -I scripts/nightly/publish.py publish --config nightly.toml --date 2026-10-29 \
    --report /var/lib/af-nightly/reports/2026-10-29 [--notify-again]
```

`--report DIR` is the daily report's directory: `daily.json` must parse as JSON whose `schema`
starts with `af.nightly-daily/`, `report.html` is at most 4 MiB, and `digest.md` is UTF-8 text
of at most 3500 bytes; a missing, empty or oversized file refuses. A skipped night reads none of
them.

For a `built` night, `publish`:

1. verifies the night as `build` left it: each archive has the digest and size `nightly.json`
   records, `dist/SHA256SUMS` lists exactly those archives in the release's format, and
   `minisign -V -q -m SHA256SUMS -x SHA256SUMS.minisig -p <public_key>` verifies, with the
   trusted comment `af nightly-YYYY.MM.DD`. Any difference refuses before a request is sent;
2. creates the GitHub release of tag `nightly-YYYY.MM.DD` at the night's `head_sha`: a
   pre-release named `Nightly YYYY-MM-DD`, never marked latest, with `digest.md` as its notes.
   A tag that already names another commit refuses;
3. uploads, in this order, the two archives, `SHA256SUMS`, `SHA256SUMS.minisig`,
   `nightly.json`, `daily.json` and `report.html`. Each asset's size and digest as GitHub
   reports them must equal the file's;
4. sends `digest.md` as one plain-text Telegram message (no parse mode, link preview off) when
   `[publish]` names the bot and chat;
5. prunes: of the repository's releases whose tag matches
   `^nightly-[0-9]{4}\.[0-9]{2}\.[0-9]{2}$` and that are published pre-releases (not drafts),
   the ones beyond the newest `keep_nights` by tag date are deleted, oldest first, the release
   and then its tag. The night just published has no exemption: publishing a night older than
   the newest `keep_nights` deletes its release again, the receipt lists its tag as pruned and
   stderr says so. A nightly-named release that is a draft or not a pre-release is foreign: it
   is never deleted and is named on stderr and in the receipt. A tag counts as pruned only once
   its tag ref is deleted too; when the release went but the tag deletion failed, pruning stops
   there, the tag and the reason go to the receipt's `prune_failures`, and the tag is left for
   you to delete by hand. No other release or tag is ever touched;
6. writes the receipt `publish.json` and prints the release URL.

Every request goes to `api.github.com`, or for an upload to the release's `upload_url` on
`uploads.github.com`; each is bounded to 60 s and a bounded response, no redirect is followed,
and only reads are retried, once.

**Amending.** Publishing a night again (a late measurement, a recomposed report) finds the
release by its tag. It must be a pre-release whose target commit is `head_sha`, and the
archives, sums, signature and `nightly.json` already on it must carry the night's sizes and
digests: a difference refuses before any request that writes, because shipped bytes never
change. Then the notes, `daily.json` and `report.html` are replaced (each asset is deleted and
uploaded again), any of the seven assets an interrupted publish left out is uploaded, and the
receipt is rewritten with `amendments` counted. An amendment sends no Telegram message unless
`--notify-again` is given; the receipt keeps the earlier message id, or the earlier reason
there is none. After a failed message, publish again with `--notify-again` to send it. A
publish that died before writing its receipt (during an upload, say) sent no message: running
it again completes the release and then sends the digest, once.

**A skipped night** publishes no release and uploads nothing. It sends the one line
`nightly-YYYY.MM.DD: no new commits on main since <previous_tag>` and writes a receipt saying
so; publishing it again sends nothing, `--notify-again` included. A night without
`nightly.json` refuses.

**The receipt**, `builds_root/<date>/publish.json`, is `af.nightly-publish/1`
(`schemas/performance-nightly-publish-v1.json`): `status` (`published` or `skipped`), `date`,
`tag`, `head_sha`, `repository`, `release` (`id` and `url`, or `null`), `assets` (each one's
`name`, `size` and `digest` as GitHub reports them, in upload order), `telegram` (the
`message_id`, or `null`) and `telegram_reason` (why there is no message, or `null`), `pruned`
(tags whose release and tag ref are both deleted), `prune_failures` (each `tag` whose release
was deleted but whose tag deletion failed, with its `reason`), `foreign` (tags),
`amendments` (0 on the first publish) and `published_unix_ms`. The reader recomputes the tag,
the release URL and the asset names. A Telegram failure, a failed pruning request or a foreign
release after the release is published still writes the receipt.

## Exit codes

| Exit | Meaning |
|---|---|
| 0 | Built (`build`), or planned (`plan`, `build --only-plan` of a night that would build), or published with every step done (`publish`, a skipped night's line included). |
| 3 | Skipped: main's newest green commit is the previous night's (`build`, `build --only-plan`). |
| 1 | Refused, with the reason on stderr as `nightly: refused: …`; a malformed command line too. From `publish` also: published, with the receipt written, but the Telegram message or a pruning request failed (`nightly: failed: …`) or a foreign nightly-named release was left alone (`nightly: refused: …`). |

## Running one night by hand

```sh
git -C /var/lib/af-nightly/afactory.git fetch --prune origin
python3 -I scripts/nightly/build.py plan --config nightly.toml --date 2026-10-29
python3 -I scripts/nightly/build.py build --config nightly.toml --date 2026-10-29
python3 -I scripts/nightly/publish.py publish --config nightly.toml --date 2026-10-29 \
    --report /var/lib/af-nightly/reports/2026-10-29
```

Run it from a checkout of the trusted repository, not from the night's `source/`. `plan` prints
the commit it would build and every red commit it walked over; `build --only-plan` prints the
same plan and exits as `build` would (0 or 3) without writing anything. `--previous PATH`
compares with a built or skipped record other than the newest one. `publish` prints the
release URL; running it again amends the night.
