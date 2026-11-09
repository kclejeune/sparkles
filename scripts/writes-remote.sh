#!/usr/bin/env bash
# Provider-neutral writes screen. Select --executor nsc or --executor boat:bx_ID.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
exec bash "$root/scripts/nsc-writes.sh" "$@"
