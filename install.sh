#!/bin/sh
# Install af — the newest stable release, or AF_INSTALL_VERSION — into the self-managed layout:
#   $XDG_DATA_HOME/af/versions/<v>/{af,receipt.toml}   and   $XDG_BIN_HOME/af -> that binary
# Requires the GitHub CLI authenticated with access to the release repository while it is
# private. Never stores a token. Re-running is idempotent; `af self` takes over from here: the
# installed binary records the activation, so `af self rollback` works from the first update on.
#
#   gh api repos/PakhomovAlexander/afactory/contents/install.sh -H 'Accept: application/vnd.github.raw' | sh
#
# Verification: the archive digest against the release's SHA256SUMS, and — when `minisign` is on
# PATH and AF_RELEASE_KEY names the release public key file — that file's signature too. The
# installed `af` carries the key itself and verifies every later install.
set -eu

REPO="${AF_REPO:-PakhomovAlexander/afactory}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/af/versions"
BIN="${XDG_BIN_HOME:-$HOME/.local/bin}"
# The first release with `af self`. Older releases cannot activate themselves or update back.
FLOOR="0.7.1"

case "$(uname -s):$(uname -m)" in
  Darwin:arm64) host="aarch64-apple-darwin" ;;
  Linux:x86_64) host="x86_64-unknown-linux-musl" ;;
  Linux:aarch64|Linux:arm64) host="aarch64-unknown-linux-musl" ;;
  Darwin:x86_64) echo "af: no release is built for x86_64-apple-darwin — fix: build from source (cargo install --path crates/reviewctl --locked)" >&2; exit 1 ;;
  *) echo "af: unsupported platform $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac

command -v gh >/dev/null 2>&1 || { echo "af: the GitHub CLI is required — fix: install gh, then gh auth login" >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "af: gh is not authenticated — fix: gh auth login (an account with access to $REPO)" >&2; exit 1; }

# The newest of the given `vX.Y.Z` tags by semantic version — never by creation date, so a
# patch release on an older line can never shadow the current one.
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
  tag=$(gh release list --repo "$REPO" --exclude-pre-releases --exclude-drafts --limit 100 --json tagName -q '.[].tagName' | semver_max)
  [ -n "$tag" ] || { echo "af: no release found in $REPO" >&2; exit 1; }
fi
version="${tag#v}"
if semver_lt "$version" "$FLOOR"; then
  echo "af: $version predates self-management (the first release with \`af self\` is $FLOOR); pick a newer release" >&2
  exit 1
fi
asset="af-${tag}-${host}.tar.gz"

if [ -x "$DATA/$version/af" ] && grep -q "^version = \"$version\"" "$DATA/$version/receipt.toml" 2>/dev/null \
   && grep -q "^target = \"$host\"" "$DATA/$version/receipt.toml" 2>/dev/null; then
  echo "af $version is already installed at $DATA/$version/af (receipt present)"
else
  # Absent, or a bare binary without a receipt: never adopt it — install fresh and replace it.
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT HUP INT TERM
  gh release download "$tag" --repo "$REPO" --pattern "$asset" --dir "$tmp"
  gh release download "$tag" --repo "$REPO" --pattern SHA256SUMS --dir "$tmp"
  verified_by=sha256sums
  if command -v minisign >/dev/null 2>&1 && [ -n "${AF_RELEASE_KEY:-}" ]; then
    gh release download "$tag" --repo "$REPO" --pattern SHA256SUMS.minisig --dir "$tmp" >/dev/null 2>&1 \
      || { echo "af: release $tag has no SHA256SUMS.minisig to verify against $AF_RELEASE_KEY" >&2; exit 1; }
    minisign -V -q -m "$tmp/SHA256SUMS" -x "$tmp/SHA256SUMS.minisig" -p "$AF_RELEASE_KEY" \
      || { echo "af: SHA256SUMS of $tag does not verify against $AF_RELEASE_KEY" >&2; exit 1; }
    verified_by=minisign
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
