#!/usr/bin/env bash
# The X08 Phase 1 latency gate for queries under 0.3 ms. One build runs with the query
# memory pool on and with `--total-query-memory-mb 0`, both from one shared store, in both
# orders and once against itself, through scripts/nsc-bench-remote.sh on this host. Then
# combine.py estimates the pool's effect with the position bias cancelled and applies the
# gate.
#
#   scripts/x08-gates/latency.sh WORK BIN [ROUNDS] [REPS]   (after prepare.sh WORK)
#
# Run it on a quiet host while holding the host's benchmark lock. It pins the servers to
# CPU 2 and the client to CPU 4.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
USAGE="usage: latency.sh WORK BIN [ROUNDS] [REPS]"
W=$(realpath "${1:?$USAGE}")
BIN=$(realpath "${2:?$USAGE}")
ROUNDS=${3:-20}
REPS=${4:-300}
QS="distinct-obj count-all predicate-counts types-grouped path-plus values-star employee-docs star-lookup subquery-agg minus"
run() { # <name> <flags of A> <flags of B>
  local d=$W/runs/$1
  rm -rf "$d"
  mkdir -p "$d/queries"
  for q in $QS; do cp "$W/queries/$q.rq" "$d/queries/"; done
  zstd -q -1 -f "$BIN" -o "$d/bin-A.zst"
  cp "$d/bin-A.zst" "$d/bin-B.zst"
  ln -s "$W/data.nt.zst" "$d/data.nt.zst"
  cp "$W/nsc-bench-remote.sh" "$d/"
  # the build runs on the host it was built for, so it needs no other loader
  cat > "$d/run.env" << ENV
DYNAMIC_A=0
DYNAMIC_B=0
ROUNDS=$ROUNDS
REPS=$REPS
WARMUP=3
RAYON_THREADS=1
SERVER_CPU=2
CLIENT_CPU=4
SPIN=1
PORT=4171
SERVE_ARGS_A="$2"
SERVE_ARGS_B="$3"
SHARE_STORE=1
ENV
  echo "$1: $(uptime)"
  bash "$d/nsc-bench-remote.sh" > /dev/null
  rm -rf "$d/stores" "$d/srv" "$d/data.nt" "$d"/bin-*.zst
}
run pool-on-second "--total-query-memory-mb 0" ""
run pool-on-first "" "--total-query-memory-mb 0"
run same "" ""
python3 "$HERE/combine.py" "$W/runs/pool-on-second/results/samples.tsv" \
  "$W/runs/pool-on-first/results/samples.tsv" "$W/runs/same/results/samples.tsv"
