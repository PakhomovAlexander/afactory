#!/usr/bin/env bash
# Every relative link in every Markdown file must name something in the tree.
#
#   scripts/check-links.sh        # lists every broken link; exit 1 if there is one
#
# Only the path is checked: `#fragment`s are stripped, never resolved. Absolute URLs
# (`http(s)://`, `mailto:`, any `scheme:`) and bare `#anchor` links are ignored, and so are
# fenced code blocks and inline code spans. Inline links `[text](target)`, images, and
# reference definitions `[id]: target` are covered. Needs bash, find, and awk; nothing else.
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

if [[ "$broken" -gt 0 ]]; then
  echo "check-links: $broken broken relative link(s) in $checked checked" >&2
  exit 1
fi
echo "check-links: $checked relative links resolve"
