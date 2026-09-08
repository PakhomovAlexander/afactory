#!/usr/bin/env bash
# markdownlint over every Markdown file, from the digest-pinned toolchain in tools/markdownlint/.
#
#   scripts/markdownlint.sh        # the markdownlint CI job; `make markdownlint`
#
# `npm ci` installs exactly what package-lock.json pins — every tarball checked against the
# integrity hash recorded there — into a cache keyed by the lock's digest, outside the (read-only)
# source tree, once per machine. The installed version is compared with the pin before anything
# runs. Nothing is fetched by a floating version: not `npx`, not whatever the registry serves
# today. Needs node and npm, and a reachable registry until that cache is warm.
#
# That first install is why the review pipeline's Gate runs this only as an advisory Check
# (.af/pipelines/review.toml): `npm ci` has no offline path, and a *required* Check that cannot
# run loses the Round with no reviewer. Enforcement lives in the markdownlint job in
# .github/workflows/ci.yml, where the network is available.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
tools="$root/tools/markdownlint"
for program in node npm; do
  command -v "$program" >/dev/null 2>&1 || { echo "markdownlint: $program is required" >&2; exit 1; }
done
pinned="$(node -p "require(process.argv[1]).devDependencies['markdownlint-cli2']" "$tools/package.json")"
lock_digest="$( { command -v sha256sum >/dev/null 2>&1 && sha256sum "$tools/package-lock.json" || shasum -a 256 "$tools/package-lock.json"; } | awk '{print $1}')"
# The install tree must be a directory this user owns. With neither XDG_CACHE_HOME nor HOME set
# — precisely the Gate's environment — a `${HOME:-/tmp}` fallback lands in world-writable
# /tmp/.cache, and the line below execs whatever `markdownlint-cli2` it finds there while the pin
# check reads the package.json supplied by that same directory: anyone who pre-creates the path
# runs code inside the Gate, self-attesting its version. Refuse instead, which also surfaces the
# missing HOME rather than hiding it.
if [[ -n "${AFACTORY_MARKDOWNLINT_DIR:-}" ]]; then
  prefix="$AFACTORY_MARKDOWNLINT_DIR"
elif [[ -n "${XDG_CACHE_HOME:-}" ]]; then
  prefix="$XDG_CACHE_HOME/afactory/markdownlint/$lock_digest"
elif [[ -n "${HOME:-}" ]]; then
  prefix="$HOME/.cache/afactory/markdownlint/$lock_digest"
else
  echo "markdownlint: neither XDG_CACHE_HOME nor HOME is set, so this script owns no cache \
directory — it will not install into or execute out of /tmp — fix: set XDG_CACHE_HOME or HOME, \
or name the install tree with AFACTORY_MARKDOWNLINT_DIR" >&2
  exit 1
fi
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
