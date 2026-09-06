#!/usr/bin/env bash
# Plan every consumer fixture with the given `af` binary. Token-free; creates no Campaign state.
#
#   fixtures/consumers/check.sh <af-binary> [fixture-dir ...]
#
# Each fixture is copied into a fresh temporary git repository and committed, because `af review
# plan` reads policy from a git revision, never from the working tree. Exit 1 if any plan fails.
set -euo pipefail

af=${1:?usage: fixtures/consumers/check.sh <af-binary> [fixture-dir ...]}
case "$af" in
  /*) ;;
  *) af="$(pwd -P)/$af" ;;
esac
[ -x "$af" ] || { echo "not an executable: $af" >&2; exit 2; }
shift
root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
if [ $# -eq 0 ]; then
  set -- "$root"/*/
fi

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 AF_SELF_OFFLINE=1   # plan with THIS binary, never a pinned one
status=0
for fixture in "$@"; do
  fixture="${fixture%/}"
  name="$(basename "$fixture")"
  if [ ! -d "$fixture/.af" ]; then
    continue
  fi
  tmp="$(mktemp -d)"
  cp -R "$fixture"/. "$tmp"/
  git -C "$tmp" init -q
  git -C "$tmp" -c user.name=consumer -c user.email=consumer@example.invalid add -A
  git -C "$tmp" -c user.name=consumer -c user.email=consumer@example.invalid \
    commit -q -m "consumer fixture $name"
  if out="$("$af" review plan --repo "$tmp" \
      --policy-rev HEAD --base HEAD --candidate HEAD --json 2>&1)"; then
    if printf '%s' "$out" | grep -q '"schema": *"af/review-plan@1"'; then
      echo "ok   $name"
    else
      echo "FAIL $name: plan succeeded without an af/review-plan@1 document"
      printf '%s\n' "$out"
      status=1
    fi
  else
    echo "FAIL $name: $out"
    status=1
  fi
  rm -rf "$tmp"
done
exit "$status"
