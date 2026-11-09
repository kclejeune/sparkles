#!/usr/bin/env bash
# Provider-neutral jena screen. Select --executor nsc or --executor boat:bx_ID.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
exec bash "$root/scripts/nsc-jena.sh" "$@"
