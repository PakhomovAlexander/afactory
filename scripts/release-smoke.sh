#!/usr/bin/env bash
# The functional smoke every packaged af binary passes before a release publishes it: onboard a
# fresh repository and plan it, token-free, with no Campaign state and no Provider.
#
#   scripts/release-smoke.sh AF [SCRATCH]
#
# AF is the binary to run, or a wrapper that runs it (the release runs aarch64 under qemu).
# SCRATCH defaults to a temporary directory. The release workflow runs it on each packaged
# binary; the always-on base job of every pull request runs it on that pull request's debug
# binary, so a change to `af onboard` or `af review plan` that breaks it fails the pull request
# that makes the change, not the release after it (v0.14.0-rc.1 was tagged and never published
# because only the release ran it).
set -euo pipefail

[[ $# -ge 1 && $# -le 2 ]] || { sed -n '5,6p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }
af="$1"
if [[ $# -eq 2 ]]; then
  scratch="$2"
  rm -rf "$scratch" && mkdir -p "$scratch"
else
  scratch="$(mktemp -d)"
  trap 'rm -rf "$scratch"' EXIT
fi
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
# A private home: the caller's Providers, Stores and configuration neither change the outcome
# nor receive anything.
export HOME="$scratch/home" XDG_CONFIG_HOME="$scratch/home/.config" \
  XDG_STATE_HOME="$scratch/home/.local/state" XDG_DATA_HOME="$scratch/home/.local/share" \
  XDG_CACHE_HOME="$scratch/home/.cache"
mkdir -p "$HOME"
repo="$scratch/repo"
mkdir -p "$repo"
git -C "$repo" init -q
"$af" onboard --repo "$repo" --runner codex --gate check=true --apply
git -C "$repo" add -A
git -C "$repo" -c user.name=release -c user.email=release@example.invalid \
  commit -q -m "onboarded by the built binary"
# With no Provider registered the plan is refused for its missing bindings (exit 1, cause
# review_binding_missing) and carried whole in the refusal's details.plan (#363, #434); any
# other failure stops the release.
status=0
"$af" review plan --repo "$repo" \
  --policy-rev HEAD --base HEAD --candidate HEAD --json > "$scratch/plan.json" || status=$?
if [[ "$status" -eq 0 ]]; then
  filter='.schema'
elif [[ "$status" -eq 1 ]]; then
  filter='select(.cause == "review_binding_missing") | .details.plan.schema'
else
  filter='empty'
fi
jq -e "$filter == \"af/review-plan@1\"" "$scratch/plan.json" >/dev/null || {
  echo "the built binary planned no af/review-plan@1 document (exit $status)" >&2
  cat "$scratch/plan.json" >&2
  exit 1
}
echo "release smoke: onboarded and planned with $af"
