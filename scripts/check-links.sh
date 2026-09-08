#!/usr/bin/env bash
# Every reference into this tree must name something that is in it.
#
#   scripts/check-links.sh        # lists every broken reference; exit 1 if there is one
#
# Two passes. Relative Markdown links: only the path is checked, `#fragment`s are stripped and
# never resolved. Absolute URLs (`http(s)://`, `mailto:`, any `scheme:`) and bare `#anchor` links
# are ignored, and so are fenced code blocks and inline code spans. Inline links `[text](target)`,
# images, and reference definitions `[id]: target` are covered.
#
# Then `cargo … --example <name>` anywhere in the tree — Markdown, TOML comments, scripts, Rust.
# A deleted example leaves its instructions behind everywhere it was cited, and every citation
# still reads as a working command; `fixtures/consumers/hub/.af/pipelines/review.toml` told
# consumers to run a lock generator that had been deleted. Frozen migration inputs under
# `crates/*/tests/fixtures/` are exempt: they are a record of bytes that once existed, not
# instructions to anyone, and correcting them would change what the migration tests migrate.
# Needs bash, find, grep, and awk.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"
export LC_ALL=C

broken=0
checked=0
while IFS= read -r file; do
  dir="$(dirname "$file")"
  # One `line<TAB>target` per link outside code, as written.
  links="$(awk '
    /^(```|~~~)/ { fence = !fence; next }
    fence { next }
    {
      line = $0
      gsub(/`[^`]*`/, "", line)
      while (match(line, /\]\([^)]*\)/)) {
        print NR "\t" substr(line, RSTART + 2, RLENGTH - 3)
        line = substr(line, RSTART + RLENGTH)
      }
      if (match($0, /^[ \t]*\[[^]]+\]:[ \t]+[^ \t]+/)) {
        def = substr($0, RSTART, RLENGTH)
        sub(/^[ \t]*\[[^]]+\]:[ \t]+/, "", def)
        print NR "\t" def
      }
    }' "$file")"
  [[ -n "$links" ]] || continue
  while IFS=$'\t' read -r line target; do
    target="${target#<}"
    target="${target%%[[:space:]]*}"
    target="${target%>}"
    target="${target%%#*}"
    [[ -n "$target" ]] || continue                         # a bare #anchor
    [[ "$target" =~ ^[A-Za-z][A-Za-z0-9+.-]*: ]] && continue  # http:, https:, mailto:, …
    [[ "$target" == //* ]] && continue
    checked=$((checked + 1))
    if [[ "$target" == /* ]]; then
      resolved="$root$target"
    else
      resolved="$dir/$target"
    fi
    if [[ ! -e "$resolved" ]]; then
      echo "$file:$line: broken link: $target"
      broken=$((broken + 1))
    fi
  done <<< "$links"
done < <(find . \( -path ./.git -o -path ./target -o -path ./.scratch -o -name node_modules \) -prune \
           -o -name '*.md' -print | sed 's|^\./||' | sort)

# ------------------------------------------------- cargo examples that were cited
# `--example <name>` must name crates/<crate>/examples/<name>.rs (or examples/<name>.rs).
examples=0
while IFS=: read -r file line rest; do
  name="$(printf '%s' "$rest" | awk 'match($0, /--example[ =]+[A-Za-z0-9_-]+/) {
    print substr($0, RSTART, RLENGTH)
  }' | awk '{print $NF}' | sed 's/^--example=//')"
  [[ -n "$name" ]] || continue
  case "$file" in crates/*/tests/fixtures/*) continue ;; esac   # frozen migration input
  examples=$((examples + 1))
  if [[ ! -f "examples/$name.rs" ]] && ! compgen -G "crates/*/examples/$name.rs" >/dev/null; then
    echo "$file:$line: no such cargo example: $name"
    broken=$((broken + 1))
  fi
done < <(grep -rIn -- '--example' . \
           --exclude-dir=.git --exclude-dir=target --exclude-dir=.scratch \
           --exclude-dir=node_modules --exclude=check-links.sh 2>/dev/null \
         | sed 's|^\./||' || true)

if [[ "$broken" -gt 0 ]]; then
  echo "check-links: $broken broken reference(s) in $checked link(s) and $examples example citation(s)" >&2
  exit 1
fi
echo "check-links: $checked relative links and $examples cargo example citation(s) resolve"
