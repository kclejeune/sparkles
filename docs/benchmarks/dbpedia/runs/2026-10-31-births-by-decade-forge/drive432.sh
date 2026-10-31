#!/usr/bin/env bash
# Item 432: DBpedia 50M slice, births-by-decade, Sparkles (1acdc38a) against QLever on forge.
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
echo "load: $(cat /proc/loadavg) free: $(df -h --output=avail $HOME | tail -1)"
export SPARKLES=$B/bin/1acdc38a SPARKLES_COMMIT=1acdc38a SERVER_MEM_MAX=12G CLIENT_CPU=11
export QSERVER=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-server
export QINDEX=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-index
export PATH=$B/tools:$PATH
nix() { echo 'Nix invocation refused on the timing host' >&2; return 125; }
export -f nix
CACHE=$HOME/.cache/sparkles-bench/billion
mkdir -p "$WORK/50m/results" "$WORK/50m/logs"
ln -sfn "$CACHE/nt" "$WORK/nt"
rm -f "$WORK/50m/data"; mkdir -p "$WORK/50m/data"
cp -al "$CACHE/50m/data/." "$WORK/50m/data/"
touch "$WORK/50m/data/.complete"
data=()
for f in "$WORK"/50m/data/*.nt.zst; do
  data+=(--data-path "$f|CC BY-SA 3.0 and GFDL|DBpedia 2022.12.01 subject slice 50m")
done
python3 "$S/scripts/bench-record.py" manifest --suite dbpedia --out "$WORK/50m/results/manifest.json" \
  --sparkles "$SPARKLES" --binary-commit 1acdc38a --engine qlever="$QSERVER" --workdir "$WORK/50m" \
  --memory-limit "$SERVER_MEM_MAX" --client-cpu "$CLIENT_CPU" "${data[@]}" > /dev/null
export WORKDIR=$WORK SCALE=50m ENGINES="sparkles qlever" QUERIES=births-by-decade COLD=1 WARMUP=2 RUNS=10 TIMEOUT=600 LOAD_TIMEOUT=1h
echo "load $(date -u +%T)"
"$S/scripts/bench-billion.sh" load
echo "load done $(date -u +%T)"
for p in $(seq "$PASSES"); do
  echo "pass $p $(date -u +%T) load $(cut -d' ' -f1-3 /proc/loadavg)"
  "$S/scripts/bench-billion.sh" queries report
  mkdir -p "$B/passes/$p"
  cp "$WORK/50m/results/"*.json "$B/passes/$p/"
  cp "$WORK/50m/logs/"server-*.log "$B/passes/$p/" 2>/dev/null || true
done
python3 "$S/scripts/bench-record.py" finish "$WORK/50m/results/manifest.json"
echo "all done $(date -u +%T)"
