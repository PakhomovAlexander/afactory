#!/usr/bin/env bash
# Fetch minisign by exact version and SHA-256 digest, verify it, and print the binary's path.
#
#   scripts/fetch-minisign.sh <dest-dir>
#
# The release workflow signs SHA256SUMS with this binary, so it must be exactly the bytes pinned
# here, and it must be proven before the signing key is anywhere near it: the archive digest is
# checked first, and only then is the extracted binary run — to verify upstream's own signature
# over that archive as a second, independent check. Nothing is fetched by a floating tag or from
# a distribution package. Linux (x86_64, aarch64) and macOS are covered.
#
# The publishing job is Linux, so the Darwin pin would rot unseen if nothing ran it. The
# `make check (macos-latest)` job in .github/workflows/release.yml exercises it — fetch, digest,
# upstream signature, then `make installer-test-signed` with the extracted binary — which is the
# release path itself, checked before anything is published rather than after the pin has rotted.
#
# Digest provenance: the official release assets at
# https://github.com/jedisct1/minisign/releases/tag/0.12 —
#   https://github.com/jedisct1/minisign/releases/download/0.12/minisign-0.12-linux.tar.gz
#   https://github.com/jedisct1/minisign/releases/download/0.12/minisign-0.12-macos.zip
# downloaded on 2026-09-07 and hashed with `shasum -a 256`. The upstream public key is the one
# published at https://jedisct1.github.io/minisign/; both assets' `.minisig` files verify with it.
set -euo pipefail

version="0.12"
upstream_key="RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"
case "$(uname -s)" in
  Linux)
    asset="minisign-$version-linux.tar.gz"
    sha256="9a599b48ba6eb7b1e80f12f36b94ceca7c00b7a5173c95c3efc88d9822957e73"
    unpacker="tar" ;;
  Darwin)
    asset="minisign-$version-macos.zip"
    sha256="89000b19535765f9cffc65a65d64a820f433ef6db8020667f7570e06bf6aac63"
    unpacker="unzip" ;;
  *) echo "fetch-minisign: no pinned minisign asset for $(uname -s)" >&2; exit 1 ;;
esac
# Named per branch, because the macOS asset is a zip and `unzip` is not on every image. Without
# this the failure is `unzip: command not found` from the middle of a verification step.
for program in curl "$unpacker"; do
  command -v "$program" >/dev/null 2>&1 \
    || { echo "fetch-minisign: $program is required to unpack $asset on $(uname -s)" >&2; exit 1; }
done
dest="${1:?usage: scripts/fetch-minisign.sh <dest-dir>}"
mkdir -p "$dest"

digest_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{print $1}'
}

base="https://github.com/jedisct1/minisign/releases/download/$version"
curl -fsSL --retry 3 -o "$dest/$asset" "$base/$asset"
curl -fsSL --retry 3 -o "$dest/$asset.minisig" "$base/$asset.minisig"
actual="$(digest_of "$dest/$asset")"
if [[ "$actual" != "$sha256" ]]; then
  rm -f "$dest/$asset"
  echo "fetch-minisign: $asset is not the pinned asset" >&2
  echo "expected $sha256" >&2
  echo "actual   $actual" >&2
  exit 1
fi

case "$asset" in
  *.tar.gz)
    tar -xzf "$dest/$asset" -C "$dest"
    arch="$(uname -m)"
    case "$arch" in
      x86_64|aarch64) ;;
      *) echo "fetch-minisign: the Linux asset has no $arch binary" >&2; exit 1 ;;
    esac
    bin="$dest/minisign-linux/$arch/minisign" ;;
  *.zip)
    unzip -oq "$dest/$asset" minisign -d "$dest"
    bin="$dest/minisign" ;;
esac
[[ -f "$bin" ]] || { echo "fetch-minisign: $asset does not contain $bin" >&2; exit 1; }
chmod 0755 "$bin"
"$bin" -V -q -P "$upstream_key" -m "$dest/$asset" -x "$dest/$asset.minisig" \
  || { echo "fetch-minisign: upstream's signature over $asset does not verify" >&2; exit 1; }
printf '%s\n' "$bin"
