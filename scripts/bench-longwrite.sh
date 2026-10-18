#!/usr/bin/env bash
# Sustained writes with compaction (docs/specs/X06 section 3.9): hours of concurrent reads and
# writes against one engine at a time, on Sparkles, Apache Jena (TDB2 + Fuseki), QLever,
# Fluree and Oxigraph, with a per-minute time series, the compactions Sparkles ran, a final
# state check and a check after a restart.
#
#   scripts/bench-longwrite.sh [DATA] [WORKDIR]
#
# DATA is a person count for scripts/gen-data.py (default 1000000, 10.5M triples), an
# N-Triples file, or a directory of .nt.zst files such as a scale directory of
# scripts/bench-billion.sh (WORKDIR/<scale>/data, for example the 50m slice), which is
# decompressed into WORKDIR/data.nt once. Every engine loads the same file, as in
# scripts/bench.sh, and then, one engine at a time:
#
# 1. its loaded store is copied (reflinks where the file system has them) and served;
# 2. scripts/bench-longwrite/longrun.py runs READERS readers (default 16) and WRITERS writers
#    (default 1) for SECONDS (default 14400, four hours). The readers run the queries of
#    READ_QUERIES in rotation, and the writers commit inserts and deletes of single triples
#    and of BATCH (default 100) triples with a fixed seed (SEED), at WRITE_RATE requests per
#    second in all (default 0: as fast as the engine answers). Each writer deletes only above
#    LIVE_MAX (default 200000) of its own triples, so the data stays near its size while every
#    commit leaves work for compaction;
# 3. the state the writers were told about is checked against the engine's answer, the
#    server is stopped and started again (the time to its first answer is recorded), and the
#    state is checked again.
#
# Sparkles runs with automatic compaction at its defaults. The other engines run with the
# settings of scripts/bench.sh and their own background maintenance. TDB2 is not compacted:
# its compaction (tdb2.tdbcompact) needs the server stopped or an administrative request, and
# the run reports its disk growth instead. QLever keeps updates in memory, so its state after
# a restart is the loaded data, which the check reports as missing triples. Every result
# carries the durability class of docs/specs/X06 section 5.1, and the device flushes per write
# request confirm it (/proc/diskstats of the device that holds the store).
#
# Results go to WORKDIR/results: longwrite.json (per engine: the series, the compactions, the
# checks, the time to restart), summary.md, manifest.json and record.json.
#
# Env: ENGINES (default the five), SECONDS_RUN (default 14400), READERS, WRITERS, BATCH,
# LIVE_MAX, WRITE_RATE, SEED, READ_QUERIES (names; default every query of the set),
# QUERY_SET (synthetic or dbpedia; default synthetic for a person count, dbpedia otherwise),
# SAMPLE (seconds between samples, default 5), SKIP_LOAD=1, PORT_BASE (default 48400),
# SPARKLES, SPARKLES_ARGS, SERVER_MEM_MAX, CLIENT_CPU, FLUREE and FLUREE_VERSION as in
# scripts/bench.sh. CLIENT_CPUS (default the last quarter of the CPUs) pins the load
# generator, so it does not compete with the server for the same cores.
set -euo pipefail

DATA=${1:-1000000}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${2:-$ROOT/target/bench-longwrite}
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
ENGINES=${ENGINES-sparkles jena qlever fluree oxigraph}
PORT_BASE=${PORT_BASE:-48400}
SECONDS_RUN=${SECONDS_RUN:-14400}
MAX_TIME=${MAX_TIME:-120}
export MAX_TIME

mkdir -p "$WORK/results" "$WORK/queries" "$WORK/logs"
WORK=$(cd "$WORK" && pwd)
case $DATA in /*) ;; *) [[ $DATA =~ ^[0-9]+$ ]] || DATA=$(realpath "$DATA") ;; esac
cd "$WORK"
# shellcheck source=scripts/bench-lib.sh
source "$ROOT/scripts/bench-lib.sh"
RES=results

# ------------------------------------------------------------------------------ data
if [[ $DATA =~ ^[0-9]+$ ]]; then
  QUERY_SET=${QUERY_SET:-synthetic}
  if [ ! -f data.nt ]; then
    log "generating the synthetic data ($DATA people)"
    python3 "$ROOT/scripts/gen-data.py" "$DATA" > data.nt.tmp
    mv data.nt.tmp data.nt
  fi
  SCALE=$DATA
else
  QUERY_SET=${QUERY_SET:-dbpedia}
  SCALE=$(basename "$DATA")
  if [ ! -f data.nt ]; then
    if [ -d "$DATA" ]; then
      log "decompressing $DATA/*.nt.zst into data.nt"
      find "$DATA" -name '*.nt.zst' -print0 | sort -z | xargs -0 zstd -dcq > data.nt.tmp
      mv data.nt.tmp data.nt
    elif [ -f "$DATA" ]; then
      ln -sf "$DATA" data.nt
    else
      die "no data at $DATA"
    fi
  fi
fi
log "data: $(wc -l < data.nt) triples"

# ---------------------------------------------------------------------------- queries
rm -f queries/*.rq
case $QUERY_SET in
  synthetic)
    awk -v dir=queries '
      /^## / { if (out) close(out); out = dir "/" $2 ".rq"; next }
      /^#/ { next }
      out { print > out }' "$ROOT/scripts/bench-longwrite/synthetic.txt"
    ;;
  dbpedia)
    for n in entity-facts-1 entity-facts-2 entity-summary-1 inlinks-count-1 redirect-target-1 place-births-1 \
      category-people-1 outlink-classes-1 film-director-optional place-union-1; do
      cp "$ROOT/scripts/bench-billion/queries/$n.rq" queries/
    done
    ;;
  *) die "QUERY_SET must be synthetic or dbpedia" ;;
esac
if [ -n "${READ_QUERIES:-}" ]; then
  NAMES=$READ_QUERIES
else
  NAMES=$(find queries -name '*.rq' -printf '%f\n' | sed 's/\.rq$//' | sort | tr '\n' ' ')
fi
for n in $NAMES; do [ -f "queries/$n.rq" ] || die "no query $n in the $QUERY_SET set"; done

# ------------------------------------------------------------------------- the runs
DATAFILE=data.nt
LOADRES=$RES
resolve_tools
trap stop_all EXIT
args=(--out "$RES/manifest.json" --suite longwrite --sparkles "$SPARKLES" --workdir "$WORK"
  --memory-limit "${SERVER_MEM_MAX:-}" --client-cpu "${CLIENT_CPUS:-}"
  --data "longwrite-$SCALE=$WORK/data.nt|see the source|$QUERY_SET data")
has jena && args+=(--engine "jena=$FUSEKI")
has qlever && args+=(--engine "qlever=$QSERVER")
has oxigraph && args+=(--engine "oxigraph=$OXIGRAPH")
has fluree && args+=(--engine "fluree=$FLUREE")
python3 "$ROOT/scripts/bench-record.py" manifest "${args[@]}" > /dev/null
[ -n "${SKIP_LOAD:-}" ] || load_all

# the load generator's CPUs: the last quarter, away from the servers' first cores
if [ -z "${CLIENT_CPUS:-}" ] && command -v taskset > /dev/null; then
  n=$(nproc)
  CLIENT_CPUS="$((n - n / 4))-$((n - 1))"
fi
GEN=()
[ -n "${CLIENT_CPUS:-}" ] && [ "$CLIENT_CPUS" != none ] && GEN=(taskset -c "$CLIENT_CPUS")

STORE=$WORK/scratch
all=$ENGINES
for e in $all; do
  ENGINES=$e
  log "$e: copying its store"
  copy_stores
  start "$e"
  fields=()
  [ -n "${UPDFIELD[$e]:-}" ] && fields=(--field "${UPDFIELD[$e]}")
  comp=()
  [ "$e" = sparkles ] && comp=(--compaction-url "localhost:${PORT[$e]}/\$/stats/bench")
  out=$RES/run-$e.json
  log "$e: $SECONDS_RUN s of ${READERS:-16} readers and ${WRITERS:-1} writers"
  "${GEN[@]}" python3 "$ROOT/scripts/bench-longwrite/longrun.py" run --query-url "${URL[$e]}" \
    --update-url "${UPDURL[$e]}" "${fields[@]}" --queries queries --names "$NAMES" --seconds "$SECONDS_RUN" \
    --readers "${READERS:-16}" --writers "${WRITERS:-1}" --batch "${BATCH:-100}" --live-max "${LIVE_MAX:-200000}" \
    --write-rate "${WRITE_RATE:-0}" --seed "${SEED:-20261018}" --sample "${SAMPLE:-5}" --port "${PORT[$e]}" \
    --store "$(store_of "$e")" "${comp[@]}" --read-timeout "$MAX_TIME" --out "$out" | tee "logs/longwrite-$e.log"
  check=$(python3 "$ROOT/scripts/bench-longwrite/longrun.py" check --query-url "${URL[$e]}" "${fields[@]}" --state "$out") || true
  log "$e: state check $check"
  stop "$e"
  start "$e"
  restart_s=${READY_S[$e]}
  check2=$(python3 "$ROOT/scripts/bench-longwrite/longrun.py" check --query-url "${URL[$e]}" "${fields[@]}" --state "$out") || true
  log "$e: restart in $restart_s s, state check $check2"
  stop "$e"
  python3 -I - "$RES/longwrite.json" "$e" "$out" "${check:-null}" "${check2:-null}" "$restart_s" << 'EOF'
import json, os, sys
f, e, run, c1, c2, ready = sys.argv[1:]
dur = {"sparkles": "synced", "jena": "synced", "fluree": "synced", "oxigraph": "written", "qlever": "memory"}
d = json.load(open(f)) if os.path.exists(f) else {}
r = json.load(open(run))
r.pop("state", None)
load = lambda s: json.loads(s) if s and s != "null" else None
r.update({"durability": dur.get(e), "check": load(c1), "check_reopen": load(c2), "restart_ready_s": float(ready)})
d[e] = r
json.dump(d, open(f, "w"), indent=1)
EOF
  [ -n "${KEEP_SCRATCH:-}" ] || rm -rf "$STORE"
done
ENGINES=$all
python3 "$ROOT/scripts/bench-record.py" finish "$RES/manifest.json"
python3 "$ROOT/scripts/bench-longwrite/longrun.py" summary "$RES/longwrite.json" > "$RES/summary.md"
python3 "$ROOT/scripts/bench-record.py" build --suite longwrite --scale "$SCALE" --results "$RES" \
  --manifest "$RES/manifest.json" --out "$RES/record.json" --queries --timer longrun.py
log "record: $WORK/$RES/record.json, table: $WORK/$RES/summary.md"
