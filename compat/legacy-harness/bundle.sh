#!/usr/bin/env bash
# bundle.sh — build a review bundle for a change: diff vs base, changed files,
# commit messages, diffstat, candidate tests. Read-only on the repo, no network.
#
# Usage:
#   bundle.sh [-C <repo-dir>] [--base <ref>] [--uncommitted] [--paths <g,g>] [--out <dir>]
#
#   --base         diff base ref (default: auto — the remote's default branch
#                  via origin/HEAD, else first of origin/master, origin/main,
#                  master, main that exists)
#   --uncommitted  include working-tree changes; untracked files are listed in
#                  untracked.txt AND appended to diff.patch/files.txt as adds
#   --paths        comma-separated pathspecs limiting the reviewed diff
#   --out          bundle directory (default: <repo>/.git/self-review/<ts>-<branch>)
#
# Prints a short summary; the LAST line of stdout is the bundle directory.
set -euo pipefail

REPO="."
BASE=""
OUT=""
UNCOMMITTED=0
PATHS=""

while [ $# -gt 0 ]; do
  case "$1" in
    -C) REPO="$2"; shift 2 ;;
    --base) BASE="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --uncommitted) UNCOMMITTED=1; shift ;;
    --paths) PATHS="$2"; shift 2 ;;
    -h|--help) awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"; exit 0 ;;
    *) echo "bundle.sh: unknown argument: $1" >&2; exit 2 ;;
  esac
done

# SPEC always starts with "--" so "${SPEC[@]}" is never an empty expansion
# (bash 3.2 + set -u); a bare trailing "--" means "no path filter" to git.
SPEC=(--)
if [ -n "$PATHS" ]; then
  IFS=',' read -r -a globs <<< "$PATHS"
  SPEC+=("${globs[@]}")
fi

cd "$REPO"
git rev-parse --git-dir >/dev/null 2>&1 || { echo "bundle.sh: not a git repo: $REPO" >&2; exit 2; }

if [ -z "$BASE" ]; then
  # Prefer the remote's actual default branch — a migrated repo may keep a
  # stale origin/master alongside the real origin/main. The symref can
  # dangle (e.g. after fetch --prune); verify it resolves or fall through.
  BASE="$(git symbolic-ref -q --short refs/remotes/origin/HEAD 2>/dev/null || true)"
  if [ -n "$BASE" ] && ! git rev-parse --verify --quiet "$BASE^{commit}" >/dev/null; then
    BASE=""
  fi
fi
if [ -z "$BASE" ]; then
  for cand in origin/master origin/main master main; do
    if git rev-parse --verify --quiet "$cand^{commit}" >/dev/null; then BASE="$cand"; break; fi
  done
fi
[ -n "$BASE" ] || { echo "bundle.sh: cannot auto-detect a base ref; pass --base" >&2; exit 2; }

MERGE_BASE="$(git merge-base "$BASE" HEAD)"
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
TS="$(date +%Y%m%d-%H%M%S)"
[ -n "$OUT" ] || OUT="$(git rev-parse --absolute-git-dir)/self-review/$TS-$(echo "$BRANCH" | tr '/' '_')"
mkdir -p "$OUT"

# Refuse a worktree-root OUT before writing anything: it cannot be excluded
# from the untracked scan, and even a refused run must not litter artifacts
# the next bundle would review as phantom changes. Canonicalize BOTH sides
# with the external pwd -P: it resolves symlinks (macOS /var -> /private/var)
# AND prints on-disk case — bash's builtin keeps typed case, which on a
# case-insensitive filesystem never prefix-matches git's canonical toplevel.
# The refusal itself is inode-identity (-ef), immune to any residual alias.
phys_pwd() { cd "$1" && { /bin/pwd -P 2>/dev/null || pwd -P; }; }
outabs="$(phys_pwd "$OUT")"
top="$(phys_pwd "$(git rev-parse --show-toplevel)")"
if [ "$outabs" = "$top" ] || [ "$outabs" -ef "$top" ]; then
  echo "bundle.sh: --out must not be the worktree root — the bundle's own artifacts would be reviewed as untracked changes" >&2
  exit 2
fi

if [ "$UNCOMMITTED" -eq 1 ]; then
  git diff "$MERGE_BASE" "${SPEC[@]}" > "$OUT/diff.patch"
  git diff --name-status "$MERGE_BASE" "${SPEC[@]}" > "$OUT/files.txt"
  git diff --stat "$MERGE_BASE" "${SPEC[@]}" > "$OUT/stats.txt"
  git diff --numstat "$MERGE_BASE" "${SPEC[@]}" > "$OUT/.numstat"
  # git diff can't see untracked files — append their full contents as adds,
  # or reviewers converge without ever seeing a brand-new file. NUL-delimited:
  # ls-files C-quotes non-ASCII names on its text output, which would feed
  # git-diff a quoted literal it cannot open. If OUT sits inside the
  # worktree, exclude it — the bundle must not review its own artifacts.
  outrel="${outabs#"$top"/}"
  if [ "$outrel" != "$outabs" ]; then
    # literal: an OUT name containing glob chars must not widen the exclusion.
    git ls-files --others --exclude-standard -z "${SPEC[@]}" ":(exclude,literal)$outrel" > "$OUT/.untracked0"
  else
    git ls-files --others --exclude-standard -z "${SPEC[@]}" > "$OUT/.untracked0"
  fi
  tr '\0' '\n' < "$OUT/.untracked0" > "$OUT/untracked.txt"
  while IFS= read -r -d '' uf; do
    printf 'A\t%s\n' "$uf" >> "$OUT/files.txt"
    git diff --no-index -- /dev/null "$uf" >> "$OUT/diff.patch" || true
    git diff --no-index --numstat -- /dev/null "$uf" >> "$OUT/.numstat" || true
  done < "$OUT/.untracked0"
  rm -f "$OUT/.untracked0"
else
  git diff "$MERGE_BASE" HEAD "${SPEC[@]}" > "$OUT/diff.patch"
  git diff --name-status "$MERGE_BASE" HEAD "${SPEC[@]}" > "$OUT/files.txt"
  git diff --stat "$MERGE_BASE" HEAD "${SPEC[@]}" > "$OUT/stats.txt"
  git diff --numstat "$MERGE_BASE" HEAD "${SPEC[@]}" > "$OUT/.numstat"
fi
if [ ! -s "$OUT/files.txt" ]; then
  echo "bundle.sh: no changes vs $BASE (merge-base $MERGE_BASE) — wrong --base, or forgot --uncommitted?" >&2
  exit 1
fi
git log --reverse --format='%h %s%n%b' "$MERGE_BASE..HEAD" > "$OUT/commits.txt"

# Changed test files, and candidate tests matched by name tokens of changed
# source files (camelCase split at case boundaries, snake tokens >= 5 chars).
# name-status rows are TAB-separated (last field = new path, also for renames);
# awk must split on TAB only or paths with spaces get mangled.
awk -F'\t' '{print $NF}' "$OUT/files.txt" > "$OUT/.paths"
grep -E '(^|/)tests?/' "$OUT/.paths" > "$OUT/tests_changed.txt" || true
grep -Ev '(^|/)tests?/' "$OUT/.paths" | while IFS= read -r f; do
  b="${f##*/}"; b="${b%%.*}"
  printf '%s\n' "$b" \
    | sed 's/\([a-z0-9]\)\([A-Z]\)/\1_\2/g' \
    | tr '[:upper:]' '[:lower:]' \
    | tr -cs 'a-z0-9' '\n'
done | awk 'length($0) >= 5' | sort -u | head -20 > "$OUT/.tokens" || true

: > "$OUT/tests_candidates.txt"
if [ -s "$OUT/.tokens" ]; then
  pat="$(paste -sd'|' - < "$OUT/.tokens")"
  git ls-files -- 'tests/*' '*/tests/*' 'test/*' '*/test/*' 2>/dev/null \
    | grep -E -i "($pat)" | sort -u | head -200 > "$OUT/tests_candidates.txt" || true
fi
rm -f "$OUT/.paths" "$OUT/.tokens"

{
  echo "repo=$(basename "$(git rev-parse --show-toplevel)")"
  echo "branch=$BRANCH"
  echo "head=$(git rev-parse HEAD)"
  echo "base=$BASE"
  echo "merge_base=$MERGE_BASE"
  echo "uncommitted=$UNCOMMITTED"
  echo "created=$TS"
  echo "files_changed=$(wc -l < "$OUT/files.txt" | tr -d ' ')"
  echo "diff_lines=$(wc -l < "$OUT/diff.patch" | tr -d ' ')"
  echo "changed_lines=$(awk '{ins += $1; del += $2} END {print ins + del + 0}' "$OUT/.numstat")"
} > "$OUT/meta.env"
rm -f "$OUT/.numstat"

sed 's/^/  /' "$OUT/meta.env"
echo "$OUT"
