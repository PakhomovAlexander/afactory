- The owner's nightly publish step: `scripts/nightly/publish.py publish --config nightly.toml
  --date YYYY-MM-DD --report DIR` verifies a built night (archive digests, `SHA256SUMS`, its
  minisign signature), publishes it as the GitHub pre-release `nightly-YYYY.MM.DD` at the
  night's commit with the two archives, `SHA256SUMS`, `SHA256SUMS.minisig`, `nightly.json`,
  `daily.json` and `report.html`, sends the report's `digest.md` as one plain-text Telegram
  message, prunes the nightly pre-releases beyond the newest `keep_nights` (14 by default),
  the published night included when it is that old, without touching drafts or any other
  release or tag, and writes the receipt `af.nightly-publish/1`.
  Publishing a night again replaces only its notes and report and never its shipped bytes; a
  skipped night sends one line, once, and publishes nothing. `nightly.toml` gains a `[publish]` table
  naming a separate write token, the optional Telegram bot and chat, and `keep_nights`
  ([ADR-0271](docs/adr/0271-publish-nightlies-as-pre-releases-and-prune-them.md),
  [`docs/nightly.md`](docs/nightly.md#publishing)).
