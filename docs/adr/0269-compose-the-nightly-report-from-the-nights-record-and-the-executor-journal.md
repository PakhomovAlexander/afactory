# ADR-0269: Compose the nightly report from the night's record and the executor journal

**Status:** proposed (2026-10-30).

## Context

The owner's nightly train builds a nightly on the Claw VM
([ADR-0265](0265-build-nightlies-from-the-newest-green-main-on-the-owners-vm.md)) and measures it
as two originals of the independent executor, `nightly:<tag>:core` and
`nightly:<tag>:telemetry`
([ADR-0266](0266-measure-nightlies-as-originals-of-the-independent-executor.md)). Nothing turned
those two inputs into the one page and the one message the owner reads in the morning. The
publisher that posts them (a GitHub pre-release, Telegram) is a later step; it needs files to
publish, written by a step that publishes nothing itself.

## Considered options

1. Ask the executor's service for its status and render that. Rejected: the service reports
   queue states, not verified evidence, and the journal is on the same host (ADR-0240), so a
   second reader adds a dependency without adding trust.
2. Refuse a night whose measurement is missing or late. Rejected: the publisher runs on a
   schedule, and a refusal would publish nothing at all for a night that was built, skipped or
   simply measured late. Only a night without its build record has nothing to report.
3. Infer a figure an input lacks (a baseline median from the previous night, a pull request
   from a commit message). Rejected: the report would claim more than its evidence holds.
4. Compose a closed record from the night's record and the verified journal, render the page and
   the digest from that record alone, and mark every missing input unavailable with its reason.
   Chosen.

## Decision

`python3 -I -B scripts/nightly/report.py compose --config nightly.toml --date YYYY-MM-DD
--output DIR` reads the night's `builds_root/<date>/nightly.json` (`af.nightly-build/1`), the
nightly executor's journal (the `root` of the configuration `[report].executor_config` names),
its `local-telemetry-history/history.json`, GitHub and three host facts, and writes into the new
directory `DIR` exactly `daily.json`, `report.html` and `digest.md`, and nothing anywhere else. It
never pushes, publishes, posts, deletes, runs Docker or cargo.

1. **Configuration.** `af.nightly-config/1` gains an optional `[report]` table:
   `executor_config` (absolute path), `history_nights` (1 to 60, default 14) and
   `headline_instances` (one to four instance IDs as the suite names them). `build.py` ignores
   it; `compose` refuses a configuration without it. The top-level `github_token_file` serves
   both scripts.
2. **The record.** `af.nightly-daily/1` is a closed contract in `perflab.nightlycontracts` with
   `schemas/performance-nightly-daily-v1.json` and indexed valid and invalid fixtures. It holds
   the headline (label `PASS`, `FAIL`, `INCOMPLETE` or `UNAVAILABLE` and its words), the build,
   the baseline, the performance, telemetry, trend, since-last-night, backlog and machine
   sections and one completeness entry per section. A number is an exact rational with its
   three-significant-digit text, and the reader recomputes the text from the rational. The
   reader also requires the label to be the verdict's result (or `UNAVAILABLE` without one), a
   skipped night to have no performance, telemetry, trend or since-last-night section, each
   machine fact to be a value or `null` with its reason, and `measured_is_shipped` to compare
   the measured candidate's digest with the shipped x86_64 `af`'s, or be `null` with the reason.
3. **Verified journal evidence.** The composer reads the journal directly, takes no lock and
   writes nothing there. An original is used only when its request is the one the journal index
   binds by digest, its state is `complete` and seals its result's bytes, the result is this
   local original's (request digest, sources, backend, status, environment and producer
   digests, no Actions receipt), and `executor.file_manifest` over its evidence equals the
   result's sealed list, every file rehashed. The core original's `plan.json` must be the plan
   its request froze and name its sources; its sealed `run/builds.json` must belong to that plan
   and its `executor.built_binaries` must equal the result's `binaries`, as `executor verify`
   requires, so `measured_is_shipped` is computed only from verified binaries;
   `report/verdict.json` must name that plan's run and digest; `report/report.json` must carry
   that verdict, name those sources and list every report member with the hash and size the
   result sealed. Each file is opened by a fixed name below the original, without following a
   symlink, and accepted only with its sealed hash, so no path a document supplies selects what
   is read. A tag alone does not make an original this build's evidence: the night's core and
   telemetry originals must have measured the record's `head_sha` against its `baseline_sha`
   (and, when the request derives one, its `baseline_tag`), and the previous night's core
   original counts for the verdict changes only when it measured the record's
   `previous_head_sha`. A refused original leaves its section unavailable with the reason and
   is never shown.
4. **Honest states.** A skipped night composes the short report: no new commits reached main,
   and it stands on the previous night it names. A night whose core original has no result, or
   one refused, composes with its performance section unavailable and exits 0; a later compose
   into another directory shows a late result. A missing `nightly.json` is the only refusal of
   an input (exit 1, naming the path, nothing written). GitHub unreachable, or answering another
   shape than asked, leaves the backlog, the merged pull requests, the head's subject and the
   baseline's date and counts unavailable, never the night. A host fact that cannot be read is
   `null` with its reason.
5. **The trend.** For each headline instance, one point for each of the last `history_nights`
   earlier nights the journal holds a core original of, chosen by tag date however far apart
   (not a calendar window), and one for this night when its bound core original verifies, in
   night order, each the night's own candidate median; an earlier original that does not
   verify is listed with its reason instead of a point. A point whose baseline release differs
   from the previous point's is marked with that release. Points are never paired across nights
   ([ADR-0172](0172-admit-candidate-files-through-bounded-descriptors-and-report-archive-and-trend-runs-offline.md)).
   A headline instance that is no instance of the night's verdict leaves the trend unavailable
   with that reason.
6. **The page and the digest.** `report.html` is drawn by `perflab.render` alone (its CSP, its
   hash-authorized style and script, every value escaped, no external reference), each section
   from its own part of `daily.json`, so the page and the record cannot disagree: the headline,
   the performance table (moved instances first, the unchanged as a count, the sentinel line),
   the trend plots, since last night, the backlog, the machine and the completeness table.
   Every count stands in a table cell of its own. `report.py` holds no HTML: each element,
   headings and the verdict line included, comes from a `perflab.render` helper.
   `digest.md` is the same content compressed into at most 3500 bytes: the headline first, the
   moved instances (cut with a count when they do not fit), the counts, the queue and the
   machine line, then the links as bare URLs on their own lines.
7. **One GitHub reader.** The bounded anonymous-or-token reader of `build.py` moves to
   `scripts/nightly/nightlygithub.py` and opens each GET with `urllib.request`: 5 s to connect
   and 30 s for each read (the opener's HTTPS connection bounds the connect apart from the
   reads, as the build's reader did), at most 16 MiB per response, no proxy from the
   environment and no redirect followed, so a request and
   its token reach api.github.com only, each request retried once on a failure or a 5xx. Tests
   inject the opener. It gains one list walk for the report, bounded to 20 pages of 100, more
   items a failure rather than a shortened list. The backlog asks each open pull request for
   its mergeable state, one request each, for at most `PULL_REQUESTS_MAX` (100) of them, the
   lowest-numbered; with more open, `pull_requests_partial` records the partial read and its
   reason.
   `build.py` keeps its behaviour and refuses the night on a failure; `report.py` marks the
   section unavailable. The machine facts come from one injectable host: `os.statvfs` of
   `builds_root`, `/proc/locks` for the executor's machine lock (`/tmp/af-perf-machine.lock`,
   never taken, so a report never makes a cycle busy; a `/proc/locks` longer than its 1 MiB
   read bound cannot show the lock free, so the fact is then `null` with that reason) and
   `systemctl --failed --plain --no-legend`.

## Consequences

[`docs/nightly.md`](../nightly.md) gains the report: the `[report]` keys, the inputs, the
sections, how to read each label and how to compose again, and
[`nightly.example.toml`](../../scripts/nightly/nightly.example.toml) shows the table. The
composer runs as the account that owns the nightly journal. Publication of the three files,
scheduling and systemd units are later steps; nothing here changes the build's plan or build
behaviour, the executor, the release train or `.af/af.lock`.

`scripts/performance/tests/test_nightly_report.py` proves the command through `main` with a
fake GitHub transport and a fake host on a journal built from the laboratory's schema
fixtures: the three nights' `daily.json` and `digest.md` equal their goldens, side by side under
`fixtures/performance/schema/nightly-daily/` (each `digest-*.md` named by its `valid-*.json`
entry's `digest` key in `index.json`, so the schema directory's every file stays indexed), the
page by structure (its sections in order, the lab's CSP, style and script only, the renderer's
elements only, no event attribute or external reference, every table cell equal to the record's
value, and a changed number fails it), evidence with HTML metacharacters escaped, the refusals
of a verdict of another plan, a report member with another hash, evidence changed after its
seal, an original of another candidate or baseline and result binaries other than its builds
record's, the trend's selection by tag date, every open pull request's mergeable state, a
`/proc/locks` beyond its bound, GitHub and host unavailability and the digest's bound. It runs
no network, Docker or cargo.
