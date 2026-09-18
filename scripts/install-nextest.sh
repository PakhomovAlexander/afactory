#!/usr/bin/env bash
set -euo pipefail
version=0.9.132
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    target=x86_64-unknown-linux-gnu
    digest=e22f14ecaff5519dbfe521e8717d64e9989648bddc23eb1f71bb0053518a52e7 ;;
  Darwin-arm64|Darwin-x86_64)
    target=universal-apple-darwin
    digest=6ce5c844ae3cdac3f6f42fd86bf71a8bf99aecd76bab9f6743c808223d31fad1 ;;
  *) echo "Unsupported nextest host" >&2; exit 1 ;;
esac
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
archive="cargo-nextest-$version-$target.tar.gz"
curl --fail --location --retry 3 \
  "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$version/$archive" \
  --output "$stage/$archive"
(cd "$stage" && printf '%s  %s\n' "$digest" "$archive" | shasum -a 256 -c -)
# Keep the installed tool outside the Cargo dependency/build cache.
mkdir -p "$stage/bin"
tar -xzf "$stage/$archive" -C "$stage/bin" ./cargo-nextest
install_dir="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/af-ci-tools"
mkdir -p "$install_dir"
install "$stage/bin/cargo-nextest" "$install_dir/cargo-nextest"
if [[ -n "${GITHUB_PATH:-}" ]]; then
  echo "$install_dir" >> "$GITHUB_PATH"
fi
"$install_dir/cargo-nextest" --version
