#!/usr/bin/env bash
# Open the release PR for version X.Y.Z: bump the workspace version, write the CHANGELOG
# section from the pull requests merged since the last release, commit on release/vX.Y.Z, push,
# and open the PR. Merging that PR is the one human act of a release; .github/workflows/release.yml
# tags, checks, builds, signs, and publishes from there (ADR-0045).
#
#   scripts/release.sh X.Y.Z --compat "<one line on authority compatibility>" [--dry-run] [--yes]
#
# The compatibility line is required: every release says whether committed `.af/` authority
# keeps working as is, needs `af onboard --refresh-lock`, or needs a documented hand edit.
set -euo pipefail

usage() {
  sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
}

version=""
compat=""
dry_run=0
yes=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --compat) compat="${2:-}"; shift 2 ;;
    --compat=*) compat="${1#--compat=}"; shift ;;
    --dry-run) dry_run=1; shift ;;
    --yes) yes=1; shift ;;
    -h|--help) usage ;;
    -*) echo "release: unknown option $1" >&2; usage ;;
    *) [[ -z "$version" ]] || usage; version="${1#v}"; shift ;;
  esac
done
[[ -n "$version" && -n "$compat" ]] || usage
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ ]] || { echo "release: $version is not X.Y.Z or X.Y.Z-rc.N" >&2; exit 2; }

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"
repo="$(gh repo view --json nameWithOwner -q .nameWithOwner)"
current="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
tag="v$version"

# Semantic ordering: numeric X.Y.Z first; on an equal base a final release is newer than any
# rc, and rcs order by their number. Prints `newer`, `same`, or `older` for $1 against $2.
semver_order() {
  python3 - "$1" "$2" <<'EOF'
import re, sys
def key(version):
    base, _, pre = version.partition("-")
    return ([int(part) for part in base.split(".")], 0 if pre else 1, [int(n) for n in re.findall(r"\d+", pre)])
a, b = key(sys.argv[1]), key(sys.argv[2])
print("newer" if a > b else "same" if a == b else "older")
EOF
}
bump=1
case "$(semver_order "$version" "$current")" in
  same)
    # Already bumped on main (a feature PR that had to change the version): only the changelog
    # section is missing for this commit to become a release commit.
    bump=0 ;;
  older)
    echo "release: $version is not newer than the current version $current" >&2
    exit 1 ;;
esac
[[ -z "$(git status --porcelain)" ]] || { echo "release: the working tree is not clean" >&2; exit 1; }
if git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1; then
  echo "release: $tag already exists on origin" >&2
  exit 1
fi

# The pull requests merged since the last release, newest first.
last_tag="$(git tag --list 'v*' --sort=-v:refname | head -1)"
since="$(git log -1 --format=%cI "$last_tag" 2>/dev/null || echo 1970-01-01T00:00:00Z)"
changes="$(gh pr list --repo "$repo" --state merged --base main --limit 200 \
  --search "merged:>=$since" --json number,title,mergedAt \
  --jq 'sort_by(.mergedAt) | .[] | "- \(.title) (#\(.number))"')"
[[ -n "$changes" ]] || changes="- (no pull requests merged since $last_tag)"

date="$(date -u +%Y-%m-%d)"
section="## [$version] - $date

### Authority compatibility

$compat

### Changes

$changes
"

echo "release: $current -> $version on $repo"
echo
echo "$section"
if [[ "$dry_run" -eq 1 ]]; then
  echo "(dry run: nothing written)"
  exit 0
fi

git fetch -q origin main
branch="release/$tag"
git switch -q -c "$branch" origin/main

if [[ "$bump" -eq 1 ]]; then
  # Cargo.toml: the first `version = "…"` line is [workspace.package].version.
  python3 - "$current" "$version" <<'EOF'
import sys
current, version = sys.argv[1], sys.argv[2]
text = open("Cargo.toml").read()
needle = f'version = "{current}"\n'
if needle not in text:
    sys.exit(f"Cargo.toml has no version = \"{current}\" line")
open("Cargo.toml", "w").write(text.replace(needle, f'version = "{version}"\n', 1))
EOF
  cargo update --workspace --quiet
fi

# CHANGELOG.md: the new section goes right under [Unreleased].
python3 - "$version" "$section" <<'EOF'
import sys
version, section = sys.argv[1], sys.argv[2]
path = "CHANGELOG.md"
text = open(path).read()
marker = "## [Unreleased]\n"
if marker not in text:
    sys.exit("CHANGELOG.md has no [Unreleased] section")
head, tail = text.split(marker, 1)
open(path, "w").write(head + marker + "\n" + section + tail.lstrip("\n"))
EOF

git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -q -m "release: $tag" -m "$compat"
git --no-pager show --stat HEAD | head -20

if [[ "$yes" -ne 1 ]]; then
  read -r -p "push $branch and open the release PR on $repo? [y/N] " answer
  [[ "$answer" == y || "$answer" == Y ]] || { echo "release: left on branch $branch, nothing pushed"; exit 0; }
fi
git push -q -u origin "$branch"
gh pr create --repo "$repo" --base main --head "$branch" --title "release: $tag" --body "$section

Merging this PR tags \`$tag\`; the release workflow checks the tagged commit on Linux and macOS, builds every target, plans the consumer fixtures with each binary, signs \`SHA256SUMS\`, and publishes the release."
