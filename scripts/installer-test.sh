#!/usr/bin/env bash
# install.sh end to end against a local fake release. Hermetic: no network, no real gh, no real
# release, nothing written outside a temporary root.
#
#   scripts/installer-test.sh [--signed] <af-binary>          # make installer-test[-signed]
#
# The binary under test is packaged as fake releases the way the release workflow packages one
# (archive + SHA256SUMS, the installer listed too), served by a `gh` double that reads the
# directory layout `af self` itself accepts (AF_RELEASE_SOURCE: <root>/<tag>/<asset>). Proved:
# the script parses; the newest release by semantic version installs, checksum-verified and
# receipted, and is activated through the installed binary; a wrong checksum, an unlisted
# asset, a binary reporting another version, and a version below the self-management floor are
# refused and leave nothing behind; a rerun is idempotent. With --signed, minisign
# (MINISIGN=<path>, or on PATH — absent is a failure, never a skip) signs SHA256SUMS the way the
# release job does: a valid signature is recorded in the receipt; a forged, tampered, or
# missing one is refused.
set -euo pipefail

signed=0
if [[ "${1:-}" == "--signed" ]]; then
  signed=1
  shift
fi
af="${1:?usage: scripts/installer-test.sh [--signed] <af-binary>}"
case "$af" in
  /*) ;;
  *) af="$(pwd -P)/$af" ;;
esac
[[ -x "$af" ]] || { echo "installer-test: not an executable: $af" >&2; exit 2; }
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
installer="$root/install.sh"

minisign=""
if [[ "$signed" -eq 1 ]]; then
  minisign="${MINISIGN:-$(command -v minisign || true)}"
  [[ -n "$minisign" && -x "$minisign" ]] || {
    echo "installer-test: --signed needs minisign (MINISIGN=<path>, or on PATH); scripts/fetch-minisign.sh <dir> fetches the pinned one" >&2
    exit 1
  }
fi

case "$(uname -s):$(uname -m)" in
  Darwin:arm64) host="aarch64-apple-darwin" ;;
  Linux:x86_64) host="x86_64-unknown-linux-musl" ;;
  Linux:aarch64|Linux:arm64) host="aarch64-unknown-linux-musl" ;;
  *) echo "installer-test: install.sh installs no release on $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac
version="$(AF_SELF_OFFLINE=1 "$af" --version | awk '{print $2}')"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "installer-test: $af --version reported '$version'" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

digest_of() {
  { command -v sha256sum >/dev/null 2>&1 && sha256sum "$1" || shasum -a 256 "$1"; } | awk '{print $1}'
}

# ------------------------------------------------------------------ the gh double
shim="$work/shim"
mkdir -p "$shim"
cat > "$shim/gh" <<'GH'
#!/bin/sh
# A gh double: releases live at $AF_RELEASE_SOURCE/<tag>/<asset>, the layout `af self` reads.
set -eu
src="${AF_RELEASE_SOURCE:?}"
case "${1:-} ${2:-}" in
  "auth status") exit 0 ;;
  "release list")
    for dir in "$src"/v*/; do
      [ -d "$dir" ] || continue
      tag="$(basename "$dir")"
      case "$tag" in
        *-*) ;;            # --exclude-pre-releases
        *) echo "$tag" ;;
      esac
    done
    exit 0 ;;
  "release download")
    tag="$3"
    shift 3
    pattern=""
    dir="."
    while [ $# -gt 0 ]; do
      case "$1" in
        --pattern) pattern="$2"; shift 2 ;;
        --dir) dir="$2"; shift 2 ;;
        --repo) shift 2 ;;
        --clobber) shift ;;
        *) echo "gh double: unsupported option $1" >&2; exit 64 ;;
      esac
    done
    [ -f "$src/$tag/$pattern" ] || { echo "gh double: release $tag has no asset $pattern" >&2; exit 1; }
    cp "$src/$tag/$pattern" "$dir/"
    exit 0 ;;
esac
echo "gh double: unsupported command: $*" >&2
exit 64
GH
chmod +x "$shim/gh"
[[ -z "$minisign" ]] || ln -s "$minisign" "$shim/minisign"

# ------------------------------------------------------------------ fake releases
# The signing keys, when the signature path is under test. `keyenv` is what every install gets.
keyenv=()
if [[ "$signed" -eq 1 ]]; then
  mkdir -p "$work/keys"
  for name in release other; do
    "$minisign" -G -W -f -p "$work/keys/$name.pub" -s "$work/keys/$name.key" >/dev/null 2>&1
  done
  keyenv=(AF_RELEASE_KEY="$work/keys/release.pub")
fi

# publish <case> <version> <binary> [ok|tampered|unlisted]: <case>/releases/v<version>/{archive,SHA256SUMS}
publish() {
  local case_dir="$1" v="$2" binary="$3" mode="${4:-ok}"
  local asset="af-v$v-$host.tar.gz" dir="$case_dir/releases/v$v" stage real
  stage="$(mktemp -d "$work/stage.XXXXXX")"
  mkdir -p "$dir"
  cp "$binary" "$stage/af"
  chmod 0755 "$stage/af"
  tar -czf "$dir/$asset" -C "$stage" af
  real="$(digest_of "$dir/$asset")"
  case "$mode" in
    ok) printf '%s  %s\n%s  install.sh\n' "$real" "$asset" "$(digest_of "$installer")" > "$dir/SHA256SUMS" ;;
    tampered) printf '%064d  %s\n' 0 "$asset" > "$dir/SHA256SUMS" ;;
    unlisted) printf '%s  af-v%s-other-target.tar.gz\n' "$real" "$v" > "$dir/SHA256SUMS" ;;
  esac
}

# sign <case> <version> <key-name>: SHA256SUMS.minisig the way the release job writes it
sign() {
  "$minisign" -S -W -s "$work/keys/$3.key" -m "$1/releases/v$2/SHA256SUMS" -t "af v$2" >/dev/null
}

# fake_af <version>: a script answering --version the way a release binary does
fake_af() {
  local path="$work/fake-af-$1"
  printf '#!/bin/sh\nif [ "${1:-}" = "--version" ]; then echo "af %s"; exit 0; fi\necho "fake af %s $*" >&2\n' "$1" "$1" > "$path"
  chmod 0755 "$path"
  printf '%s' "$path"
}

# sandboxed <case> <program> [args...]: run under XDG directories rooted at <case>, the gh
# double first on PATH, and nothing inherited from this shell.
sandboxed() {
  local case_dir="$1"
  shift
  mkdir -p "$case_dir/home"
  env -i PATH="$shim:/usr/bin:/bin" HOME="$case_dir/home" \
    XDG_CONFIG_HOME="$case_dir/config" XDG_DATA_HOME="$case_dir/data" XDG_STATE_HOME="$case_dir/state" \
    XDG_CACHE_HOME="$case_dir/cache" XDG_BIN_HOME="$case_dir/bin" \
    AF_RELEASE_SOURCE="$case_dir/releases" NO_COLOR=1 ${keyenv[@]+"${keyenv[@]}"} "$@"
}

run_installer() {
  local case_dir="$1"
  shift
  sandboxed "$case_dir" "$@" sh "$installer"
}

passed=0
ok() {
  passed=$((passed + 1))
  echo "ok   $1"
}
fail() {
  echo "FAIL $1" >&2
  [[ $# -lt 2 ]] || printf '%s\n' "$2" >&2
  exit 1
}

# expect_refusal <name> <case> <message> [VAR=value ...]
expect_refusal() {
  local name="$1" case_dir="$2" message="$3" output
  shift 3
  if output="$(run_installer "$case_dir" "$@" 2>&1)"; then
    fail "$name: install.sh succeeded" "$output"
  fi
  [[ "$output" == *"$message"* ]] || fail "$name: expected '$message'" "$output"
  [[ ! -e "$case_dir/data/af/versions" && ! -e "$case_dir/bin/af" ]] || fail "$name: something was installed" "$output"
  ok "$name"
}

# ------------------------------------------------------------------ the cases
sh -n "$installer" || fail "install.sh does not parse"
ok "install.sh parses"

# The newest release by semantic version wins over an older one and a pre-release.
happy="$work/happy"
publish "$happy" "0.7.1" "$(fake_af 0.7.1)"
publish "$happy" "$version" "$af"
mkdir -p "$happy/releases/v9.9.9-rc.1"
if [[ "$signed" -eq 1 ]]; then
  sign "$happy" "0.7.1" release
  sign "$happy" "$version" release
  verified_by="minisign"
else
  verified_by="sha256sums"
fi
output="$(run_installer "$happy" 2>&1)" || fail "install" "$output"
[[ "$output" == *"af $version installed at"* ]] || fail "install: no installed line" "$output"
receipt="$happy/data/af/versions/$version/receipt.toml"
[[ -f "$receipt" ]] || fail "install: no receipt at $receipt" "$output"
for line in "version = \"$version\"" "target = \"$host\"" "verified_by = \"$verified_by\""; do
  grep -qx "$line" "$receipt" || fail "install: receipt lacks $line" "$(cat "$receipt")"
done
ok "the newest release installs, verified by $verified_by, with a receipt"

[[ "$(readlink "$happy/bin/af")" == "$happy/data/af/versions/$version/af" ]] \
  || fail "activation: bin/af does not link to the installed binary" "$(ls -l "$happy/bin" 2>&1)"
status="$(sandboxed "$happy" env AF_SELF_OFFLINE=1 "$happy/data/af/versions/$version/af" self status --json 2>&1)" \
  || fail "activation: af self status failed" "$status"
[[ "$status" == *"\"default\": \"$version\""* ]] || fail "activation: not the default" "$status"
ok "the installed binary activated itself and reports itself as the default"

output="$(run_installer "$happy" 2>&1)" || fail "rerun" "$output"
[[ "$output" == *"already installed"* ]] || fail "rerun: not idempotent" "$output"
ok "a rerun is idempotent"

case_dir="$work/tampered"
publish "$case_dir" "$version" "$af" tampered
[[ "$signed" -eq 0 ]] || sign "$case_dir" "$version" release
expect_refusal "a wrong checksum is refused" "$case_dir" "checksum mismatch" AF_INSTALL_VERSION="$version"

case_dir="$work/unlisted"
publish "$case_dir" "$version" "$af" unlisted
[[ "$signed" -eq 0 ]] || sign "$case_dir" "$version" release
expect_refusal "an asset SHA256SUMS does not list is refused" "$case_dir" "checksum mismatch" AF_INSTALL_VERSION="$version"

case_dir="$work/wrong-version"
publish "$case_dir" "$version" "$(fake_af 0.0.1)"
[[ "$signed" -eq 0 ]] || sign "$case_dir" "$version" release
expect_refusal "a binary reporting another version is refused" "$case_dir" "release binary reported" AF_INSTALL_VERSION="$version"

case_dir="$work/floor"
publish "$case_dir" "0.7.0" "$(fake_af 0.7.0)"
[[ "$signed" -eq 0 ]] || sign "$case_dir" "0.7.0" release
expect_refusal "a release below the self-management floor is refused" "$case_dir" "predates self-management" AF_INSTALL_VERSION=0.7.0

if [[ "$signed" -eq 1 ]]; then
  case_dir="$work/forged"
  publish "$case_dir" "$version" "$af"
  sign "$case_dir" "$version" other
  expect_refusal "a signature by another key is refused" "$case_dir" "does not verify" AF_INSTALL_VERSION="$version"

  case_dir="$work/tampered-after-signing"
  publish "$case_dir" "$version" "$af"
  sign "$case_dir" "$version" release
  printf '%064d  af-v%s-other-target.tar.gz\n' 1 "$version" >> "$case_dir/releases/v$version/SHA256SUMS"
  expect_refusal "checksums changed after signing are refused" "$case_dir" "does not verify" AF_INSTALL_VERSION="$version"

  case_dir="$work/unsigned"
  publish "$case_dir" "$version" "$af"
  expect_refusal "a release without SHA256SUMS.minisig is refused" "$case_dir" "has no SHA256SUMS.minisig" AF_INSTALL_VERSION="$version"
fi

if [[ "$signed" -eq 1 ]]; then
  echo "installer-test: $passed checks passed, signature path included"
else
  echo "installer-test: $passed checks passed (signature path: make installer-test-signed)"
fi
