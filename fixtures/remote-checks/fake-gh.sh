#!/bin/sh
# A fake `gh` serving recorded GitHub API documents; it never contacts a network. It simulates
# GitHub's pull request and its `refs/pull/12/merge` test merge in the local bare repository.
STATE='@STATE@'
BARE='@BARE@'
TASK='@TASK@'
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
export GIT_AUTHOR_NAME=github GIT_AUTHOR_EMAIL=github@example.invalid
export GIT_COMMITTER_NAME=github GIT_COMMITTER_EMAIL=github@example.invalid
printf '%s\n' "$*" >> "$STATE/calls.log"
head_sha() { git --git-dir="$BARE" rev-parse --verify -q "refs/heads/af-gate/$TASK/head"; }
base_sha() { git --git-dir="$BARE" rev-parse --verify -q "refs/heads/af-gate/$TASK/base"; }
merge_ref() {
  H=$(head_sha); B=$(base_sha)
  [ -n "$H" ] && [ -n "$B" ] || return 0
  MODE=$(cat "$STATE/merge-mode" 2>/dev/null || echo good)
  case "$MODE" in
    good) T=$(git --git-dir="$BARE" rev-parse "$H^{tree}")
          M=$(echo merge | git --git-dir="$BARE" commit-tree "$T" -p "$B" -p "$H");;
    other-tree) T=$(git --git-dir="$BARE" rev-parse "$B^{tree}")
          M=$(echo merge | git --git-dir="$BARE" commit-tree "$T" -p "$B" -p "$H");;
    other-parents) T=$(git --git-dir="$BARE" rev-parse "$H^{tree}")
          M=$(echo merge | git --git-dir="$BARE" commit-tree "$T" -p "$H");;
    *) return 0;;
  esac
  git --git-dir="$BARE" update-ref refs/pull/12/merge "$M"
}
# Like GitHub, a pull request keeps the last head and base commits it saw once a branch is
# gone; `pull-head-sha` fakes a head another pusher moved it to.
pull_sha() {
  SHA=$($1)
  if [ -n "$SHA" ]; then printf '%s\n' "$SHA" > "$STATE/last-$1"; else SHA=$(cat "$STATE/last-$1" 2>/dev/null); fi
  printf '%s' "$SHA"
}
pull() {
  merge_ref
  PULL_STATE=$(cat "$STATE/pull-state" 2>/dev/null || echo open)
  PULL_HEAD=$(pull_sha head_sha); PULL_BASE=$(pull_sha base_sha)
  if [ -e "$STATE/pull-head-sha" ]; then PULL_HEAD=$(cat "$STATE/pull-head-sha"); fi
  printf '{"number":12,"html_url":"https://github.com/octo/gate/pull/12","state":"%s","draft":true,' "$PULL_STATE"
  printf '"head":{"ref":"af-gate/%s/head","sha":"%s","repo":{"full_name":"octo/gate"}},"base":{"ref":"af-gate/%s/base","sha":"%s","repo":{"full_name":"octo/gate"}}}' "$TASK" "$PULL_HEAD" "$TASK" "$PULL_BASE"
}
serve() { sed -e "s/@HEAD@/$(head_sha)/g" -e "s/@TASK@/$TASK/g" "$1"; }
case "$1" in
  auth)
    if [ -e "$STATE/unauthenticated" ]; then
      echo "You are not logged into any GitHub hosts. To log in, run: gh auth login" >&2; exit 1
    fi
    echo "github.com: Logged in to github.com account octo (keyring)"; exit 0;;
  api) shift;;
  *) echo "fake gh: unsupported command $1" >&2; exit 2;;
esac
if [ "$1" = "--method" ] && [ "$2" = "PATCH" ]; then
  # Closing the gate pull request when its Task finishes (ADR-0144).
  if [ -e "$STATE/refuse-close" ]; then
    echo "gh: Resource not accessible by integration (HTTP 403)" >&2; exit 1
  fi
  case "$3 $4 $5" in
    "repos/octo/gate/pulls/12 -f state=closed")
      echo closed >> "$STATE/pulls-closed"; echo closed > "$STATE/pull-state"; pull; exit 0;;
  esac
  echo "fake gh: unsupported PATCH $3" >&2; exit 2
fi
if [ "$1" = "--method" ] && [ "$2" = "POST" ]; then
  if [ -e "$STATE/refuse-pull" ]; then
    echo "gh: Validation Failed (HTTP 422): pushes to $BARE are not allowed" >&2; exit 1
  fi
  echo created >> "$STATE/pulls-created"
  touch "$STATE/pull-open"
  pull; exit 0
fi
case "$1" in
  repos/octo/gate/actions/jobs/*/logs)
    P=${1#repos/octo/gate/actions/jobs/}; JOB=${P%%/*}
    if [ -e "$STATE/log-$JOB.txt" ]; then cat "$STATE/log-$JOB.txt"; exit 0; fi
    echo "gh: Not Found (HTTP 404)" >&2; exit 1;;
  repos/octo/gate/pulls\?*)
    if [ -e "$STATE/pull-open" ]; then printf '['; pull; printf ']'; else printf '[]'; fi;;
  repos/octo/gate/pulls/12) pull;;
  repos/octo/gate/actions/runs\?*)
    if [ -e "$STATE/runs-hang" ]; then echo $$ > "$STATE/hang.pid"; exec sleep 60; fi
    if [ -e "$STATE/runs.json" ]; then serve "$STATE/runs.json"
    else printf '{"total_count":0,"workflow_runs":[]}'; fi;;
  repos/octo/gate/actions/runs/*/attempts/*/jobs\?*)
    P=${1#repos/octo/gate/actions/runs/}; RUN=${P%%/*}; P=${P#*/attempts/}; ATTEMPT=${P%%/*}
    if [ -e "$STATE/jobs-$RUN-$ATTEMPT.json" ]; then serve "$STATE/jobs-$RUN-$ATTEMPT.json"
    else printf '{"total_count":0,"jobs":[]}'; fi;;
  *) echo "fake gh: unsupported api $1" >&2; exit 2;;
esac
