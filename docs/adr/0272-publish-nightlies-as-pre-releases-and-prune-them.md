# ADR-0272: Publish nightlies as pre-releases and prune them

**Status:** proposed (2026-10-30).

## Context

[ADR-0265](0265-build-nightlies-from-the-newest-green-main-on-the-owners-vm.md) builds a signed
nightly on the owner's VM into `builds_root/<date>/` and publishes nothing; the daily report
step composes a directory holding `daily.json`, `report.html` and `digest.md`. Nobody can
install a nightly or read its report until something puts both where the owner and users find
them, and the owner wants one short message a night saying what happened. A release on GitHub
is where `install.sh` and `af self` already look, so a nightly there must never look like a
release, and the release train
([ADR-0045](0045-one-release-train-and-a-pin-that-binds-bytes.md)) must stay exactly as it is.
One nightly a night must not grow the repository's releases without bound.

## Considered options

- Publish with the `gh` CLI: rejected. It is one more tool on the VM, it reads its token from
  its own configuration or the environment, and a test could only fake it as a process; REST
  through `urllib` lets the tests inject the transport as `test_nightly_build.py` injects its
  GitHub.
- Use the build's read-only token, or take tokens from arguments or the environment: rejected.
  The build needs no write access, and an argument or variable reaches process listings and
  child processes; each token is a file the configuration names.
- Publish nightlies as full releases, or as drafts: rejected. A full release is what the stable
  ranking reads, and a draft is invisible to anyone installing.
- Re-upload every asset when a night is published again: rejected. The archives, sums and
  signature are the shipped bytes; replacing them would let one tag name two different
  binaries over time.
- Send the digest as Markdown: rejected. A Telegram Markdown parse failure loses the whole
  message, and the digest is written for people, not for the parser.
- Prune by date of creation, or every release older than a fortnight: rejected. Only a tag
  matching the nightly pattern on a pre-release is ours to delete; anything else stays.

## Decision

`scripts/nightly/publish.py publish --config nightly.toml --date YYYY-MM-DD --report DIR
[--notify-again]` (stdlib Python, `python3 -I`) reads the `af.nightly-config/1` file, whose
`[publish]` table names `github_token_file` (a token with contents write, separate from the
read-only `github_token_file`), optionally `telegram_token_file` and `telegram_chat_id`
together, and `keep_nights` (3 to 60, 14 by default). [`docs/nightly.md`](../nightly.md) is the
operator page.

1. **Verify before writing.** For a `built` night `publish` checks the report directory
   (`daily.json` is valid under the `af.nightly-daily/1` contract, `report.html` at most
   4 MiB, `digest.md` UTF-8 text of at most 3500 bytes; a missing or oversized file refuses).
   The report must belong to this exact night: its `date`, `tag`, `repository` and
   `build.head_sha` equal the selected build record and configured repository, whether the
   directory came from `--report` or was the newest selected under `--report-root`. A valid
   report of another night therefore refuses before any GitHub or Telegram write. `publish`
   also checks the night as `build` left it: every archive has the digest and size `nightly.json`
   records, `dist/SHA256SUMS` is exactly the release-format sums of those archives, and
   `minisign -V -q -m SHA256SUMS -x SHA256SUMS.minisig -p <public_key>` (the call `build`
   makes, through an injectable runner) verifies with the night's trusted comment. The tokens
   are read too. Nothing is sent before all of it passes.
2. **One pre-release per night.** The release of tag `nightly-YYYY.MM.DD` at the night's
   `head_sha` is created through the REST API as a pre-release named `Nightly YYYY-MM-DD`, never
   marked latest, with `digest.md` as its notes, and receives, in this order, the two archives,
   `SHA256SUMS`, `SHA256SUMS.minisig`, `nightly.json`, `daily.json` and `report.html` through
   its `upload_url`. A tag that already names another commit refuses. Each asset's size and
   digest as GitHub reports them must equal the file's. The transport is shared with `build`
   (`scripts/nightly/githubrest.py`): JSON requests to api.github.com, uploads only to
   uploads.github.com under the repository's releases, bounded responses, 60 s per request,
   no redirect followed, only reads retried once.
3. **Amending never changes shipped bytes.** A later `publish` of the night finds the release
   by its tag and refuses, before any request that writes, unless it is a pre-release that
   targets `head_sha` and every archive, sum, signature and record already on it carries the
   night's size and digest. It then replaces the notes, `daily.json` and `report.html` (the
   asset is deleted and uploaded again), uploads any of the seven assets an interrupted
   publish left out, sends no second Telegram message unless `--notify-again`, and counts the
   amendment in the receipt. A publish that died before its receipt is completed by its
   retry, which sends the digest: once when the first run died during an upload, a second
   time when it died after the message, since the receipt is the only memory of one.
4. **Telegram in plain text.** With the bot configured, `digest.md` is sent as one
   `sendMessage` without a parse mode and with the link preview off. A skipped night publishes
   no release and uploads nothing; it sends the one line `nightly-YYYY.MM.DD: no new commits on
   main since <previous_tag>`, once: a later publish of the night sends nothing, even with
   `--notify-again`. Without the bot nothing is sent and the receipt says why.
   A failed message leaves the release in place: the receipt is written with the reason and
   `publish` exits 1.
5. **Prune only our own.** Of the repository's releases whose tag matches
   `^nightly-[0-9]{4}\.[0-9]{2}\.[0-9]{2}$` and that are published pre-releases (not drafts),
   those beyond the newest `keep_nights` by tag date are deleted, oldest first, release first
   and then its tag ref. The night just published has no exemption: when it is older than the
   newest `keep_nights`, its release goes too and the receipt lists its tag. A tag is recorded
   as pruned only once its tag ref is deleted; a release deleted whose tag deletion failed
   stops pruning, is recorded with the reason in `prune_failures`, and `publish` exits 1. A
   nightly-named release that is a draft or not a pre-release is foreign: it is never deleted,
   it is named in the receipt and on stderr, and `publish` exits 1. No other release or tag is
   ever touched.
6. **The receipt.** `builds_root/<date>/publish.json` is `af.nightly-publish/1`, a closed
   contract of the laboratory with a generated schema and golden fixtures: the status
   (`published` or `skipped`), the release id and URL, each asset's name, size and digest as
   GitHub reports them, the Telegram message id or `null` with the reason, the pruned tags,
   the tags whose release went but whose tag deletion failed with the reason, the foreign
   tags, the number of amendments and `published_unix_ms`. Its reader recomputes the
   tag, the release URL and the asset names. `publish` prints the release URL; it exits 0
   when everything succeeded and 1 when it refused (no receipt) or published with a failed
   message, a failed pruning request or a foreign release (receipt written). A night without
   `nightly.json` refuses.

Tokens are never printed, logged, written to the receipt or passed as an argument.

## Consequences

The owner can install a nightly from GitHub and read its report there, and gets one message a
night. `scripts/release.sh`, `release.yml`, `install.sh`, `scripts/pin-release.sh`,
`scripts/resolve-release.sh` and `.af/af.lock` are unchanged: they already ignore `nightly-…`
tags, and `test_nightly_build.py` keeps proving it. `build.py` keeps its plan and build
behaviour; its GitHub reader moved into `githubrest.py` unchanged. A night whose publish
failed half-way is published again by running the same command: the release is amended and
completed, and its digest sent then, once. The systemd chain that runs build, measurement, report and publish in turn is a
later change.

`scripts/performance/tests/test_nightly_publish.py` proves, against an in-memory GitHub, a
Telegram recorder and a fake minisign: a first publish, an amendment that replaces only the
report, a refused amendment of changed shipped bytes with no write request, a partial publish
completed with its one message, a skipped night's one line sent once even with
`--notify-again`, pruning beside foreign, draft and non-pre-release releases, an older night
pruned with the rest so exactly the newest `keep_nights` stay, a failed tag deletion recorded
apart from the pruned tags,
Telegram unconfigured and failing, a signature that does not verify, and missing, oversized
and malformed report files.
