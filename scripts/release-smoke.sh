#!/usr/bin/env bash
# The functional smoke every packaged af binary passes before a release publishes it: onboard a
# fresh repository and plan native Review, token-free, without an admitted Provider.
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
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 AF_SELF_OFFLINE=1
export AF_PROVIDERS_FILE="$scratch/providers.toml"
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
# A native Review file selects the committed Task catalog. Keep the request outside source;
# a real change gives the Diff guard meaningful input before any model or Gate can run.
cp "$repo/.af/task-packages/onboard/review-code/task.json" "$scratch/review.json"
printf 'A source change for native Review planning.\n' > "$repo/change.txt"
status=0
"$af" review plan --repo "$repo" --file "$scratch/review.json" \
  --authority HEAD --base HEAD --uncommitted --json > "$scratch/plan.json" || status=$?
# Missing Providers produce the current native selection receipt, not an invented Review
# plan. Accept only that precise, uncharged refusal; every other failure stops publication.
jq -e --argjson status "$status" '
  $status == 1 and .schema == "af/task-selection@3" and
  .task_id == "review" and .attempts == 0 and .chargeable_tokens == 0 and
  (.selection_id | startswith("sha256:")) and
  (.request_revision_id | startswith("sha256:")) and
  .selection.decision.kind == "unavailable" and
  (.selection.candidates | length) == 1 and
  .selection.candidates[0].pipeline == "onboard/review-code" and
  .selection.candidates[0].state.kind == "unavailable" and
  (.selection.candidates[0].state.reason |
    contains("provider `codex-main` is not an explicit entry in the machine-local registry"))
' "$scratch/plan.json" >/dev/null || {
  echo "the built binary produced no native missing-Provider selection receipt (exit $status)" >&2
  cat "$scratch/plan.json" >&2
  exit 1
}
echo "release smoke: onboarded native Review and checked the zero-Attempt Provider refusal with $af"
