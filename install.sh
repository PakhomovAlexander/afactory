#!/bin/sh
# Install af — the newest stable release, or AF_INSTALL_VERSION — into the self-managed layout:
#   $XDG_DATA_HOME/af/versions/<v>/{af,receipt.toml}   and   $XDG_BIN_HOME/af -> that binary
# Requires the GitHub CLI authenticated with access to the release repository while it is
# private. Never stores a token. Re-running is idempotent; `af self update` takes over from here.
#
#   gh api repos/PakhomovAlexander/afactory/contents/install.sh -H 'Accept: application/vnd.github.raw' | sh
set -eu

REPO="${AF_REPO:-PakhomovAlexander/afactory}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/af/versions"
BIN="${XDG_BIN_HOME:-$HOME/.local/bin}"

case "$(uname -s):$(uname -m)" in
  Darwin:arm64) host="aarch64-apple-darwin" ;;
  Darwin:x86_64) host="x86_64-apple-darwin" ;;
  Linux:x86_64) host="x86_64-unknown-linux-gnu" ;;
  Linux:aarch64|Linux:arm64) host="aarch64-unknown-linux-gnu" ;;
  *) echo "af: unsupported platform $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac

command -v gh >/dev/null 2>&1 || { echo "af: the GitHub CLI is required — fix: install gh, then gh auth login" >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "af: gh is not authenticated — fix: gh auth login (an account with access to $REPO)" >&2; exit 1; }

if [ -n "${AF_INSTALL_VERSION:-}" ]; then
  tag="v${AF_INSTALL_VERSION#v}"
else
  tag=$(gh release list --repo "$REPO" --exclude-pre-releases --exclude-drafts --limit 1 --json tagName -q '.[0].tagName')
  [ -n "$tag" ] || { echo "af: no release found in $REPO" >&2; exit 1; }
fi
version="${tag#v}"
asset="af-${tag}-${host}.tar.gz"

if [ -x "$DATA/$version/af" ]; then
  echo "af $version is already installed at $DATA/$version/af"
else
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT HUP INT TERM
  gh release download "$tag" --repo "$REPO" --pattern "$asset" --dir "$tmp"
  if gh release download "$tag" --repo "$REPO" --pattern SHA256SUMS --dir "$tmp" >/dev/null 2>&1; then
    expected=$(grep " [*]*$asset\$" "$tmp/SHA256SUMS" | awk '{print $1}')
    verified_by=sha256sums
  else
    gh release download "$tag" --repo "$REPO" --pattern "$asset.sha256" --dir "$tmp"
    expected=$(awk '{print $1}' "$tmp/$asset.sha256")
    verified_by=sha256-sidecar
  fi
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
  rm -rf "$DATA/$version" && mv "$staging" "$DATA/$version"
  echo "af $version installed at $DATA/$version/af ($verified_by verified)"
fi

mkdir -p "$BIN"
ln -sfn "$DATA/$version/af" "$BIN/.af.$$" && mv -f "$BIN/.af.$$" "$BIN/af"
echo "af $version is the default: $BIN/af"
case ":$PATH:" in
  *":$BIN:"*) ;;
  *) echo "add $BIN to PATH, e.g.:  export PATH=\"$BIN:\$PATH\"" ;;
esac
echo "next: af self setup-shell --write   (completions + man pages) · af self status"
