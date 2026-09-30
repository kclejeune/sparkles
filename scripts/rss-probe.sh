#!/usr/bin/env bash
# Memory probe for a Sparkles server binary, run against an existing benchmark workdir:
#
#   scripts/rss-probe.sh WORKDIR [NAME]
#
# 1. starts a fresh server on WORKDIR/sparkles.db (result cache off, like bench.sh);
# 2. runs each benchmark query in WORKDIR/queries once and records RSS after them;
# 3. runs 3 rounds of 160 star-join requests from 16 clients, recording RSS after each;
# 4. reports the decoded-block cache size and the peak RSS (VmHWM).
# RSS is read IDLE seconds (default 2) after each step.
#
# With LOAD=1 it first bulk-loads WORKDIR/data.nt into a scratch database and records the
# load's wall time and peak RSS (from getrusage of the child process).
# Results are merged into WORKDIR/results/rss-probe.json under NAME (default "sparkles").
# Env: SPARKLES (binary, default target/release/sparkles), PROBE_PORT (default 3961),
# SERVE_ARGS (extra `sparkles serve` flags, e.g. --idle-release-ms 0), IDLE.
set -euo pipefail

WORK=$(cd "${1:?usage: rss-probe.sh WORKDIR [NAME]}" && pwd)
NAME=${2:-sparkles}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
PORT=${PROBE_PORT:-3961}
cd "$WORK"
mkdir -p results

declare -A OUT
if [ -n "${LOAD:-}" ]; then
  rm -rf probe-load.db
  read -r secs peak < <(python3 - "$SPARKLES" <<'EOF'
import resource, subprocess, sys, time
t = time.monotonic()
subprocess.run([sys.argv[1], "load", "--loc", "probe-load.db", "data.nt"], check=True,
               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
# ru_maxrss is in KiB on Linux
print(f"{time.monotonic() - t:.2f}", resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss // 1024)
EOF
)
  rm -rf probe-load.db
  OUT[load_s]=$secs; OUT[load_peak_mib]=$peak
  echo "load: ${secs}s, peak RSS ${peak} MiB"
fi

if ss -ltn | grep -q ":$PORT "; then echo "port $PORT is in use" >&2; exit 1; fi
rm -rf probe-server
# shellcheck disable=SC2086 # SERVE_ARGS holds extra `serve` flags
"$SPARKLES" --result-cache-mb 0 serve --data probe-server --loc bench="$WORK/sparkles.db" --port "$PORT" ${SERVE_ARGS:-} > probe-server.log 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null; wait $PID 2>/dev/null; rm -rf "$WORK/probe-server"' EXIT
for _ in $(seq 240); do curl -sf "localhost:$PORT/\$/ping" >/dev/null && break; sleep 0.5; done
rss() { awk '/VmRSS/ {printf "%.0f", $2/1024}' "/proc/$PID/status"; }
hwm() { awk '/VmHWM/ {printf "%.0f", $2/1024}' "/proc/$PID/status"; }
q() { curl -sf --max-time 300 -o /dev/null -H 'Accept: text/tab-separated-values' --data-urlencode "query@$1" "localhost:$PORT/bench/sparql"; }

OUT[start_mib]=$(rss)
for f in queries/*.rq; do
  case "$f" in queries/_*) continue ;; esac
  q "$f" || echo "query $f failed" >&2
done
# RSS is read after IDLE seconds without requests: what a container limit sees between
# bursts (the server returns free heap memory to the OS once it is idle)
IDLE=${IDLE:-2}
sleep "$IDLE"; OUT[after_queries_mib]=$(rss)
for round in 1 2 3; do
  export -f q; export PORT
  seq 160 | xargs -P 16 -I{} bash -c 'q queries/star-join.rq'
  sleep "$IDLE"; OUT[after_round${round}_mib]=$(rss)
done
OUT[peak_mib]=$(hwm)
OUT[block_cache_mib]=$(curl -sf "localhost:$PORT/\$/stats/bench" | python3 -c 'import json,sys; print(json.load(sys.stdin)["cache"]["bytes"] >> 20)')

args=(); for k in "${!OUT[@]}"; do args+=("$k=${OUT[$k]}"); done
python3 - results/rss-probe.json "$NAME" "${args[@]}" <<'EOF'
import json, os, sys
f, name, kvs = sys.argv[1], sys.argv[2], sys.argv[3:]
d = json.load(open(f)) if os.path.exists(f) else {}
d[name] = {k: float(v) if "." in v else int(v) for k, v in (kv.split("=", 1) for kv in kvs)}
json.dump(d, open(f, "w"), indent=1, sort_keys=True)
print(name, json.dumps(d[name], sort_keys=True))
EOF
