#!/usr/bin/env bash
# Select only the version-bump commit on main; an ordinary later push cannot steal its tag.
set -euo pipefail

version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
[[ -n "$version" ]] || { echo "Cargo.toml has no workspace version" >&2; exit 1; }
head="$(git rev-parse HEAD)"
[[ "$head" == "$GITHUB_SHA" ]] || { echo "checkout differs from triggering commit" >&2; exit 1; }
tag=""
candidate="v$version"

# Prefer the peeled commit for annotated tags; lightweight tags already name the commit.
remote_commit() {
  local refs
  refs="$(git ls-remote origin "refs/tags/$candidate" "refs/tags/$candidate^{}")" || return 1
  printf '%s\n' "$refs" | awk '/\^\{\}$/ { peeled=$1 } NF == 2 { direct=$1 } END { print (peeled ? peeled : direct) }'
}

if [[ "$GITHUB_REF_TYPE" == tag ]]; then
  tag="$GITHUB_REF_NAME"
  [[ "$tag" == "$candidate" ]] || { echo "tag $tag does not match version $version" >&2; exit 1; }
else
  # fetch-depth: 2 supplies the first parent for squash, merge and fast-forward commits.
  parent_manifest="$(git show HEAD^:Cargo.toml)"
  previous="$(printf '%s\n' "$parent_manifest" | sed -n 's/^version = "\(.*\)"$/\1/p' | head -1)"
  [[ -n "$previous" ]] || { echo "parent has no workspace version" >&2; exit 1; }
  if [[ "$previous" == "$version" ]]; then
    echo "workspace version unchanged; ordinary main validation"
  elif ! awk -v version="$version" 'index($0, "## [" version "]") == 1 { found=1 } END { exit !found }' CHANGELOG.md; then
    echo "no changelog section for $version; not a release commit"
  else
    remote="$(remote_commit)"
    if [[ -z "$remote" ]]; then
      git config user.name "github-actions[bot]"
      git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
      git tag -a "$candidate" -m "$candidate" "$head"
      if git push origin "refs/tags/$candidate"; then
        remote="$head"
      else
        # A concurrent creator may have won. Never force or silently swallow a network error.
        remote="$(remote_commit)"
        [[ -n "$remote" ]] || { echo "tag push failed and no remote tag exists" >&2; exit 1; }
      fi
    fi
    if [[ "$remote" == "$head" ]]; then
      tag="$candidate"
    else
      echo "$candidate already names another commit; ordinary main validation"
    fi
  fi
fi

prerelease=false
[[ "$version" == *-* ]] && prerelease=true
{
  echo "tag=$tag"
  echo "version=$version"
  echo "prerelease=$prerelease"
} >> "$GITHUB_OUTPUT"
