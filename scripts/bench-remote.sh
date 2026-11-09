#!/usr/bin/env bash
# Provider-neutral bench screen. Select --executor nsc or --executor boat:bx_ID.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
exec bash "$root/scripts/nsc-bench.sh" "$@"
