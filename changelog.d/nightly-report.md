- The owner's daily nightly report: `scripts/nightly/report.py compose --config nightly.toml
  --date YYYY-MM-DD --output DIR` writes `daily.json` (`af.nightly-daily/1`), a self-contained
  `report.html` and a Telegram `digest.md` of at most 3500 bytes from the night's
  `nightly.json`, the nightly executor's verified core and telemetry originals (bound to the
  night's head and baseline, the measured binaries to the original's sealed builds record), the
  trend of up to four headline instances over the last `history_nights` earlier nights, GitHub's
  backlog and three machine facts. Every missing or refused input leaves its section unavailable
  with the reason; only a night without `nightly.json` is refused. `nightly.toml` gains a
  `[report]` table, and `build.py` and `report.py` share one bounded GitHub reader on
  `urllib.request`, whose timeout is now 30 s for the connection as for each read
  ([ADR-0271](docs/adr/0271-compose-the-nightly-report-from-the-nights-record-and-the-executor-journal.md),
  [`docs/nightly.md`](docs/nightly.md#the-daily-report)).
