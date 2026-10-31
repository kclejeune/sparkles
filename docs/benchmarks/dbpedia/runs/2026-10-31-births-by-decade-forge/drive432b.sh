#!/usr/bin/env bash
# Item 432, query passes over the stores that drive432.sh loaded.
set -euo pipefail
B=$HOME/lanes/forge-checks-440-432-20261031
S=$B/src/1acdc38a
WORK=$B/billion
PASSES=${PASSES:-5}
exec 8> "$HOME/lanes/.bench.lock"; flock 8
exec 9> "$HOME/lanes/.heavy.lock"; flock 9
echo "locks held $(date -u +%FT%TZ)"
if pgrep -x rustc >/dev/null || pgrep -x cargo >/dev/null || pgrep -f 'sparkles serve|qlever-server|fuseki-server|oxigraph serve|fluree server' >/dev/null; then
  echo "refusing: something else is running"; ps -eo pid,args --sort=-pcpu | head -8 | cut -c1-150; exit 1
fi
export SPARKLES=$B/bin/1acdc38a SPARKLES_COMMIT=1acdc38a SERVER_MEM_MAX=12G CLIENT_CPU=11
export QSERVER=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-server
export QINDEX=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-index
export PATH=$B/tools:$PATH
nix() { echo 'Nix invocation refused on the timing host' >&2; return 125; }
export -f nix
export WORKDIR=$WORK SCALE=50m ENGINES="sparkles qlever" QUERIES=births-by-decade COLD=1 WARMUP=2 RUNS=10 TIMEOUT=600
sync
# let the writeback of the loads and the load average settle
for i in $(seq 60); do
  d=$(awk '/^(Dirty|Writeback):/ {s += $2} END {print s}' /proc/meminfo)
  [ "$d" -lt 2048 ] && break
  sleep 2
done
sleep 90
echo "settled $(date -u +%T) dirty ${d} kB load $(cut -d' ' -f1-3 /proc/loadavg)"
mkdir -p "$B/passes"
rm -rf "$B/passes/"*
for p in $(seq "$PASSES"); do
  echo "pass $p $(date -u +%T) load $(cut -d' ' -f1-3 /proc/loadavg)"
  "$S/scripts/bench-billion.sh" queries
  mkdir -p "$B/passes/$p"
  cp "$WORK/50m/results/"*.json "$B/passes/$p/"
  cp "$WORK/50m/logs/"server-*.log "$B/passes/$p/" 2>/dev/null || true
done
python3 "$S/scripts/bench-record.py" finish "$WORK/50m/results/manifest.json"
echo "all done $(date -u +%T)"
