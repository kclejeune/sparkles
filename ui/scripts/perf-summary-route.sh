#!/bin/sh
# Times the ontology summary route, GET /$/ontology/{ds}, the first time after the server
# starts (cold, when the server builds its model of the ontology) and the next two times
# (warm), with the server's resident memory after the first (ROADMAP item 725).
#
#   SPARKLES_BIN=... sh scripts/perf-summary-route.sh chebi taxon300k taxon1m
#
# It starts the server of scripts/perf-seed.mjs for each dataset, so that one dataset's
# model is not in memory for the next, and stops it afterwards.
set -eu
cd "$(dirname "$0")/.."
: "${SPARKLES_BIN:?SPARKLES_BIN names the server binary}"
PORT="${SPARKLES_SEED_PORT:-18725}"
export SPARKLES_SEED_PORT="$PORT" SPARKLES_PERF_MEM="${PERF_SERVER_MEM:-11G}"
for ds in "$@"; do
  SPARKLES_PERF_DATASETS="$ds" node scripts/perf-seed.mjs > "${TMPDIR:-/tmp}/perf-summary-route.log" 2>&1 &
  server=$!
  for _ in $(seq 1 600); do
    grep -q '^ready' "${TMPDIR:-/tmp}/perf-summary-route.log" && break
    sleep 1
  done
  url="http://127.0.0.1:$PORT/\$/ontology/$ds?graph=default"
  for run in cold warm warm; do
    t=$(curl -s -o /dev/null -w '%{time_total} %{size_download}' "$url")
    echo "$ds $run ${t% *} s, ${t#* } bytes"
    if [ "$run" = cold ]; then
      pid=$(pgrep -P "$server" | head -1 || true)
      [ -n "$pid" ] && echo "$ds server rss after the first read: $(ps -o rss= -p "$pid") KiB"
    fi
  done
  kill "$server" 2> /dev/null || true
  wait "$server" 2> /dev/null || true
done
