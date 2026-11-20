# Nightly builds

The owner's Linux VM builds a signed nightly from the newest green main commit, verifies the
full native performance family, composes a daily report and publishes a scoped pre-release.
A deterministic [native release Scenario](#native-release-scenario) coordinates four asynchronous
phases, driven by a pinned daily Ant at 03:00 Europe/Moscow and external tick/worker services.
Models run only inside Tasks; this Scenario runs no Tasks or models. See the
[native release decision](adr/2026-11-16-orchestrate-native-nightly-releases.md).
The old four-unit chain is retired and ships no active clock or handover.

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

One TOML file, `af.nightly-config/2`; [`nightly.example.toml`](../scripts/nightly/nightly.example.toml)
is a commented copy to start from. Unknown keys refuse the file.

| Key | Meaning |
|---|---|
| `schema` | `"af.nightly-config/2"`. |
| `repository` | `owner/name` on GitHub, whose check runs decide which commit is green. |
| `mirror` | Absolute path of the bare mirror; the night reads `refs/heads/main` and the tags. |
| `builds_root` | Absolute path holding one directory per night and the Cargo cache; apart from `mirror`. |
| `build_image` | The musl build image, `sha256:<image ID>` or `name@sha256:<digest>`. |
| `qemu_aarch64` | The user-mode runner the aarch64 smoke runs under; a missing one refuses the build. |
| `sign_key` | The nightly minisign secret key file. |
| `public_key` | The matching minisign public key file; the signature must verify against it. |
| `required_checks` | Check-run names that must be present and green, beside every `check /` run. A release-workflow job is refused here. |
| `baseline_ref` | Optional reviewed release tag; omitted/null selects highest reachable stable. This VM uses `v0.15.1-rc.3`. |
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

Standalone legacy scripts still accept an explicit `--date YYYY-MM-DD`, `--night-of UNIT`,
or their historical UTC default. The native release worker always supplies its immutable tag's
explicit date; it never derives a fresh date from the process clock or a service restart.

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
3. Resolve the configured `baseline_ref` tag exactly and require it to be an ancestor of the
   chosen commit. When absent/null, use the highest reachable stable `vX.Y.Z` tag. Freeze both
   baseline tag and commit in the plan and build record; there is no fallback.
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
    publish.json               af.nightly-publish/2, written by each `publish` of the night
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
the directory it chose and the root it defaulted to on stderr. `daily.json` must be valid under
the `af.nightly-daily/1` contract, `report.html` is at most 4 MiB, and `digest.md` is UTF-8 text
of at most 3500 bytes. The report's `date`, `tag`, `repository` and `build.head_sha` must equal
the selected build record and configured repository, whether `--report` named the directory or
`--report-root` selected it; a valid report of another night refuses before any GitHub or
Telegram write. A missing, empty or oversized file refuses, and so does a built night with no
report directory under the root. A skipped night reads none of them.

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
no proxy is taken from the process environment, and only reads are retried, once.

**Amending.** Publishing a night again (a late measurement, a recomposed report) finds the
release by its tag. Before a PATCH or upload, that answer must name the selected tag and its
upload URL must name the same release id; create and update responses are bound the same way.
It must be a pre-release whose target commit is `head_sha`, and the archives, sums, signature
and `nightly.json` already on it must carry the night's sizes and digests: a difference refuses
before any request that writes, because shipped bytes never change. Then the notes, `daily.json` and `report.html` are replaced (each asset is deleted and
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

**The receipt**, `builds_root/<date>/publish.json`, is `af.nightly-publish/2`
(`schemas/performance-nightly-publish-v2.json`): `status` (`published` or `skipped`), `date`,
`tag`, `head_sha`, `repository`, `release` (`id` and `url`, or `null`), `assets` (each one's
`name`, `size` and `digest` as GitHub reports them, in upload order), `telegram` (the
`message_id`, or `null`) and `telegram_reason` (why there is no message, or `null`), `pruned`
(tags whose release and tag ref are both deleted), `prune_failures` (each `tag` whose release
was deleted but whose tag deletion failed, with its `reason`), `foreign` (tags),
`failures` (bounded structured `stage` and `reason` evidence for notification, pruning
and foreign-release failures), `amendments` (0 on the first publish) and `published_unix_ms`. The reader recomputes the tag,
the release URL and the asset names. A Telegram failure, a failed pruning request or a foreign
release after the release is published still writes the receipt, including failures while
listing releases or deleting a release. Version 1 receipts are refused; finish or discard
earlier state under the release that wrote it.

## Exit codes

These are `build.py`'s and `publish.py`'s; `measure.py`'s are under
[Native release Scenario](#native-release-scenario), `report.py`'s under [Composing again](#composing-again).

| Exit | Meaning |
|---|---|
| 0 | Built (`build`), or planned (`plan`, `build --only-plan` of a night that would build), or published with every step done (`publish`, a skipped night's line included). |
| 3 | Skipped: main's newest green commit is the previous night's (`build`, `build --only-plan`). |
| 1 | Refused, with the reason on stderr as `nightly: refused: …`; a malformed command line too. From `publish` also: published, with the receipt written, but the Telegram message or a pruning request failed (`nightly: failed: …`) or a foreign nightly-named release was left alone (`nightly: refused: …`). |

## Native release Scenario

The native release is deterministic and takes exactly one validated `nightly_tag` input,
`nightly-YYYY.MM.DD`. It has four Action/Wait pairs: build, full native comparison, report,
and publication. Actions acknowledge only durable queue admission within 30 seconds. The
separately supervised worker performs computation; waits verify the phase certificates.
There are no Tasks or model calls and both token budgets are explicitly zero.

The pinned daily Ant runs at **03:00 Europe/Moscow**, with overlap and missed occurrences
skipped. `scheduled_nightly_tag` derives its input from the scheduled local day before creating
the Run. Restart, a delayed tick and midnight rollover cannot replace it with today's date.
Daily and interval scheduling, Ant retention and machine authority otherwise retain their
existing behavior. An interval Ant cannot request this daily date projection.

The comparison uses the existing native release family: all core partitions, telemetry,
trace and CPU profile lanes. Every member must complete and verify; diagnostic lanes may be
explicitly not applicable according to the native verifier. A complete measured regression is
reported as FAIL and may publish. Missing, failed, synthetic, unverified or incomplete evidence
blocks publication and successful release completion. No narrower original or unrelated baseline
is substituted. These checks do not change the native profiles, oracles or execution bounds.

`baseline_ref` is an optional, explicit release tag in `af.nightly-config/2`. Omission (or JSON
null) retains highest reachable stable selection. For this VM set `baseline_ref = "v0.15.1-rc.3"`.
The highest stable `v0.15.0` lacks the current telemetry fixture's Pipeline v6 schema and is not
a complete comparison baseline. A missing or incompatible selected tag refuses; there is no
fallback. The build resolves the exact tag and commit once; later phases bind that sealed build,
the exact comparison sources and configuration digest, and the report's baseline tag and commit.
Publication's certificate retains the build, comparison and receipt digests. Before building,
the selected source must embed the installed authorized nightly public key; resumed records
and publication recheck that source identity. A green source with an older key is refused.

The durable worker does one phase/cycle per invocation under an exclusive lock. Native comparison
member keys reconcile retained originals across restart. A completed build record is recovered;
an interrupted build without one fails closed instead of starting another build. Publication
recovers only through a current-version receipt and read-only reconciliation of the exact
remote tag, head, release identity, notes and complete asset digests against the sealed signed
build and certified report. A failed receipt, a current tag recorded as pruned, or absent or
changed remote evidence refuses completion. An interrupted publication without a receipt
requires operator inspection; recovery never repeats create, upload, notification or deletion.
Standalone `publish` retains its explicit amendment and retention policy, including pruning
an explicitly selected old night; that outcome cannot satisfy native Scenario completion. Reports publish by directory rename. An interrupted pending
report/export is retained for operator inspection. Failed phases stop the release; they cannot
produce a satisfied Wait.

## Prepare and install offline

Use the reviewed, committed control checkout on this VM, for example `/opt/af-perf/control`,
owned by root with no worker write access. Its HEAD and installed `af --version --json` commit
must match the executor's exact `control_sha`. Every installed performance source/schema byte
must equal that producer; the nightly extension files and public trust root must also match the
same committed control. Install all authority from that checkout. Do not mix a new nightly script
with an older performance/schema tree, patch producer checks, or change accepted-main authority.
An uncommitted candidate cannot be installed as reviewed authority.

Prerequisites, checked before creating an installation, registration or enablement:

- Linux x86_64; Python 3, Git, Docker, minisign, Moscow tzdata and executable QEMU aarch64.
- The digest-pinned build images already present locally; the installer never pulls an image.
- A fetched trusted bare mirror shared by build and comparison, including the selected baseline.
- A full native local nightly executor (`core.tier = "nightly"`, empty `core.cases`), unchanged
  native bounds and at least 130 GiB free on the relevant filesystems (122 GiB native reserve
  plus build room). Native preflight still enforces its actual configuration and storage limits.
- The approved nightly key pair, public key ID `0596484DC93A8A62`, installed externally. Deployment
  tooling checks secret-file metadata only; it never reads, generates or recovers a private key.
- Same-worker owner-only mode 0600 configs, tokens and signing secret; mode 0700 installation and
  job-store parents with safe ancestors. The stable release key remains separate and unchanged.
- An installed native `af` matching `control_sha`, with Scenario/Ant support. Binary probes use
  isolated HOME/XDG directories and offline self-management.

The single worker account needs Docker and native performance access. Keep its config, executor
journal, builds, reports, installation and job state under private directories it owns. The
root-owned control is a read-only dependency, not the worker's config or job store. Do not run a
second executor service on the nightly executor configuration.

After reviewing machine paths and providing the external prerequisites, prepare concrete inactive
files as that worker account (choose a reviewed future start instant in milliseconds):

```sh
python3 -IB /opt/af-perf/control/scripts/nightly/install.py prepare \
  --config /var/lib/af-nightly/config/nightly.toml \
  --af /opt/af-perf/control/target/release/af --repo /opt/af-perf/control \
  --root /var/lib/af-nightly/jobs --state /var/lib/af-nightly/state \
  --output /var/lib/af-nightly/prepared --user af-nightly \
  --start-unix-ms 1794873600000
```

`prepared/INSTALL.txt` contains the exact install command and observed missing prerequisites.
`prepare` creates no keys, credentials, Ant or service. `install` refuses missing dependencies
before writing machine authority and requires absent installation/job roots; it never resets
receipts. Run the emitted install command as the worker, then review `service.json`, the pinned
launchers, `scenario.json`, `policy.json`, `ant.json` and systemd files. Every launcher pins its
source/config authority bytes; the tick launcher also pins the native binary. A changed install
requires new reviewed authority, never editing an active pin.

## The first night by hand

Run from a directory with no project release pin, with the worker's private HOME/XDG settings.
Use the installed binary; below `af` means that exact executable. Review each validation result
and substitute its exact revision for `PIN`:

```sh
af scenario validate --file /var/lib/af-nightly/native/scenario.json \
  --authority /var/lib/af-nightly/native/policy.json --json
printf '%s\n' '{"nightly_tag":{"type":"nightly_tag","value":"nightly-2026.11.17"}}' \
  > /var/lib/af-nightly/inputs.json
af scenario start nightly-manual --file /var/lib/af-nightly/native/scenario.json \
  --authority /var/lib/af-nightly/native/policy.json \
  --inputs /var/lib/af-nightly/inputs.json --confirm-pin PIN \
  --state /var/lib/af-nightly/state --execute --json
/var/lib/af-nightly/native/worker
af scenario resume nightly-manual --state /var/lib/af-nightly/state --json
```

Repeat worker/resume drives until the full comparison, report and publication finish. Inspect the
native comparison verification, daily report and publication receipt. A waiting Run is active;
never treat a queued Action, an exit code, or a missing family as a successful manual proof.

Before activating the Ant, stop and disable the old timer and every chain unit:

```sh
sudo systemctl disable --now af-nightly.timer
sudo systemctl stop af-nightly-build.service af-nightly-measure.service \
  af-nightly-report.service af-nightly-publish.service
sudo systemctl mask af-nightly.timer af-nightly-build.service af-nightly-measure.service \
  af-nightly-report.service af-nightly-publish.service
```

The shipped legacy timer/services are inert (`RefuseManualStart=yes`) and contain no schedule,
commands or handover. Install the four generated tick/worker files into `/etc/systemd/system`
and reload systemd. Then validate and register the Ant disabled:

```sh
af ant validate --file /var/lib/af-nightly/native/ant.json \
  --authority /var/lib/af-nightly/native/policy.json --json
af ant add --file /var/lib/af-nightly/native/ant.json \
  --authority /var/lib/af-nightly/native/policy.json --confirm-pin PIN \
  --state /var/lib/af-nightly/state --json
```

Only after the explicit manual proof and pin review, enable the Ant and the two external minute
timers. These commands are operator actions; the installer never executes them:

```sh
af ant enable nightly-release --state /var/lib/af-nightly/state --json
sudo systemctl enable --now af-nightly-ant-tick.timer af-nightly-worker.timer
```

The timers drive work; the pinned Ant owns the release clock. Tick and worker services conflict
with the retired chain, preventing simultaneous activation. Optional encrypted-key passphrases
use the worker unit's commented `LoadCredential` directive, never a secret in source or argv.

## Recovery

Inspect `af scenario show`, `af ant show`, the private job certificates and systemd journal.
Run the same worker and resume the same Scenario; do not generate a replacement tag or baseline.
Source and baseline commit IDs, config digests and predecessor certificates must continue to match.
A missing sealed build after an interrupted build, changed config, failed native family, or uncertain
publication requires operator diagnosis; automatic retries do not invent completion evidence.

The service retains at most 60 jobs and refuses new work when full; it does not delete provenance.
Its byte and wall limits remain explicit. Archive settled job evidence under operator policy before
preparing a new private installation. Native Ant retention separately compacts eligible old receipts
and Run bodies while protecting active work, exact accounting, recent/latest receipts and settlement
capacity. A retained completion is never a substitute for current native comparison verification.

## The daily report

The native release worker supplies the already offline-verified comparison aggregate and
telemetry member to the existing renderer. Its daily performance result names the comparison ID;
the full comparison bundle retains all core partitions and diagnostic evidence. Standalone
`compose` retains its original nightly-journal input path for explicit diagnostics; it cannot
replace the native release's full-family evidence or authorize native publication.

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

The new directory's device and inode are recorded at creation and checked again after its path
is reopened. Files are created exclusively by descriptor below that exact directory without
following links, so replacing the path with either a symlink or another real directory refuses
the compose and writes no file into the substituted directory.

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
and `publish --report-root` takes the newest ([Publishing](#publishing)). `compose`
exits 0 whenever the night has a record, whatever is unavailable, and 1 (`nightly: refused: …`
on stderr) when the night has no `nightly.json`, the configuration has no `[report]` table or is
malformed, the command line is malformed or names no directory while the configuration has no
`reports_root`, `--output` exists, or the root is no directory or already holds 99 directories
of the night; a refusal writes nothing.
