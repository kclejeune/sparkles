#!/usr/bin/env bash
# Sustained writes with compaction (docs/specs/X06 section 3.9): an hour or more of concurrent
# reads and writes against one engine at a time, on Sparkles, Apache Jena (TDB2 + Fuseki),
# QLever, Fluree and Oxigraph, with a per-minute time series, the compactions Sparkles ran,
# and state checks at the end, after a SIGKILL and after a normal restart.
#
#   scripts/bench-longwrite.sh [DATA] [WORKDIR]
#
# DATA is a person count for scripts/gen-data.py (default 1000000, 10.5M triples), an
# N-Triples file, or a directory of .nt.zst files such as a scale directory of
# scripts/bench-billion.sh (WORKDIR/<scale>/data, for example the 50m slice), which is
# decompressed into WORKDIR/data.nt once. A workdir keeps the data it was made with and
# refuses other data. Every engine loads the same file, as in scripts/bench.sh. Then every
# engine answers the read set on its loaded store, one at a time, and the answers are
# compared: an engine whose answers differ from the majority is not run and is recorded as
# answer-disagreement, and one that fails a query is recorded as error, unless IGNORE_ANSWERS=1. Then, one engine at a time:
#
# 1. its loaded store is copied (reflinks where the file system has them) and served;
# 2. scripts/bench-longwrite/longrun.py runs READERS readers (default 16) and WRITERS writers
#    (default 1) for SECONDS_RUN (default 3600). The readers run the queries of READ_QUERIES
#    in rotation, and the writers commit inserts and deletes of single triples and of BATCH
#    (default 100) triples with a fixed seed (SEED), at WRITE_RATE requests per second in all
#    (default 0: as fast as the engine answers). Each writer deletes only above LIVE_MAX
#    (default 200000) of its own triples, so the data stays near its size while every commit
#    leaves work for compaction. The load generator runs in its own session under a time
#    limit, and the script's exit trap kills that session;
# 3. the state the writers were told about is checked against the engine's answer. Then the
#    server is killed with SIGKILL, started again and checked, which exercises the recovery
#    of the engine's log, and finally stopped normally, started again and checked. Each
#    restart records the time from the start to the first answered query.
#
# Sparkles runs with automatic compaction at its defaults. The other engines run with the
# settings of scripts/bench.sh and their own background maintenance. TDB2 is not compacted,
# because its compaction (tdb2.tdbcompact) needs the server stopped or an administrative
# request, and the run reports its disk growth instead. QLever keeps updates in memory, so its
# state after a restart is the loaded data, which its documented durability class allows.
# Every result carries the durability class of docs/specs/X06 section 5.1, and the disk
# flushes per write request show it. `longrun.py merge` gives each engine a status: `error`
# when a check failed, no write succeeded, more than 1% of the requests failed, the run
# stopped early, or a synced or written engine lost acknowledged writes in a restart.
#
# Results go to WORKDIR/results: longwrite.json (per engine: the series, the compactions, the
# checks, the restart times, the status), summary.md, answers.json, manifest.json and
# record.json.
#
# Env: ENGINES (default the five), SECONDS_RUN (default 3600), READERS, WRITERS, BATCH,
# LIVE_MAX, WRITE_RATE, SEED, READ_QUERIES (names; default every query of the set),
# QUERY_SET (synthetic or dbpedia; default synthetic for a person count, dbpedia otherwise),
# SAMPLE (seconds between samples, default 5), SKIP_LOAD=1, IGNORE_ANSWERS=1, PORT_BASE
# (default 28400), SPARKLES, SPARKLES_ARGS, SERVER_MEM_MAX, FLUREE and FLUREE_VERSION as in
# scripts/bench.sh. CLIENT_CPUS pins the load generator (default the last quarter of the
# CPUs on a host with 4 or more, none otherwise; "none" turns it off). The servers are not
# pinned, so the pinning only keeps the generator's own processes together.
set -euo pipefail

DATA=${1:-1000000}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${2:-$ROOT/target/bench-longwrite}
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
ENGINES=${ENGINES-sparkles jena qlever fluree oxigraph}
PORT_BASE=${PORT_BASE:-28400}
SECONDS_RUN=${SECONDS_RUN:-3600}
MAX_TIME=${MAX_TIME:-120}
export MAX_TIME

mkdir -p "$WORK/results" "$WORK/queries" "$WORK/logs"
WORK=$(cd "$WORK" && pwd)
case $DATA in /*) ;; *) [[ $DATA =~ ^[0-9]+$ ]] || DATA=$(realpath "$DATA") ;; esac
cd "$WORK"
# shellcheck source=scripts/bench-lib.sh
source "$ROOT/scripts/bench-lib.sh"
RES=results
LW=$ROOT/scripts/bench-longwrite/longrun.py

# ------------------------------------------------------------------------------ data
if [ -f data.label ] && [ "$(cat data.label)" != "$DATA" ]; then
  die "$WORK holds the data of $(cat data.label), not $DATA; use another workdir"
fi
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
echo "$DATA" > data.label
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
  read -r -a NAMES <<< "$READ_QUERIES"
else
  mapfile -t NAMES < <(find queries -name '*.rq' -printf '%f\n' | sed 's/\.rq$//' | sort)
fi
for n in "${NAMES[@]}"; do [ -f "queries/$n.rq" ] || die "no query $n in the $QUERY_SET set"; done
selected() { true; }

# ------------------------------------------------------------------- generator CPUs
if [ -z "${CLIENT_CPUS:-}" ] && command -v taskset > /dev/null; then
  n=$(nproc)
  if [ "$n" -ge 4 ]; then CLIENT_CPUS="$((n - n / 4))-$((n - 1))"; else CLIENT_CPUS=none; fi
fi
GEN=()
[ -n "${CLIENT_CPUS:-}" ] && [ "$CLIENT_CPUS" != none ] && GEN=(taskset -c "$CLIENT_CPUS")

# ------------------------------------------------------------------- load and answers
DATAFILE=data.nt
LOADRES=$RES
resolve_tools
GENPID=""
# the servers, and the load generator's whole session
cleanup() {
  [ -n "$GENPID" ] && { kill -KILL -- "-$GENPID" 2> /dev/null || true; }
  stop_all
}
trap cleanup EXIT
args=(--out "$RES/manifest.json" --suite longwrite --sparkles "$SPARKLES" --workdir "$WORK"
  --memory-limit "${SERVER_MEM_MAX:-}" --client-cpu "${CLIENT_CPUS:-}"
  --data "longwrite-$SCALE=$WORK/data.nt|see the source|$QUERY_SET data")
has jena && args+=(--engine "jena=$FUSEKI")
has qlever && args+=(--engine "qlever=$QSERVER")
has oxigraph && args+=(--engine "oxigraph=$OXIGRAPH")
has fluree && args+=(--engine "fluree=$FLUREE")
python3 "$ROOT/scripts/bench-record.py" manifest "${args[@]}" > /dev/null
[ -n "${SKIP_LOAD:-}" ] || load_all

# the read set's answers on every engine's loaded store, one engine at a time
rm -f "$RES/answers.json"
COUNT_ONLY=${COUNT_ONLY:-}
NO_CHECK=${NO_CHECK:-}
all=$ENGINES
for e in $all; do
  ENGINES=$e
  start "$e"
  answer_check
  stop "$e"
done
ENGINES=$all
# By the agreement rule of the record (scripts/bench-record.py fingerprints), one line per
# engine that failed a query of the read set ("error <engine> <query>") or whose answer
# differs from the majority ("wrong <engine> <query>"), and one per query on which two or
# more engines disagree with no majority ("nomajority <query>"), which needs three engines.
verdicts=$(
  python3 -I - "$ROOT/scripts/bench-record.py" "$RES/answers.json" << 'EOF'
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("bench_record", sys.argv[1])
br = importlib.util.module_from_spec(spec)
spec.loader.exec_module(br)
with open(sys.argv[2]) as f:
    ans = json.load(f)
fp = br.fingerprints(ans)
for q, per in sorted(ans.items()):
    hashes = set()
    for e in sorted(per):
        v = fp.get((q, br.engine_name(e)))
        if v is None:
            print("error", e, q)
        else:
            hashes.add(v["hash"])
            if v["agrees"] is False:
                print("wrong", e, q)
    if len(hashes) > 1 and all((fp.get((q, br.engine_name(e))) or {}).get("agrees") is None for e in per):
        print("nomajority", q)
EOF
) || die "could not compare the answers of the read set"
declare -A VERDICT=()
while read -r kind a b; do
  case $kind in
    error) [ -n "${VERDICT[$a]:-}" ] || VERDICT[$a]="error:failed $b" ;;
    wrong) VERDICT[$a]="answer-disagreement:differs from the majority on $b" ;;
    nomajority) log "the engines disagree on $a with no majority; every engine runs" ;;
  esac
done <<< "$verdicts"
for k in "${!VERDICT[@]}"; do log "$k: ${VERDICT[$k]#*:} of the read set"; done

# --------------------------------------------------------------------------- the runs
# check <engine> <run file>: the state check's JSON, or a failure object
check() {
  local fields=()
  [ -n "${UPDFIELD[$1]:-}" ] && fields=(--field "${UPDFIELD[$1]}")
  python3 "$LW" check --query-url "${URL[$1]}" "${fields[@]}" --state "$2" || true
}
# restart <engine>: start the server in a subshell, so a failed start is recorded rather than
# ending the run; RESTART_S gets the seconds from the start to the first answered query
restart() {
  local e=$1 t0 t1
  t0=$(date +%s.%N)
  if ! (start "$e") > "logs/restart-$e.log" 2>&1; then
    RESTART_S=""
    return 1
  fi
  for _ in $(seq 1 1200); do
    if curl -sf -o /dev/null --max-time 10 -H 'Accept: application/sparql-results+json' \
      --data-urlencode 'query=SELECT * WHERE { ?s ?p ?o } LIMIT 1' "${URL[$e]}"; then
      t1=$(date +%s.%N)
      RESTART_S=$(awk "BEGIN {printf \"%.3f\", $t1 - $t0}")
      return 0
    fi
    sleep 0.1
  done
  RESTART_S=""
  return 1
}
fail_json() { echo "{\"ok\": false, \"error\": \"$1\"}"; }

STORE=$WORK/scratch
for e in $all; do
  v=${VERDICT[${NAME[$e]}]:-}
  if [ -n "$v" ] && [ -z "${IGNORE_ANSWERS:-}" ]; then
    log "$e: not run, it ${v#*:} of the read set"
    python3 "$LW" merge --into "$RES/longwrite.json" --engine "$e" --answers "${v%%:*}" --answers-reason "${v#*:}"
    continue
  fi
  ENGINES=$e
  log "$e: copying its store"
  copy_stores
  start "$e"
  fields=()
  [ -n "${UPDFIELD[$e]:-}" ] && fields=(--field "${UPDFIELD[$e]}")
  comp=()
  [ "$e" = sparkles ] && comp=(--compaction-url "localhost:${PORT[$e]}/\$/stats/bench")
  out=$RES/run-$e.json
  rm -f "$out"
  log "$e: $SECONDS_RUN s of ${READERS:-16} readers and ${WRITERS:-1} writers"
  setsid timeout -k 30 $((SECONDS_RUN + 900)) "${GEN[@]}" python3 "$LW" run --query-url "${URL[$e]}" \
    --update-url "${UPDURL[$e]}" "${fields[@]}" --queries queries --names "${NAMES[*]}" --seconds "$SECONDS_RUN" \
    --readers "${READERS:-16}" --writers "${WRITERS:-1}" --batch "${BATCH:-100}" --live-max "${LIVE_MAX:-200000}" \
    --write-rate "${WRITE_RATE:-0}" --baseline "${BASELINE:-60}" --seed "${SEED:-20261018}" --sample "${SAMPLE:-5}" --port "${PORT[$e]}" \
    --store "$(store_of "$e")" "${comp[@]}" --read-timeout "$MAX_TIME" --out "$out" > "logs/longwrite-$e.log" 2>&1 &
  GENPID=$!
  wait "$GENPID" || log "$e: the load generator failed, see logs/longwrite-$e.log"
  GENPID=""
  cat "logs/longwrite-$e.log"
  c1=$(check "$e" "$out")
  log "$e: state check $c1"
  # a crash: the recovery of the engine's log
  pid=$(pid_on "${PORT[$e]}")
  [ -n "$pid" ] && { kill -KILL "$pid" 2> /dev/null || true; }
  stop "$e"
  if restart "$e"; then
    c2=$(check "$e" "$out")
    s2=$RESTART_S
  else
    c2=$(fail_json "the server did not start after SIGKILL")
    s2=""
  fi
  log "$e: after SIGKILL, ready in ${s2:-?} s, state check $c2"
  stop "$e"
  if restart "$e"; then
    c3=$(check "$e" "$out")
  else
    c3=$(fail_json "the server did not start after a normal stop")
  fi
  log "$e: after a restart, ready in ${RESTART_S:-?} s, state check $c3"
  stop "$e"
  python3 "$LW" merge --into "$RES/longwrite.json" --engine "$e" --run "$out" --check "$c1" --check-crash "$c2" \
    --check-reopen "$c3" --restart-s "${RESTART_S:-}" --crash-restart-s "$s2"
  # the servers' logs outlive the store copy
  while IFS= read -r -d '' f; do cp "$f" "logs/$e-$(basename "$f")"; done < <(find "$STORE" -maxdepth 3 -name '*.log' -print0)
  [ -n "${KEEP_SCRATCH:-}" ] || rm -rf "$STORE"
done
ENGINES=$all
python3 "$ROOT/scripts/bench-record.py" finish "$RES/manifest.json"
python3 "$LW" summary "$RES/longwrite.json" > "$RES/summary.md"
python3 "$ROOT/scripts/bench-record.py" build --suite longwrite --scale "$SCALE" --results "$RES" \
  --manifest "$RES/manifest.json" --out "$RES/record.json" --queries --timer longrun.py
log "record: $WORK/$RES/record.json, table: $WORK/$RES/summary.md"
