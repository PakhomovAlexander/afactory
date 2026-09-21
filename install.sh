#!/bin/sh
# Install af — the newest stable release, or AF_INSTALL_VERSION — into the self-managed layout:
#   $XDG_DATA_HOME/af/versions/<v>/{af,receipt.toml}   and   $XDG_BIN_HOME/af -> that binary
# Needs curl and tar; never stores a token. Re-running is idempotent; `af self` takes over from
# here: the installed binary records the activation, so `af self rollback` works from the first
# update on.
#
#   curl -fsSL https://github.com/PakhomovAlexander/afactory/releases/latest/download/install.sh | sh
#
# A private fork, or a host without curl, falls back to the GitHub CLI: `gh auth login` with an
# account that can read the release repository (AF_REPO), and the same script works unchanged.
#
# Verification: the archive digest against the release's SHA256SUMS, and — when `minisign` is on
# PATH — that file's signature against the release public key embedded below (AF_RELEASE_KEY
# names a different key file). The installed `af` carries the key itself and verifies every
# later install.
set -eu

REPO="${AF_REPO:-PakhomovAlexander/afactory}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/af/versions"
BIN="${XDG_BIN_HOME:-$HOME/.local/bin}"
# The oldest supported release: the first that ships SHA256SUMS.minisig. Nothing older installs.
FLOOR="0.8.0"

# crates/af/keys/release.pub — the key every release is signed with.
RELEASE_PUB='untrusted comment: minisign public key 0FA9BDD8A1D67165
RWRlcdah2L2pD6l9dkQHwwqt2PjfdxZTyuAZwArbnqaVkKPrkIGOmqAL'

case "$(uname -s):$(uname -m)" in
  Darwin:arm64) host="aarch64-apple-darwin" ;;
  Linux:x86_64) host="x86_64-unknown-linux-musl" ;;
  Linux:aarch64|Linux:arm64) host="aarch64-unknown-linux-musl" ;;
  Darwin:x86_64) echo "af: no release is built for x86_64-apple-darwin — fix: build from source (cargo install --path crates/af --locked)" >&2; exit 1 ;;
  *) echo "af: unsupported platform $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

# How releases are reached: anonymously over HTTPS with curl, or through an authenticated
# GitHub CLI when curl is missing or the repository is not public (the API answers 404).
via=curl
use_gh() {
  command -v gh >/dev/null 2>&1 || { echo "af: the GitHub CLI is required${1:+ ($1)} — fix: install gh, then gh auth login" >&2; exit 1; }
  gh auth status >/dev/null 2>&1 || { echo "af: gh is not authenticated — fix: gh auth login (an account with access to $REPO)" >&2; exit 1; }
  via=gh
}
command -v curl >/dev/null 2>&1 || use_gh "curl is not installed"

# http_get URL FILE: fetch URL into FILE and print the final HTTP status (000 when curl itself
# failed). GitHub answers 404 for anything in a private repository, which is the fallback cue.
http_get() {
  code=$(curl -sSL --retry 3 -o "$2" -w '%{http_code}' -H 'Accept: application/vnd.github+json' "$1") || code=000
  [ "$code" = "200" ] || rm -f "$2"
  echo "$code"
}

# The `tag_name` values of a GitHub releases API response, one per line. Splitting at commas
# first makes compact and pretty-printed JSON read the same; a tag name never contains one.
release_tags() {
  tr ',' '\n' | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p'
}

# The newest of the given `vX.Y.Z` tags by semantic version — never by creation date, so a
# patch release on an older line can never shadow the current one. Pre-release tags carry a
# `-rc.N` suffix and never match the three-field shape, so they are never chosen here.
semver_max() {
  sed 's/^v//' | awk -F. '
    NF == 3 && $1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ {
      if (!seen || $1 > a || ($1 == a && ($2 > b || ($2 == b && $3 > c)))) { a = $1; b = $2; c = $3; seen = 1 }
    }
    END { if (seen) print "v" a "." b "." c }'
}

# semver_lt A B: true when A < B (major.minor.patch, numeric; a pre-release suffix counts as 0).
semver_lt() {
  awk -v x="$1" -v y="$2" 'BEGIN {
    split(x, a, "."); split(y, b, ".")
    for (i = 1; i <= 3; i++) { if (a[i] + 0 < b[i] + 0) exit 0; if (a[i] + 0 > b[i] + 0) exit 1 }
    exit 1 }'
}

if [ -n "${AF_INSTALL_VERSION:-}" ]; then
  tag="v${AF_INSTALL_VERSION#v}"
else
  tag=""
  if [ "$via" = curl ]; then
    code=$(http_get "https://api.github.com/repos/$REPO/releases?per_page=100" "$tmp/releases.json")
    case "$code" in
      200) tag=$(release_tags < "$tmp/releases.json" | semver_max) ;;
      404) use_gh "$REPO is not public" ;;
      *) echo "af: GitHub API returned $code listing releases of $REPO — fix: retry later, or set AF_INSTALL_VERSION=X.Y.Z to skip the lookup" >&2; exit 1 ;;
    esac
  fi
  if [ "$via" = gh ]; then
    tag=$(gh release list --repo "$REPO" --exclude-pre-releases --exclude-drafts --limit 100 --json tagName -q '.[].tagName' | semver_max)
  fi
  [ -n "$tag" ] || { echo "af: no release found in $REPO" >&2; exit 1; }
fi
version="${tag#v}"
if semver_lt "$version" "$FLOOR"; then
  echo "af: $version is older than the oldest supported release ($FLOOR); pick a newer release" >&2
  exit 1
fi
asset="af-${tag}-${host}.tar.gz"

# fetch_asset NAME: download one release asset of $tag into $tmp; false when it is absent.
# A 404 from an anonymous download of an existing tag means a private repository: switch to
# gh for this and every later asset.
fetch_asset() {
  if [ "$via" = curl ]; then
    code=$(http_get "https://github.com/$REPO/releases/download/$tag/$1" "$tmp/$1")
    case "$code" in
      200) return 0 ;;
      404) [ "$1" = "$asset" ] || return 1
           use_gh "$REPO is not public" ;;
      *) echo "af: download of $1 from $REPO $tag failed (HTTP $code)" >&2; exit 1 ;;
    esac
  fi
  gh release download "$tag" --repo "$REPO" --pattern "$1" --dir "$tmp" >/dev/null 2>&1 && [ -f "$tmp/$1" ]
}

if [ -x "$DATA/$version/af" ] && grep -q "^version = \"$version\"" "$DATA/$version/receipt.toml" 2>/dev/null \
   && grep -q "^target = \"$host\"" "$DATA/$version/receipt.toml" 2>/dev/null; then
  echo "af $version is already installed at $DATA/$version/af (receipt present)"
else
  # Absent, or a bare binary without a receipt: never adopt it — install fresh and replace it.
  fetch_asset "$asset" || { echo "af: release $tag of $REPO has no asset $asset" >&2; exit 1; }
  fetch_asset SHA256SUMS || { echo "af: release $tag of $REPO has no SHA256SUMS" >&2; exit 1; }
  verified_by=sha256sums
  if command -v minisign >/dev/null 2>&1; then
    if [ -n "${AF_RELEASE_KEY:-}" ]; then
      key="$AF_RELEASE_KEY"
    else
      key="$tmp/release.pub"
      printf '%s\n' "$RELEASE_PUB" > "$key"
    fi
    if fetch_asset SHA256SUMS.minisig; then
      minisign -V -q -m "$tmp/SHA256SUMS" -x "$tmp/SHA256SUMS.minisig" -p "$key" \
        || { echo "af: SHA256SUMS of $tag does not verify against $key" >&2; exit 1; }
      verified_by=minisign
    else
      echo "af: release $tag has no SHA256SUMS.minisig to verify against $key" >&2; exit 1
    fi
  else
    echo "af: minisign is not installed, so only the checksum is verified — to verify the release signature too: brew install minisign (macOS) or apt install minisign (Debian/Ubuntu); the installed af verifies every later update with its embedded key"
  fi
  expected=$(grep " [*]*$asset\$" "$tmp/SHA256SUMS" | awk '{print $1}')
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$asset" | awk '{print $1}')
  else
    actual=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
  fi
  [ -n "$expected" ] && [ "$actual" = "$expected" ] || {
    echo "af: checksum mismatch for $asset" >&2; echo "expected $expected" >&2; echo "actual   $actual" >&2; exit 1; }
  mkdir -p "$tmp/unpack" && tar -xzf "$tmp/$asset" -C "$tmp/unpack"
  chmod 0755 "$tmp/unpack/af"
  reported=$(AF_SELF_OFFLINE=1 "$tmp/unpack/af" --version)
  [ "$reported" = "af $version" ] || { echo "af: release binary reported '$reported', expected 'af $version'" >&2; exit 1; }
  staging="$DATA/.$version.$$"
  rm -rf "$staging" && mkdir -p "$staging"
  cp "$tmp/unpack/af" "$staging/af"
  cat > "$staging/receipt.toml" <<RECEIPT
version = "$version"
target = "$host"
source = "github:$REPO"
asset = "$asset"
sha256 = "$actual"
verified_by = "$verified_by"
installed_at = "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
RECEIPT
  rm -rf "${DATA:?}/${version:?}" && mv "$staging" "$DATA/$version"
  echo "af $version installed at $DATA/$version/af ($verified_by verified)"
fi

# The binary activates itself: it links the default and records the activation in its own
# state, so what the installer did is exactly what `af self update` would have done.
AF_SELF_OFFLINE=1 "$DATA/$version/af" self update --version "$version"
case ":$PATH:" in
  *":$BIN:"*) ;;
  *) echo "add $BIN to PATH, e.g.:  export PATH=\"$BIN:\$PATH\"" ;;
esac
echo "next: af self setup-shell --write   (completions + man pages) · af self status"
