#!/usr/bin/env bash
set -euo pipefail

# Own the complete gate using source helpers, even beneath an already-installed AF.
scripts_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$scripts_dir/gate-owner.py" bash "$scripts_dir/verify-body.sh" "$@"
