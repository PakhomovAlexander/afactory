#!/usr/bin/env bash
# Pin a published release using its verified signed digests, changing only .af/af.lock.
#
#   scripts/pin-release.sh X.Y.Z [--pr]
#
# Without --pr, prepare a clean checkout for a manual PR. --pr creates or reuses the one
# release/pin-vX.Y.Z branch and PR based on current main. It never pushes to main or force
# pushes. Merge the PR after CI passes. RELEASE_PIN_TOKEN must be a repository-scoped
# fine-grained PAT (Contents: write, Pull requests: write); GITHUB_TOKEN is not a substitute.
# Main moving before branch creation restarts preparation, at most three times. An existing
# branch on an older main is refused for human review/rebase; no second PR is created.
set -euo pipefail

usage() {
  sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
}

version=""
pr=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --pr) pr=1; shift ;;
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

if [[ "$pr" -eq 0 ]]; then
  if [[ "$(pinned)" == "$version" ]]; then
    echo "pin-release: $lock already pins af $version"
    exit 0
  fi
  refresh
  git --no-pager diff --stat
  exit 0
fi


# Require a credential whose pushes and PR events start CI. Never fall back to GITHUB_TOKEN,
# checkout's persisted credentials, an SSH bypass key, or the operator's credential helper.
[[ "${RELEASE_PIN_TOKEN:-}" == github_pat_* ]] || {
  echo "pin-release: --pr requires RELEASE_PIN_TOKEN, a fine-grained PAT with Contents and Pull requests write on this repository; GITHUB_TOKEN cannot trigger PR CI" >&2
  exit 1
}
export GH_TOKEN="$RELEASE_PIN_TOKEN"
export GH_HOST=github.com
repo="${GH_REPO:-$(gh repo view --json nameWithOwner -q .nameWithOwner)}"
# Bind API and Git publication to the same repository, before any Git network operation.
[[ "$repo" =~ ^[A-Za-z0-9-]+/[A-Za-z0-9_.-]+$ ]] || {
  echo "pin-release: expected GH_REPO=OWNER/REPO on github.com" >&2; exit 1;
}
remote_url="https://github.com/$repo.git"
origin="$(git config --get-all remote.origin.url || true)"
[[ "$origin" == "$remote_url" || "$origin" == "${remote_url%.git}" ]] || {
  echo "pin-release: origin must be credential-free HTTPS for GH_REPO on github.com; SSH, embedded credentials and other repositories are refused" >&2
  exit 1
}
# A URL rewrite could silently select SSH or another endpoint even for an explicit HTTPS
# URL. Scoped credential/HTTP settings could override the transport policy below. Refuse them
# rather than allowing ambient credentials to acquire publication authority.
if git config --get-regexp '^url\..*\.(insteadof|pushinsteadof)$|^credential\..*\.|^http\..*\.' >/dev/null; then
  echo "pin-release: URL rewrites or scoped credential/HTTP settings are configured; use a clean checkout for PAT-only publication" >&2
  exit 1
fi
branch="release/pin-v$version"
remote_git() {
  GIT_TERMINAL_PROMPT=0 GIT_ASKPASS=false git \
    -c protocol.allow=never -c protocol.https.allow=always -c http.followRedirects=false \
    -c http.extraheader= -c credential.helper= \
    -c 'credential.helper=!f() { echo username=x-access-token; echo password=$RELEASE_PIN_TOKEN; }; f' "$@"
}

# --state all prevents recreating a closed PR. A racing creator is re-read once on failure.
find_pr() {
  gh pr list --repo "$repo" --head "$branch" --state all \
    --json url,state,headRefOid,baseRefName --limit 2
}
pr_result() {
  python3 - "$1" "$2" <<'PYPR'
import json, sys
rows = json.loads(sys.argv[1])
if (len(rows) != 1 or rows[0]['state'] != 'OPEN'
        or rows[0]['baseRefName'] != 'main' or rows[0]['headRefOid'] != sys.argv[2]):
    sys.exit('pin-release: existing PR is closed, ambiguous or has another base/head; review it by hand')
print('pin-release: pin PR awaiting CI and human merge: ' + rows[0]['url'])
PYPR
}

for attempt in 1 2 3; do
  remote_git fetch -q "$remote_url" refs/heads/main:refs/remotes/origin/main
  git checkout -q --detach origin/main
  base="$(git rev-parse HEAD)"
  if [[ "$(pinned)" == "$version" ]]; then
    # Even the idempotent path checks every target digest.
    python3 scripts/af-pin.py --only-pin-moved HEAD "$version"
    echo "pin-release: $lock on main already pins af $version"
    exit 0
  fi
  python3 - "$(pinned)" "$version" <<'PYORDER'
import re, sys
def order(version):
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-rc\.(\d+))?", version)
    if not match:
        sys.exit('pin-release: main has an invalid release pin; review it by hand')
    major, minor, patch, rc = match.groups()
    return (int(major), int(minor), int(patch), rc is None, int(rc or 0))
if order(sys.argv[1]) > order(sys.argv[2]):
    sys.exit('pin-release: main pins newer af %s; refusing downgrade to %s'
             % (sys.argv[1], sys.argv[2]))
PYORDER
  prs="$(find_pr)" || { echo "pin-release: cannot inspect pin PRs; repository is not pinned" >&2; exit 1; }
  existing="$(remote_git ls-remote --heads "$remote_url" "refs/heads/$branch")"
  if [[ -n "$existing" ]]; then
    remote_git fetch -q "$remote_url" "refs/heads/$branch"
    git checkout -q --detach FETCH_HEAD
    [[ "$(git show -s --format=%P HEAD)" == "$base" ]] || {
      echo "pin-release: existing $branch is not based on current main; review/rebase its PR by hand; repository is not pinned" >&2
      exit 1
    }
    [[ "$(git diff --name-only "$base" HEAD)" == "$lock" ]] || {
      echo "pin-release: existing branch is not pin-only" >&2; exit 1;
    }
    python3 scripts/af-pin.py --only-pin-moved "$base" "$version"
    # A branch is reusable only if its bytes equal a fresh signed release refresh, not
    # merely because its digests look well-formed. Do not repair or rewrite a published ref.
    "$af" onboard --refresh-lock --af "$version" >/dev/null
    [[ -z "$(git status --porcelain)" ]] || {
      echo "pin-release: existing branch does not match the signed release refresh; review by hand; repository is not pinned" >&2
      exit 1
    }
  else
    [[ "$prs" == '[]' ]] || { echo "pin-release: a pin PR already exists without its branch; review it by hand" >&2; exit 1; }
    refresh
    git add "$lock"
    git commit -q -m "Pin af $version in .af/af.lock" \
      -m "Signed release digests; only the af pin moved. CI and human merge are required."
    remote_git fetch -q "$remote_url" refs/heads/main:refs/remotes/origin/main
    if [[ "$(git rev-parse origin/main)" != "$base" ]]; then
      echo "pin-release: main moved during attempt $attempt; starting over" >&2
      continue
    fi
    # The pin branch is the only authorized ref, even with ambient push.followTags=true.
    if ! remote_git push --no-follow-tags -q "$remote_url" "HEAD:refs/heads/$branch"; then
      echo "pin-release: pin branch push refused; repository is not pinned; rerun to inspect a racing creator" >&2
      exit 1
    fi
  fi
  # Main may have advanced while the branch push was in flight. Keep the single branch
  # for human review, but do not open a PR pretending it was prepared on the new main.
  remote_git fetch -q "$remote_url" refs/heads/main:refs/remotes/origin/main
  [[ "$(git rev-parse origin/main)" == "$base" ]] || {
    echo "pin-release: main moved after branch publication; review/rebase $branch by hand; repository is not pinned" >&2
    exit 1
  }
  head="$(git rev-parse HEAD)"
  if [[ "$prs" != '[]' ]]; then
    pr_result "$prs" "$head"
    exit $?
  fi
  body="Pin af $version using the published release's signed archive digests.

Only .af/af.lock's [af] and [af.digests] tables change. The refresh and only-pin-moved checks passed. The complete diff changes only this release pin, so default fast validation (functional, e2e, uix) applies. Perf-class validation is NOT RUN under fast; the separate Performance measurement retains its normal policy. Human merge after CI is required; publication alone has not pinned the repository.

<!-- af-pr-lines:v1 -->
<!-- /af-pr-lines -->"
  if ! gh pr create --repo "$repo" --base main --head "$branch" \
      --title "Pin af $version in .af/af.lock" --body "$body"; then
    prs="$(find_pr)" || { echo "pin-release: cannot create or inspect CI-triggering PR; repository is not pinned" >&2; exit 1; }
    if [[ "$prs" == '[]' ]]; then
      echo "pin-release: cannot create CI-triggering PR; check RELEASE_PIN_TOKEN permissions; repository is not pinned (branch retained for retry)" >&2
      exit 1
    fi
  else
    prs="$(find_pr)"
  fi
  pr_result "$prs" "$head"
  exit $?
done
echo "pin-release: main kept moving after three attempts; nothing published; repository is not pinned" >&2
exit 1
