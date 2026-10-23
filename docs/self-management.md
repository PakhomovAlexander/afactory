# Self-management: the release check

`af` manages its own installation (`af self`,
[ADR-0044](adr/0044-af-manages-itself-and-dispatches-to-the-pinned-release.md)) and checks for
newer releases after commands
([ADR-0154](adr/0154-check-for-updates-after-headless-and-json-commands.md)). This page says how
that check behaves: when it runs, what bounds it, what it records and what `af self status`
prints from it. `af help self` has the whole `[self]` policy.

## When the check runs

- After an eligible command (any but `help`, `--version`, `af self`, `af config`, `completions`
  and the browser) `af` spawns a detached `af self refresh-check` when the check is due. The
  child holds none of the command's pipes and never changes its output or exit status.
- `af self status` schedules the same detached check when it is due, so the `last check` it
  prints never stands still on a machine where only status runs.
- `af self status --refresh` runs the check in its own process, before it prints, whether or not
  it is due.
- `af self update --check` asks the release source at once and exits 10 when a newer release
  exists, 0 when none does and 1 with the reason when the source cannot be asked.
- `[self] check_every` (24 hours by default) counts from the last attempt, answered or not: an
  outage costs one query per `check_every`. `CI`, `AF_SELF_OFFLINE=1`, `[self] update_check =
  false` and `auto_update = "never"` turn the check off. Offline, `af self update --check` prints
  `offline: check skipped` and exits 0, and `af self status --refresh` says it skipped the check.

## What bounds it

Releases come anonymously over HTTPS with `curl` from a public repository; the GitHub CLI is
asked, under your own `gh` login, only when that route is refused (HTTP 401, 403 or 404) or
`curl` is missing. Neither can stall the check:

- `curl` runs with `--connect-timeout 5 --max-time 30` to list releases (600 s to download an
  asset), its retries within the same limit, and `af` ends its process group if it outlives
  them.
- `gh` runs under a 30 s deadline to list releases.
- When the anonymous route fails, the reason comes first, the HTTP status or `curl`'s own error,
  then what `gh` added, and only then a `gh auth login` hint for a repository that is not
  public. A public repository never needs `gh`; neither does `install.sh`.

The check holds `$XDG_CACHE_HOME/af/self/latest.lock` while it runs and stamps in it when it
started. A check that has held the lock for more than an hour counts as no check: the cache does
not move, and `af self status` and `af self update --check` report it with the command that
clears it:

```text
check:      a release check has been running since 2026-09-12T08:00:00Z; fix: rm ~/.cache/af/self/latest.lock
```

## What it records

The cache, `$XDG_CACHE_HOME/af/self/latest.toml`, is `af/self-check@3`
([schema](../schemas/self-check-v3.json)):

- `checked_at_epoch`: when a check last answered;
- `attempted_at_epoch`: when one was last attempted, answered or not;
- `last_error`: why the last attempt failed, one line of at most 512 bytes, absent once one
  succeeds;
- `latest`: the newest release on the configured channel;
- `latest_prerelease`: the newest pre-release, asked for whatever the channel while the running
  `af` is itself a pre-release or the channel is `rc`;
- `auto_updated`: the activation a background check made under `auto_update = "always"`.

A cache an earlier release wrote, of any version, is not read: that machine is due a check,
which writes this one ([ADR-0145](adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).

## What status prints

```text
last check: 2026-10-20T09:12:44Z · latest 0.13.0 · last attempt 2026-10-23T08:00:02Z failed: curl https://api.github.com/repos/…: curl did not answer within 70 s and was stopped — fix: check the network and retry, or set AF_SELF_OFFLINE=1 to skip release checks
```

`latest X` alone is printed only when the running `af` is a stable release no newer than X. A
pre-release user sees both, and the command when a newer pre-release waits:

```text
last check: 2026-10-23T08:00:02Z · latest stable 0.11.0 · latest pre-release 0.12.0-rc.5 (you run 0.12.0-rc.3: af self update --rc)
```

A release older than the running one is always qualified (`latest stable 0.12.0` under a running
0.12.1), never shown as simply the latest. The notice after commands names the same command: `af
0.12.0-rc.5 is available (running 0.12.0-rc.3) — af self update --rc`.

`af self status --json` prints one `af/self-status@1` document
([schema](../schemas/self-status-v1.json)) with `last_check`, `last_attempt`, `last_error`,
`latest`, `latest_prerelease` and `check_stalled_since` beside the installation, the pin and
the policy.
