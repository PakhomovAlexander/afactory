# Nightly builds

The owner's Linux VM builds a signed nightly af from the newest green commit of main
([ADR-0265](adr/0265-build-nightlies-from-the-newest-green-main-on-the-owners-vm.md)),
composes its [daily report](#the-daily-report)
([ADR-0270](adr/0270-compose-the-nightly-report-from-the-nights-record-and-the-executor-journal.md))
and publishes it as a GitHub pre-release
([ADR-0271](adr/0271-publish-nightlies-as-pre-releases-and-prune-them.md)), every night, as four
systemd units that hand over to each other
([ADR-0272](adr/0272-run-the-nightly-chain-as-systemd-units-on-the-owners-vm.md)). This page
covers the build step, `scripts/nightly/build.py`, the measure step, `scripts/nightly/measure.py`,
which submits the night to the [nightly executor](performance-local-executor.md#nightly-originals)
and waits for it, the report step, `scripts/nightly/report.py`, the publish step,
`scripts/nightly/publish.py`, and the runbook of [the nightly chain](#the-nightly-chain) that
runs them in turn. Nothing here runs on GitHub Actions. The build never pushes, publishes,
posts or deletes anything outside `builds_root/<date>/` and its own claim
`builds_root/<date>.active`; the measurement writes only through the nightly executor; the
report writes only its new directory; only `publish` writes to GitHub and Telegram
([Publishing](#publishing)).

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
| `github_token_file` | Optional file holding a read-only GitHub token, for the build's check runs and the report's reads alike; without it the API is asked anonymously, which works for a public repository. |
| `reports_root` | Optional absolute path below which `compose` writes each night's report directory and `publish` finds the newest, when the command line names neither a directory nor a root ([The daily report](#the-daily-report)); apart from `builds_root` and `mirror`. |
| `[measure]` | Optional table that only `measure.py` reads; every key has a default. |
| `measure.deadline_minutes` | How long `measure` cycles the nightly executor before it gives up on the originals still queued or running (30 to 600, default 300). |
| `measure.poll_seconds` | How long `measure` waits after a `busy` or `idle` cycle, or while another process holds the journal (10 to 600, default 60). |
| `[report]` | Optional table that `report.py` and `measure.py` read; `compose` and `measure` refuse a configuration without it. |
| `report.executor_config` | Absolute path of the nightly executor's configuration (`/etc/af-nightly/executor.toml`); its `root` is the journal the report reads and its `machine_id` names the host, and `measure` submits to and cycles it. |
| `report.history_nights` | How many earlier nights the trend shows beside this one: the last that many the journal holds a core original of, by tag date (1 to 60, default 14). |
| `report.headline_instances` | One to four instance IDs, as the suite names them, whose candidate medians the trend plots. |
| `[publish]` | The table only `publish` reads, below; `build` ignores it, and `publish` refuses a file without it. |

The `[publish]` table:

| Key | Meaning |
|---|---|
| `github_token_file` | The file holding a GitHub token with contents write on the repository, never the read-only top-level `github_token_file`; it creates, amends and prunes the nightly pre-releases. |
| `telegram_token_file` | Optional file holding the Telegram bot token; set together with `telegram_chat_id` or not at all. |
| `telegram_chat_id` | Optional chat the message goes to: a numeric chat id or `@channel`. Without the two keys nothing is sent and the receipt says so. |
| `keep_nights` | How many nightly pre-releases stay on GitHub (3 to 60, default 14). |

A token file holds the token alone on its first line. Neither token is ever printed, logged,
written to the receipt or passed as an argument.

The key's passphrase, if it has one, is read from `AF_NIGHTLY_SIGN_PASSWORD`, or else from the
systemd credential `af-nightly-sign-password` (`$CREDENTIALS_DIRECTORY/af-nightly-sign-password`,
its first line), and given to `minisign` on its standard input, never as a flag; without either
the key is used with `minisign -W` (a key without a passphrase).

Every step, `plan` included, takes its night as `--date YYYY-MM-DD`, or as `--night-of UNIT`:
the UTC date at which systemd last started the service UNIT (`systemctl show
--property=ExecMainStartTimestamp --timestamp=unix`); a unit that has not started since boot
refuses. Without either the night is today's UTC date. The step prints the night it chose on
stderr, `nightly: --date 2026-10-29 (today's UTC date)` or `nightly: --date 2026-10-29 (the UTC
date af-nightly-build.service last started)`. The chain's units pass `--night-of
af-nightly-build.service`, so the four steps of one night agree however long the measurement
runs past midnight UTC ([The nightly chain](#the-nightly-chain)).

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
python3 -I -B scripts/nightly/publish.py publish --config nightly.toml --date 2026-10-29 \
    --report-root /var/lib/af-nightly/reports [--notify-again]
```

The report is `--report DIR`, the daily report's directory, or else the newest of the night's
directories below `--report-root ROOT` or, without either, below the configuration's
`reports_root`: of `ROOT/<date>` and `ROOT/<date>.<k>` the one with the highest `k`, as
[`compose --output-root`](#the-daily-report) names them; a link is never one. `publish` prints
the directory it chose and the root it defaulted to on stderr. `daily.json` must parse as JSON
whose `schema` starts with `af.nightly-daily/`, `report.html` is at most 4 MiB, and `digest.md`
is UTF-8 text of at most 3500 bytes; a missing, empty or oversized file refuses, and so does a
built night with no report directory under the root. A skipped night reads none of them.

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
publish that died before writing its receipt is completed by running it again, which sends
the digest: once when the first run died during an upload, before the message; a second time
when it died after the message, while pruning or writing the receipt, since the receipt is the
only memory of a message.

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

These are `build.py`'s and `publish.py`'s; `measure.py`'s are under
[The measurement](#the-measurement), `report.py`'s under [Composing again](#composing-again).

| Exit | Meaning |
|---|---|
| 0 | Built (`build`), or planned (`plan`, `build --only-plan` of a night that would build), or published with every step done (`publish`, a skipped night's line included). |
| 3 | Skipped: main's newest green commit is the previous night's (`build`, `build --only-plan`). |
| 1 | Refused, with the reason on stderr as `nightly: refused: …`; a malformed command line too. From `publish` also: published, with the receipt written, but the Telegram message or a pruning request failed (`nightly: failed: …`) or a foreign nightly-named release was left alone (`nightly: refused: …`). |

## The measurement

`measure` submits the night to the nightly executor and waits for it, inside its own process:

```sh
python3 -I -B scripts/nightly/measure.py submit --config nightly.toml --date 2026-10-29
```

It reads `builds_root/<date>/nightly.json`. A skipped night measures nothing and exits 0. For a
built night it submits the core and then the telemetry original through the executor of the
tree it runs from, never another checkout, to the configuration `[report] executor_config`
names:

```sh
python3 -I -B scripts/performance/cli.py executor --config <executor_config> submit \
    --work core --kind nightly --tag nightly-2026.10.29 --head <head_sha>
python3 -I -B scripts/performance/cli.py executor --config <executor_config> submit \
    --work telemetry --kind nightly --tag nightly-2026.10.29 --head <head_sha>
```

A submission of a night already submitted prints the existing original, so running `measure`
again is safe. Then it runs that executor's `status` and `cycle` in turn until `status` shows
both originals in a terminal state (`complete`, `failed`, `interrupted`, `uncertain` or
`expired`) or `[measure] deadline_minutes` have passed since it started. After a cycle that
answers `busy` (the machine lock is held) or `idle`, or while another process holds the journal,
it waits `[measure] poll_seconds`, never past the deadline. No cycle starts after the deadline,
and a cycle that has started is never interrupted: the executor bounds it by its own
`run_seconds`. A cycle the executor refuses is followed by a `status`: when an original moved
(a failed launch ends it `uncertain`) the wait goes on, otherwise the step fails. On success it
prints each original on stdout, `nightly:nightly-2026.10.29:core exp_… complete`. The nightly
executor's configuration is submitted to and cycled by this step alone: never run an
`af-perf-executor@` unit on it, so the two never race for its journal.

| Exit | Meaning |
|---|---|
| 0 | Both originals ended, whatever their outcome, or the night was skipped. |
| 1 | `nightly: failed: …`: the deadline passed, and the message names each original still queued or running (`running (claimed)`); or the executor refused a `status`, or a `cycle` after which nothing moved. `nightly: refused: …`: the night has no `nightly.json`, the configuration has no `[report]` table or is malformed, the command line is malformed, or the executor refused a submission (a core original submitted before a refused telemetry one stays queued until `measure` runs again). |

## The nightly chain

On the owner's VM the four steps run as systemd units from the installed control
`/opt/af-perf/control` ([the local executor](performance-local-executor.md#install-trusted-control-and-source-access));
their files are in [`scripts/performance/service/`](../scripts/performance/service/):

| Unit | Account | Runs | Then |
|---|---|---|---|
| `af-nightly.timer` | | `OnCalendar=*-*-* 22:05:00 UTC`, `Persistent=true` | starts `af-nightly-build.service` |
| `af-nightly-build.service` | `af-nightly` | `build.py build` | `OnSuccess=af-nightly-measure.service`: a built night (exit 0) or a skipped one (exit 3, `SuccessExitStatus=3`) |
| `af-nightly-measure.service` | `af-perf-exec` | `measure.py submit` | `OnSuccess=` and `OnFailure=af-nightly-report.service` |
| `af-nightly-report.service` | `af-perf-exec` | `report.py compose --output-root /var/lib/af-nightly/reports` | `OnSuccess=af-nightly-publish.service` |
| `af-nightly-publish.service` | `af-nightly` | `publish.py publish --report-root /var/lib/af-nightly/reports` | the night ends |

Each service is `Type=oneshot` in the group `af-perf-readers` and runs
`/usr/bin/python3 -I -B /opt/af-perf/control/scripts/nightly/<script>.py … --config
/etc/af-nightly/nightly.toml --night-of af-nightly-build.service` with
`Environment=PYTHONDONTWRITEBYTECODE=1`, each its own process under its own account, so a step
that dies takes no other with it. Every step works on the UTC date at which the build unit
started, the night it built, however late the measurement ends. A refused build ends the chain:
the later steps need its `nightly.json`. A measurement that failed or passed its deadline is
still reported, its originals shown as they stand, and published. A refused compose has no
report to publish and ends the chain; rerun it once its cause is fixed ([Recovery](#recovery)).

The units carry the executor units' bounds (`TimeoutStartSec`, `KillMode=control-group`,
`MemoryMax`, `TasksMax`, `LimitNOFILE`, log rate limits). The build and the measurement share
the host's `/tmp` and mount namespace (`PrivateTmp=no`, `PrivateMounts=no`, no `ProtectSystem`
or `ProtectHome`, `OOMPolicy=stop`): the build bind-mounts the night's directory into its
containers and the Docker daemon resolves those paths in the host's namespace, and the
measurement takes the machine lock in `/tmp` and loop-mounts scratch the daemon must see, as
the executor's compute unit does. The report and the publish measure nothing and are hardened
like the source refresh (`ProtectSystem=strict`, `NoNewPrivileges=yes`, `ProtectHome=yes`,
`ReadWritePaths=` the reports root or the builds root); the publish has a private `/tmp`, and
the report keeps the host's to see whether the machine lock is held.

### What runs when

At 22:05 UTC the timer starts the build; `Persistent=true` starts a night the VM was off for
when it is up again, as the night of the day it starts then. The build takes about 25 minutes
(its unit allows 2 hours). The measurement waits for the core nightly, about three hours
([Nightly originals](performance-local-executor.md#nightly-originals)), and then the telemetry
original, up to `deadline_minutes` (300 by default) plus the one cycle running at the
deadline (its unit allows 18 h 30 min: the longest deadline, 600 minutes, and the example's
8-hour `run_seconds`). Compose and publish take minutes (their units allow 1 hour each, the
bounds of their GitHub requests). A built night is so published in the small hours UTC of
the next day, a skipped night within minutes, and the four steps work on the same night: the
four bounds add to 22 h 30 min, less than the day between two timer starts, so the next
build never starts while a chain still runs, and a step whose build unit last started more
than 23 hours ago refuses as out of order rather than work on another night.

## Install

The performance executor is installed first, with its accounts `af-perf-source` and
`af-perf-exec`, the group `af-perf-readers`, the source mirror and its refresh timer, the machine
lock and the installed control ([the local executor](performance-local-executor.md)); the
nightly executor's configuration is `/etc/af-nightly/executor.toml`
([Nightly originals](performance-local-executor.md#nightly-originals)). Then, as root:

```sh
useradd --system --user-group --shell /usr/sbin/nologin af-nightly
usermod --append --groups af-perf-readers,docker af-nightly
install -d -o root -g af-perf-readers -m 0750 /etc/af-nightly
install -d -o root -g root -m 0755 /var/lib/af-nightly
install -d -o af-nightly -g af-perf-readers -m 0750 /var/lib/af-nightly/builds
install -d -o af-perf-exec -g af-perf-readers -m 0750 /var/lib/af-nightly/reports
```

| Path | Owner, group, mode | Holds |
|---|---|---|
| `/etc/af-nightly/nightly.toml` | `root`, `af-perf-readers`, 0640 | this configuration, from [`nightly.example.toml`](../scripts/nightly/nightly.example.toml); it names files and holds no secret |
| `/etc/af-nightly/executor.toml` | `root`, `af-perf-readers`, 0640 | the nightly executor's configuration, writable by no one else, as the executor requires |
| `/etc/af-nightly/nightly.key` | `root`, `af-nightly`, 0440 | the nightly minisign secret key |
| `/etc/af-nightly/nightly.pub` | `root`, `af-perf-readers`, 0644 | its public key |
| `/etc/af-nightly/github-token` | `root`, `af-perf-readers`, 0440 | the optional read-only token, read by the build and the report |
| `/etc/af-nightly/github-publish-token`, `/etc/af-nightly/telegram-token` | `root`, `af-nightly`, 0440 | the publish tokens, readable by `af-nightly` alone |
| `builds_root`, `/var/lib/af-nightly/builds` | `af-nightly`, `af-perf-readers`, 0750 | the nights and the Cargo cache; the measurement and the report read it |
| `reports_root`, `/var/lib/af-nightly/reports` | `af-perf-exec`, `af-perf-readers`, 0750 | the report directories; the publish reads them |
| `mirror` | the executor's `/var/lib/af-perf-executor/source.git`, or one of your own | main and the tags; the executor's mirror, which the source refresh keeps fetched and the group reads, can serve; a mirror of your own needs a fetch before 22:05 UTC |

The units set `UMask=0027`, so what one step writes the next reads through the group; the
measurement keeps the executor's `UMask=0077` for the journal. Secrets stay files: the
configuration names the token files, and a unit names credentials, never a value. For a
nightly key with a passphrase, put it in `/etc/af-nightly/sign-password` (root, 0600) and
uncomment `LoadCredential=af-nightly-sign-password:/etc/af-nightly/sign-password` in the build
unit, or put `AF_NIGHTLY_SIGN_PASSWORD=…` in a root-owned 0600 file and uncomment its
`EnvironmentFile=` line instead. The publish tokens may be held as systemd credentials too:
uncomment the publish unit's `LoadCredential=github-publish-token:/etc/af-nightly/github-publish-token`
(and `telegram-token`), make the files root's alone, and point `[publish] github_token_file`
at `/run/credentials/af-nightly-publish.service/github-publish-token` (and `telegram_token_file`
at `…/telegram-token`); the step then reads them only inside its unit.

Install the units and start the timer:

```sh
install -m 0644 /opt/af-perf/control/scripts/performance/service/af-nightly.timer \
    /opt/af-perf/control/scripts/performance/service/af-nightly-*.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now af-nightly.timer
systemctl list-timers af-nightly.timer
```

Only the timer is enabled; the timer and the hand-over start the four services.

## The first night by hand

Before the first evening, run the four steps once in the units' order, each as its unit's
account and in the shared group `af-perf-readers`, as the units run (`-g af-perf-readers`: an
account's own primary group would leave the night's directory unreadable to the next step),
from the installed control, with `umask 027` in your shell as the units have it:

```sh
cd /
umask 027
sudo -u af-nightly -g af-perf-readers python3 -I -B /opt/af-perf/control/scripts/nightly/build.py build \
    --config /etc/af-nightly/nightly.toml --date 2026-10-29
sudo -u af-perf-exec -g af-perf-readers python3 -I -B /opt/af-perf/control/scripts/nightly/measure.py submit \
    --config /etc/af-nightly/nightly.toml --date 2026-10-29
sudo -u af-perf-exec -g af-perf-readers python3 -I -B /opt/af-perf/control/scripts/nightly/report.py compose \
    --config /etc/af-nightly/nightly.toml --date 2026-10-29 \
    --output-root /var/lib/af-nightly/reports
sudo -u af-nightly -g af-perf-readers python3 -I -B /opt/af-perf/control/scripts/nightly/publish.py publish \
    --config /etc/af-nightly/nightly.toml --date 2026-10-29 \
    --report-root /var/lib/af-nightly/reports
```

What each leaves behind, and where to look:

1. `build` prints the night's record and exits 0 (built) or 3 (skipped):
   `/var/lib/af-nightly/builds/2026-10-29/nightly.json`, with `dist/` and `logs/` beside it
   ([The night's directory](#the-nights-directory)). `plan` and `build --only-plan` show the
   commit it would choose without building; `--previous PATH` compares with another record.
2. `measure` prints each original and its end state. The journal's own view (while a cycle
   runs, the journal is held and this `status` is refused as busy; `measure` in turn waits
   while yours runs):

   ```sh
   sudo -u af-perf-exec -g af-perf-readers python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
       --config /etc/af-nightly/executor.toml status
   ```

   lists every original's state; the night's are keyed `nightly:nightly-2026.10.29:core` and
   `…:telemetry` in each request's `submission`, and `verify --request-id` prints a result.
3. `compose` prints the directory it wrote, `/var/lib/af-nightly/reports/2026-10-29`, holding
   `daily.json`, `report.html` and `digest.md` ([The daily report](#the-daily-report)).
4. `publish` prints the release URL and writes
   `/var/lib/af-nightly/builds/2026-10-29/publish.json` ([Publishing](#publishing)).

A key with a passphrase, or tokens held as systemd credentials, are only there inside the
units: run that night through them instead, `systemctl start af-nightly-build.service`, which
runs the whole chain for today's UTC date, and follow it with `journalctl -f -u 'af-nightly-*'`.

## Amending a night

A result that arrives after the night was reported, a measurement that passed its deadline or
a report you want again: compose the night again, which writes the next directory
(`2026-10-29.2`), and publish it, which amends the release with that newest report and sends no
second message unless `--notify-again`:

```sh
sudo -u af-perf-exec -g af-perf-readers python3 -I -B /opt/af-perf/control/scripts/nightly/report.py compose \
    --config /etc/af-nightly/nightly.toml --date 2026-10-29 \
    --output-root /var/lib/af-nightly/reports
sudo -u af-nightly -g af-perf-readers python3 -I -B /opt/af-perf/control/scripts/nightly/publish.py publish \
    --config /etc/af-nightly/nightly.toml --date 2026-10-29 \
    --report-root /var/lib/af-nightly/reports --notify-again
```

Originals still queued or running when `measure` gave up are finished by running `measure` of
the night again; `systemctl start af-nightly-measure.service` does it for the night the build
unit last started and then reports and publishes it, as the chain does.

## Recovery

`systemctl list-timers af-nightly.timer` shows the next and the last start. A unit that failed
is read with `systemctl status af-nightly-<step>.service` and
`journalctl -u af-nightly-<step>.service --since yesterday`; `systemctl --failed` lists it, and
the next report counts it under Machine. Every step can be run again, by hand with `--date` or
with `systemctl start af-nightly-<step>.service`, which works on the night the build unit last
started (for the build itself, today's UTC date) and runs the rest of the chain after it:

| Step | Run again |
|---|---|
| build | Refuses a night that has `nightly.json`, so a night is never built twice. A directory without `nightly.json` is a refused attempt, which the next build replaces. A built night that is wrong is removed by hand (`rm -r /var/lib/af-nightly/builds/2026-10-29`) before it is published, then built again. A claim `2026-10-29.active` a killed build left is removed by hand once no build of the night runs. |
| measure | Resubmits idempotently: it prints the night's existing originals and cycles until they end. |
| compose | Writes a new directory each time, `<date>.<k>`. A refused compose ends the chain before the publish; rerun it once its cause is fixed (`systemctl start af-nightly-report.service`, which publishes after it). |
| publish | Amends the night ([Publishing](#publishing)); a skipped night's line is never sent twice. |

An original the executor left `uncertain`, a machine-lock fence or a torn journal publication
is the executor's to recover, under its rules, unchanged:
[Queue and recovery outcomes](performance-local-executor.md#queue-and-recovery-outcomes).
`measure` never clears a fence or replays an original.

## The daily report

`compose` writes one night's report from what the night left behind, without measuring,
building or publishing anything:

```sh
python3 -I -B scripts/nightly/report.py compose --config nightly.toml --date 2026-10-29 \
    --output-root /var/lib/af-nightly/reports
```

The directory is `--output DIR`, which must not exist, or a new one below `--output-root ROOT`
or, without either, below the configuration's `reports_root`: `ROOT/<date>` when it is free,
else `ROOT/<date>.<k>` with the smallest `k` of 2 or more that is (at most 99 directories a
night), each tried by an exclusive `mkdir`, so a second compose of the night never takes the
first one's directory. `compose` prints the directory it wrote on stdout, and on stderr the root
it defaulted to. It writes exactly three files into that directory, and nothing anywhere else:

- `daily.json`, `af.nightly-daily/1` (`schemas/performance-nightly-daily-v1.json`): the record
  every other file is drawn from.
- `report.html`, one self-contained page rendered by the laboratory's renderer
  (`perflab.render`): its Content-Security-Policy, its one hash-authorized style and script,
  every value escaped, no external reference: the page links nowhere, and the GitHub links are
  the digest's.
- `digest.md`, the Telegram digest: at most 3500 bytes of plain Markdown, the headline first,
  then what moved, the queue and the machine, the links as bare URLs on their own lines.

It runs as the account that owns the nightly executor's journal, from a checkout of the trusted
repository, and reads only:

1. `builds_root/<date>/nightly.json` and, for a built night, its two archives, whose size and
   digest must be the record's before the `af` inside each is hashed;
2. the nightly executor's journal under its configuration's `root`, read directly, never
   through the executor's service, and never locked or written: the originals keyed
   `nightly:<tag>:core` and `nightly:<tag>:telemetry` and the earlier nights' core originals;
3. the executor's `local-telemetry-history/history.json`, for the telemetry figures;
4. GitHub, through the same bounded reader as the build (`urllib.request`, 5 s to connect and
   30 s for each read, at most 16 MiB a response, no proxy or redirect, each request retried
   once);
5. three host facts: the free bytes of `builds_root`'s filesystem, whether a process holds the
   executor's machine lock (`/tmp/af-perf-machine.lock`, read from `/proc/locks`, never taken)
   and how many systemd units `systemctl --failed --plain --no-legend` lists.

An original is used only after it verifies the way `executor verify` binds it: its request is
the one the journal index names by digest, its state is `complete` and seals its result's
bytes, the result is this local original's (its request, sources, environment and producer),
and every evidence file is rehashed against the result's sealed list. The core original's
`plan.json` must be the plan its request froze, its sealed `run/builds.json` must belong to that
plan and name the very binaries the result's `binaries` hold, its `report/verdict.json` must
name that plan, and its `report/report.json` must carry that verdict and list every report
member with the hash the result sealed. Files are opened by fixed names, never by a path a
document supplies. An original under the night's tag is this build's evidence only when it
measured the night's `head_sha` against the record's baseline (`baseline_sha`, and the derived
`baseline_tag` when the request names one); the previous night's core original counts for the
verdict changes only when it measured the record's `previous_head_sha`. An original that fails
any check is refused for its section, with the reason, and never shown, so
`measured_is_shipped` is only ever computed from verified binaries.

### The sections

| Section | What it shows |
|---|---|
| Headline | The tag, the head with its subject, the version, the previous night, the build's baseline (the stable tag and commit, which a core original must have measured against), its publication date and the commits and pull requests since, the archives with the `af` inside each, and the verdict label. |
| Performance | The core verdict in the laboratory's words, the moved instances first with their baseline and candidate medians, relative change and interval (exact rationals in `daily.json`, three significant digits on the page), classification and outcome, the unchanged ones as a count, the sentinels' shards, the coverage, and whether the measured candidate `af` is the shipped x86_64 one (`measured_is_shipped`). The telemetry figures follow: each overhead and read-latency figure's median change in percent. |
| Trend | One plot per headline instance: the last `history_nights` earlier nights the journal holds a core original of, chosen by tag date however far apart, and this night, each night's own candidate median, in night order, a night whose baseline release changed marked in the title. Each point is that night's paired comparison; nights are never paired with each other. An earlier original that does not verify is listed with its reason instead of a point. |
| Since last night | The pull requests merged between the previous night's head and this one, and each instance whose verdict status changed since the previous built night. |
| Backlog | Open issues by label, issues opened and closed in the last 24 hours, stale issues (no update for 14 days), open pull requests with drafts and each one's mergeable state, and the head's check runs. The mergeable state takes one request per pull request, so at most 100 (`PULL_REQUESTS_MAX`) carry it, the lowest-numbered; with more open, `pull_requests_partial` and the page say so. |
| Machine | Free bytes, the machine lock, failed systemd units. |
| Completeness | One line per section: available, or why not. |

An instance has moved when its status is anything but `within_tolerance` or `non_regression`,
or its outcome is not a pass.

### Reading the label

The headline's label is the core verdict's result: `PASS`, `FAIL` or `INCOMPLETE`, followed by
the verdict's scope and evidence words (`bootstrap scope, synthetic evidence`), so a pass of a
partial or synthetic run never reads as a full measured one. `UNAVAILABLE` means there is no
verified core verdict for the night, and the headline says why: a skipped night (no new commits
reached main; it names the night it stands on), a core original that is queued, running,
failed or refused, or a journal that cannot be read. Every number is copied or recomputed from
validated inputs; whatever an input lacks is `null` in `daily.json`, `unavailable` on the page
and `?` in the digest, with the reason under `completeness`. GitHub being unreachable leaves
the backlog and the merged pull requests unavailable, never the night.

### Composing again

A night's report is written once into a new directory and never changed. A core result that
arrives after the report is composed is shown by composing the night again into another
directory, the next `<date>.<k>` below the root; the later report supersedes the earlier one,
and `publish --report-root` takes the newest ([Amending a night](#amending-a-night)). `compose`
exits 0 whenever the night has a record, whatever is unavailable, and 1 (`nightly: refused: …`
on stderr) when the night has no `nightly.json`, the configuration has no `[report]` table or is
malformed, the command line is malformed or names no directory while the configuration has no
`reports_root`, `--output` exists, or the root is no directory or already holds 99 directories
of the night; a refusal writes nothing.
