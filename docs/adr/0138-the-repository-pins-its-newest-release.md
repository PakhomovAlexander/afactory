# ADR-0138: The repository pins its newest release

**Status:** accepted (2026-10-04). Extends
[ADR-0045](0045-one-release-train-and-a-pin-that-binds-bytes.md): the release train ends by
pinning the release it published in this repository's own `.af/af.lock`.

## Context

`af` run inside a repository dispatches to the release that repository's `.af/af.lock` pins
(ADR-0044). The pin moves only when someone runs `af onboard --refresh-lock --af VERSION` and
commits the result. Nobody did for this repository: on 2026-10-04 its lock still pinned
`0.9.0-rc.6` while `0.10.0` was published. Every `af review` and `af task` run here therefore ran
a release six versions behind. One visible symptom: a review of PR #156 failed at Provider
admission because rc.6 gives the Codex subscription probe 5 seconds, a budget that `0.10.0`
raised to 15 seconds for exactly that failure under load.

The release pull request cannot carry the pin. A pin binds the archive digest of every target,
copied from the release's signed `SHA256SUMS`, and those exist only after the workflow has built,
signed and published the release.

## Decision

- **The release workflow pins what it published.** A `pin` job runs after `publish`. It installs
  the published release with `install.sh`, which checks the archive against the signed
  `SHA256SUMS`, and runs `scripts/pin-release.sh VERSION --push`. The script refreshes the lock
  with `af onboard --refresh-lock --af VERSION` on the tip of `main`, then commits and pushes
  that one change. If `main` moves first, it starts over, up to three attempts.
- **Only the pin may move.** The script refuses when the refresh changes any file but
  `.af/af.lock`, any lock table but `[af]` and `[af.digests]`, pins another version, or leaves
  a released target without a digest. A drifted Worker or Pipeline pin needs a reviewed pull
  request, not a bot commit.
- **`make check` keeps the pin current.** `scripts/af-pin.py --check` fails unless the lock pins
  the newest release in `CHANGELOG.md`, or the one before it while the newest is in flight (the
  release pull request, and the minutes until its `pin` job lands). It also requires a digest
  for every target that `release.yml` builds. A failed `pin` job therefore blocks the next
  release pull request instead of letting the lock fall further behind.
- **By hand, the same script.** `scripts/pin-release.sh VERSION` without `--push` rewrites the
  lock in a clean working tree for a pull request.

## Consequences

- The pin commit is pushed with the workflow's token, so it starts no workflow run of its own.
  Its only change is verified by the script, and the next pull request's `make check` covers
  it.
- The push needs `main` to accept direct pushes from the workflow. If a branch ruleset later
  requires pull requests on `main`, the `pin` job fails with git's refusal and says to run the
  script by hand; the ruleset would need a bypass for GitHub Actions to keep the pin automatic.
- A release whose publish fails leaves its `CHANGELOG.md` section unpinnable. The next release
  pull request then fails the check until that section is corrected.

## Considered options

- **Pin the previous release in each release pull request.** Hermetic and reviewed, but the lock
  would always trail the published release by one, which is the drift this decision removes.
- **Open a pull request from the workflow.** Repository settings keep GitHub Actions from
  creating pull requests, and a pull request opened with the workflow token would start no checks
  either. It would also add a second human act to a release that ADR-0045 keeps at one.
- **Check against the latest published release online.** `make check` also runs inside offline
  gates and Task sandboxes, so it reads only `CHANGELOG.md` and the workflow, both in the tree.
