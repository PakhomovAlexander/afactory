#!/usr/bin/env bash
# Pin published release X.Y.Z in this repository's own .af/af.lock (ADR-0138), so the repository
# always runs its newest release. The release workflow runs it with --push right after publishing;
# by hand, it updates the lock in a clean working tree for a pull request.
#
#   scripts/pin-release.sh X.Y.Z [--push]
#
# `af onboard --refresh-lock --af X.Y.Z` writes the pin: it runs under that release, installing it
# on demand against the signed SHA256SUMS, and records every target's archive digest. Any other
# change the refresh would make is refused, because it needs a reviewed pull request. --push
# commits on the tip of origin/main and pushes, starting over when main moves meanwhile.
set -euo pipefail

usage() {
  sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
}

version=""
push=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --push) push=1; shift ;;
    -h|--help) usage ;;
    -*) echo "pin-release: unknown option $1" >&2; usage ;;
    *) [[ -z "$version" ]] || usage; version="${1#v}"; shift ;;
  esac
done
[[ -n "$version" ]] || usage
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ ]] || { echo "pin-release: $version is not X.Y.Z or X.Y.Z-rc.N" >&2; exit 2; }

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"
af="${AF:-af}"
lock=.af/af.lock

pinned() {
  sed -n '/^\[af\]$/,/^\[/s/^version = "\(.*\)"$/\1/p' "$lock" | head -1
}

# Rewrite the pin in the clean checkout at HEAD, and prove nothing else moved.
refresh() {
  "$af" onboard --refresh-lock --af "$version" >/dev/null
  local changed
  changed="$(git status --porcelain)"
  if [[ "$changed" != " M $lock" ]]; then
    echo "pin-release: the refresh changed more than $lock:" >&2
    echo "$changed" >&2
    exit 1
  fi
  python3 scripts/af-pin.py --only-pin-moved HEAD "$version"
}

[[ -z "$(git status --porcelain)" ]] || { echo "pin-release: the working tree is not clean" >&2; exit 1; }

if [[ "$push" -eq 0 ]]; then
  if [[ "$(pinned)" == "$version" ]]; then
    echo "pin-release: $lock already pins af $version"
    exit 0
  fi
  refresh
  git --no-pager diff --stat
  exit 0
fi

for attempt in 1 2 3; do
  git fetch -q origin main
  git checkout -q --detach origin/main
  base="$(git rev-parse HEAD)"
  if [[ "$(pinned)" == "$version" ]]; then
    echo "pin-release: $lock on main already pins af $version"
    exit 0
  fi
  refresh
  git add "$lock"
  git commit -q -m "Pin af $version in .af/af.lock" \
    -m "The repository runs its newest release (ADR-0138); written by scripts/pin-release.sh."
  if refusal="$(git push -q origin HEAD:refs/heads/main 2>&1)"; then
    echo "pin-release: pushed $(git rev-parse --short HEAD) to main, pinning af $version"
    exit 0
  fi
  git fetch -q origin main
  if [[ "$(git rev-parse origin/main)" == "$base" ]]; then
    echo "pin-release: main refused the push:" >&2
    echo "$refusal" >&2
    echo "pin-release: run scripts/pin-release.sh $version and open a pull request" >&2
    exit 1
  fi
  echo "pin-release: main moved during attempt $attempt; starting over" >&2
done
echo "pin-release: main kept moving; run scripts/pin-release.sh $version and open a pull request" >&2
exit 1
