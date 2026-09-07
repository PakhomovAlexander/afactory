#!/usr/bin/env bash
# markdownlint over every Markdown file, from the digest-pinned toolchain in tools/markdownlint/.
#
#   scripts/markdownlint.sh        # the review pipeline's second Gate Check; `make markdownlint`
#
# `npm ci` installs exactly what package-lock.json pins — every tarball checked against the
# integrity hash recorded there — into a cache keyed by the lock's digest, outside the (read-only)
# source tree, once per machine. The installed version is compared with the pin before anything
# runs. Nothing is fetched by a floating version: not `npx`, not whatever the registry serves
# today. Needs node and npm.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
tools="$root/tools/markdownlint"
for program in node npm; do
  command -v "$program" >/dev/null 2>&1 || { echo "markdownlint: $program is required" >&2; exit 1; }
done
pinned="$(node -p "require(process.argv[1]).devDependencies['markdownlint-cli2']" "$tools/package.json")"
lock_digest="$( { command -v sha256sum >/dev/null 2>&1 && sha256sum "$tools/package-lock.json" || shasum -a 256 "$tools/package-lock.json"; } | awk '{print $1}')"
cache_home="${XDG_CACHE_HOME:-${HOME:-/tmp}/.cache}"
prefix="${AFACTORY_MARKDOWNLINT_DIR:-$cache_home/afactory/markdownlint/$lock_digest}"
bin="$prefix/node_modules/.bin/markdownlint-cli2"

if [[ ! -x "$bin" ]]; then
  mkdir -p "$prefix"
  cp "$tools/package.json" "$tools/package-lock.json" "$prefix/"
  (cd "$prefix" && npm ci --ignore-scripts --no-audit --no-fund --loglevel=error)
fi
installed="$(node -p "require(process.argv[1]).version" "$prefix/node_modules/markdownlint-cli2/package.json")"
if [[ "$installed" != "$pinned" ]]; then
  echo "markdownlint: installed markdownlint-cli2 $installed is not the pinned $pinned — fix: rm -r '$prefix' and rerun" >&2
  exit 1
fi

cd "$root"
exec "$bin" --config .markdownlint-cli2.jsonc "**/*.md" "!target" "!.scratch"
