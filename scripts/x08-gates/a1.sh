#!/usr/bin/env bash
# The X08 A1 gate: the anonymous memory of a server with ten datasets of 1.05M triples
# against one with a single dataset, at `--cache-mb 256` and 32, after the suite ran
# against every dataset and with no query at all. Each case reads VmRSS, RssAnon, RssFile
# and RssShmem from /proc and the cache bytes from /$/server/memory into WORK/a1/a1.tsv,
# and a1-bound.py applies the gate to that table.
#
#   scripts/x08-gates/a1.sh WORK BIN [REPEATS]   (after prepare.sh WORK)
#
# A1_SERVE_ARGS adds global flags before `serve`, such as `--result-cache-mb 0`.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
USAGE="usage: a1.sh WORK BIN [REPEATS]"
W=$(realpath "${1:?$USAGE}")
BIN=$(realpath "${2:?$USAGE}")
REPEATS=${3:-3}
PORT=${A1_PORT:-4161}
read -r -a EXTRA <<< "${A1_SERVE_ARGS:-}"
O=$W/a1
mkdir -p "$O/stores"
if [ ! -d "$O/stores/s0" ]; then
  zstd -q -d -f "$W/data.nt.zst" -o "$O/data.nt"
  "$BIN" load --loc "$O/stores/s0" "$O/data.nt" > "$O/load.log" 2>&1
  for i in 1 2 3 4 5 6 7 8 9; do cp -a "$O/stores/s0" "$O/stores/s$i"; done
  rm -f "$O/data.nt"
fi
mapfile -t QUERIES < <(cd "$W/queries" && for f in *.rq; do echo "${f%.rq}"; done)
SPID=
trap '[ -n "$SPID" ] && kill "$SPID" 2> /dev/null; true' EXIT
printf 'case\tdatasets\tcache_mb\tsuite\trep\tVmRSS_kB\tRssAnon_kB\tRssFile_kB\tRssShmem_kB\tblock_bytes\tresult_bytes\tservice_bytes\thistory_bytes\tblock_entries\terrors\n' > "$O/a1.tsv"
run_case() { # <datasets> <cache-mb> <suite 0|1> <rep>
  local n=$1 mb=$2 suite=$3 rep=$4 locs=() errs=0 st mem cb
  for i in $(seq 0 $((n - 1))); do locs+=(--loc "ds$i=$O/stores/s$i"); done
  rm -rf "$O/srv"
  "$BIN" "${EXTRA[@]}" serve --data "$O/srv" "${locs[@]}" --port "$PORT" --cache-mb "$mb" \
    --timeout 600 --no-access-log > "$O/server-$n-$mb-$suite-$rep.log" 2>&1 &
  SPID=$!
  for _ in $(seq 600); do
    curl -sf -o /dev/null "localhost:$PORT/\$/ping" && break
    sleep 0.1
  done
  if [ "$suite" = 1 ]; then
    for i in $(seq 0 $((n - 1))); do
      for q in "${QUERIES[@]}"; do
        curl -sS -f --max-time 300 -o /dev/null -H 'Accept: text/tab-separated-values' \
          --data-urlencode "query@$W/queries/$q.rq" "localhost:$PORT/ds$i/sparql" 2> /dev/null || errs=$((errs + 1))
      done
    done
  fi
  # past the idle release of one second
  sleep 2
  st=$(awk '/^(VmRSS|RssAnon|RssFile|RssShmem):/ {printf "%s\t", $2}' "/proc/$SPID/status")
  mem=$(curl -sf "localhost:$PORT/\$/server/memory")
  echo "$mem" > "$O/memory-$n-$mb-$suite-$rep.json"
  cb=$(python3 -c 'import json,sys; c=json.loads(sys.argv[1])["caches"]; print("\t".join(str(c[k].get("bytes",0)) for k in ("block","result","service","history")) + "\t" + str(c["block"].get("entries",0)))' "$mem")
  printf '%s\t%s\t%s\t%s\t%s\t%s%s\t%s\n' "$n-$mb-$suite" "$n" "$mb" "$suite" "$rep" "$st" "$cb" "$errs" >> "$O/a1.tsv"
  kill "$SPID"
  wait "$SPID" 2> /dev/null || true
  SPID=
}
for rep in $(seq "$REPEATS"); do
  for mb in 256 32; do
    for n in 1 10; do
      run_case "$n" "$mb" 0 "$rep"
      run_case "$n" "$mb" 1 "$rep"
    done
  done
done
python3 "$HERE/a1-bound.py" "$O/a1.tsv"
