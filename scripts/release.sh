#!/usr/bin/env bash
# Compatibility entry point; all release preparation and execution belongs to ./dev.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
exec "$root/dev" release rc "$@"
