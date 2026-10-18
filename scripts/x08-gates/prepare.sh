#!/usr/bin/env bash
# Prepares a work directory for the X08 Phase 1 gates
# (docs/specs/X08-disk-spill-and-server-memory.md): the 1.05M triple dataset of
# scripts/gen-data.py, the queries of scripts/bench.sh without export-500k, and a copy of
# scripts/nsc-bench-remote.sh, which latency.sh and a1.sh use.
#
#   scripts/x08-gates/prepare.sh WORK
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
W=${1:?usage: prepare.sh WORK}
mkdir -p "$W/queries"
if [ ! -f "$W/data.nt.zst" ]; then
  python3 "$ROOT/scripts/gen-data.py" 100000 | zstd -q -T0 -10 -o "$W/data.nt.zst"
fi
P=$(sed -n "s/^P='\(.*\)'$/\1/p" "$ROOT/scripts/bench.sh")
while IFS=$'\t' read -r n q; do
  [ "$n" = export-500k ] && continue
  printf '%s%s' "$P" "$q" > "$W/queries/$n.rq"
done < <(sed -n "s/^add \([a-z0-9-]*\) '\(.*\)'$/\1\t\2/p" "$ROOT/scripts/bench.sh")
cp "$ROOT/scripts/nsc-bench-remote.sh" "$W/"
echo "prepared $W"
