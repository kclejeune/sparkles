#!/usr/bin/env bash
# BSBM (Berlin SPARQL Benchmark): Sparkles vs Apache Jena (TDB2 + Fuseki) vs QLever vs Fluree
# vs Oxigraph on the Explore or the Business Intelligence use case.
#
#   SPARKLES_ALLOW_GPL_BENCHMARK=1 scripts/bench-bsbm.sh [SCALE] [WORKDIR]
#
# SCALE is the number of products (2000, the smoke default, gives about 725k triples) or a
# size in millions of triples with an m suffix. The sizes follow the dataset sizes that BSBM
# publishes, 2,785 products for 1m, 28,480 for 10m and 284,826 for 100m, and other sizes get
# 2,848 products per million. The generator makes about 360 triples per product (measured with
# the options below, 2000 products give 725,305 triples). USECASE=explore (default) or bi picks
# the use case.
#
# The BSBM tools (data generator and test driver, version 0.2) are GPL-3.0. The script
# downloads them into WORKDIR only when SPARKLES_ALLOW_GPL_BENCHMARK=1 is set, checks them
# against scripts/bench-bsbm/pins.tsv, and runs them as separate processes. Nothing of them is
# committed. The generator runs with forward chaining on (-fc), a fixed seed and no other
# option, so a scale always gives the same data.
#
# Two measurements.
# 1. Per query template. The test driver is run once against a stub endpoint that records the
#    queries of its own query mixes (scripts/bench-bsbm/instantiate.py), and INSTANCES (default
#    3) distinct instances of each template become query files. Every engine's answer to every
#    instance is fingerprinted and compared before any timing (the DESCRIBE query is not
#    compared, since its result is implementation-defined). Each instance is then timed with
#    hyperfine over HTTP as in scripts/bench.sh, and the record pools the instances of a
#    template into one result with the median and p95 of all their samples.
# 2. Query mixes per hour. For each engine and each client count in CLIENTS (default
#    "1 4 16") the BSBM test driver runs its query mix over HTTP, BSBM_WARMUP warm-up mixes
#    (default 50) followed by BSBM_RUNS measured mixes (default 500), and reports QMpH. The
#    results go to results/qmph.json and the record. When the use case has DESCRIBE or
#    CONSTRUCT queries (Explore's queries 9 and 12), a second mix without them runs on every
#    engine, into results/qmph-select.json, because the driver asks for their results as
#    RDF/XML and QLever does not write RDF/XML. An engine that refuses such a request is
#    recorded with status error and the reason for the standard mix, which is not run on it.
#
# Results go to WORKDIR/results (manifest.json, record.json, summary.md, hyperfine's JSON, the
# driver's XML files).
#
# Env: USECASE, INSTANCES, CLIENTS, BSBM_RUNS, BSBM_WARMUP, BSBM_MAX_TIME (default 1800
# seconds per engine/mix/client driver run), BSBM_KILL_AFTER (default 5 seconds),
# WARMUP (default 2), RUNS (default
# 10), COLD_RUNS (default 0), SKIP_LOAD=1, ANSWERS_ONLY=1 (checks answers, skips every
# timing), SKIP_QMPH=1, QUERIES="explore-q2 …", ENGINES="sparkles jena qlever fluree
# oxigraph" (default; ENGINES="" only generates the data and the queries), PORT_BASE (default
# 28300; the driver stub takes a free port from the kernel), SPARKLES, JAVA, BSBM_GEN_HEAP (default 4g),
# SERVER_MEM_MAX, CLIENT_CPU, FLUREE and FLUREE_VERSION as in scripts/bench.sh.
set -euo pipefail

if [ "${SPARKLES_ALLOW_GPL_BENCHMARK:-}" != 1 ]; then
  cat >&2 << 'EOF'
The BSBM tools (data generator and test driver) are licensed under GPL-3.0. This script
downloads them into its work directory for a local run, runs them as separate programs
and never adds them to the repository.
Set SPARKLES_ALLOW_GPL_BENCHMARK=1 to proceed.
EOF
  exit 2
fi

SCALE_ARG=${1:-2000}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${2:-$ROOT/target/bench-bsbm}
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
ENGINES=${ENGINES-sparkles jena qlever fluree oxigraph}
PORT_BASE=${PORT_BASE:-28300}
USECASE=${USECASE:-explore}
INSTANCES=${INSTANCES:-3}
CLIENTS=${CLIENTS:-1 4 16}
BSBM_RUNS=${BSBM_RUNS:-500}
BSBM_WARMUP=${BSBM_WARMUP:-50}
BSBM_MAX_TIME=${BSBM_MAX_TIME:-1800}
BSBM_KILL_AFTER=${BSBM_KILL_AFTER:-5}
# This wall budget bounds a whole driver cell, independently of each request's MAX_TIME.
# It is a configurable guard, not a sampling interval. A cut-off run has no throughput.
for limit in BSBM_MAX_TIME BSBM_KILL_AFTER; do
  if ! { [[ ${!limit} =~ ^([0-9]+([.][0-9]*)?|[.][0-9]+)$ ]] && awk -v s="${!limit}" 'BEGIN { exit !(s > 0) }'; }; then
    echo "$limit must be a positive number of seconds, not '${!limit}'" >&2
    exit 2
  fi
done
MAX_TIME=${MAX_TIME:-300}
export MAX_TIME
case $USECASE in
  explore) UCF=usecases/explore/sparql.txt ;;
  bi) UCF=usecases/businessIntelligence/sparql.txt ;;
  *)
    echo "USECASE must be explore or bi" >&2
    exit 1
    ;;
esac
case $SCALE_ARG in
  1m) PRODUCTS=2785 ;;
  10m) PRODUCTS=28480 ;;
  100m) PRODUCTS=284826 ;;
  [0-9]*m)
    [[ ${SCALE_ARG%m} =~ ^[0-9]+$ ]] || {
      echo "SCALE must be a product count or a size such as 10m" >&2
      exit 1
    }
    PRODUCTS=$((${SCALE_ARG%m} * 2848))
    ;;
  *[!0-9]*)
    echo "SCALE must be a product count or a size such as 10m" >&2
    exit 1
    ;;
  *) PRODUCTS=$SCALE_ARG ;;
esac
SUITE=bsbm-$USECASE
SCALE=$SCALE_ARG

mkdir -p "$WORK/results" "$WORK/queries" "$WORK/logs"
WORK=$(cd "$WORK" && pwd)
cd "$WORK"
# shellcheck source=scripts/bench-lib.sh
source "$ROOT/scripts/bench-lib.sh"
# shellcheck source=scripts/bench-suite-lib.sh
source "$ROOT/scripts/bench-suite-lib.sh"
PINS=$ROOT/scripts/bench-bsbm/pins.tsv

# ---------------------------------------------------------------------------- generator
fetch_pinned "$PINS" bsbmtools-v0.2.zip bsbmtools-v0.2.zip
if [ -z "${JAVA:-}" ]; then JAVA=$(nixbin jdk17_headless java); fi
GENPARAMS="products=$PRODUCTS forward-chaining seed=default"
if [ -f bsbm/params ] && [ "$(cat bsbm/params)" != "$GENPARAMS" ]; then
  die "$WORK holds data generated with $(cat bsbm/params), not $GENPARAMS; use another workdir"
fi
# The data is generated in bsbm-gen and moved to bsbm when complete, with the params file
# written last, so an interrupted generation is never mistaken for finished data.
if [ ! -f bsbm/params ]; then
  log "generating BSBM data ($GENPARAMS)"
  rm -rf bsbm bsbm-unpack bsbm-gen
  python3 -I -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' bsbmtools-v0.2.zip bsbm-unpack
  mv bsbm-unpack/bsbmtools-0.2 bsbm-gen
  rmdir bsbm-unpack
  (cd bsbm-gen && "$JAVA" -Xmx"${BSBM_GEN_HEAP:-4g}" -cp ".:lib/*" benchmark.generator.Generator \
    -fc -pc "$PRODUCTS" -s nt -fn dataset -dir td_data > ../logs/generate.log 2>&1)
  echo "$GENPARAMS" > bsbm-gen/params
  mv bsbm-gen bsbm
fi
DATAFILE=bsbm/dataset.nt
TRIPLES=$(wc -l < "$DATAFILE")
log "dataset: $TRIPLES triples"

# --------------------------------------------------------------------------- queries
if [ ! -f "queries/names-$USECASE-$INSTANCES" ]; then
  rm -f queries/"$USECASE"-q*.rq
  python3 -I "$ROOT/scripts/bench-bsbm/instantiate.py" --tools "$WORK/bsbm" --usecase "$USECASE" --out queries \
    --instances "$INSTANCES" --runs $((4 * INSTANCES + 8)) --java "$JAVA" \
    > "queries/names-$USECASE-$INSTANCES.tmp"
  mv "queries/names-$USECASE-$INSTANCES.tmp" "queries/names-$USECASE-$INSTANCES"
fi
mapfile -t NAMES < "queries/names-$USECASE-$INSTANCES"
NO_CHECK=""
for n in "${NAMES[@]}"; do
  [ "$(qform "queries/$n.rq")" = describe ] && NO_CHECK="$NO_CHECK $n"
done
COUNT_ONLY=""
POOL=1

# ------------------------------------------------------------------------ query mixes
# the driver's request timeout in whole milliseconds, rounded up from a possibly fractional MAX_TIME
DRIVER_TIMEOUT_MS=$(awk -v m="$MAX_TIME" 'BEGIN { t = m * 1000; s = int(t); if (s < t) s++; print s }')
# The driver asks for DESCRIBE and CONSTRUCT results as RDF/XML (Accept: application/rdf+xml)
# and stops at the first request that fails. QLever 0.5.48 does not write RDF/XML and answers
# 406 Not Acceptable, so it cannot run the Explore mix, whose query 9 is a DESCRIBE and query
# 12 a CONSTRUCT. The driver only counts the bytes of those results, so the standard mix
# runs unchanged for every engine that answers in RDF/XML. A second mix, `select`, leaves
# those queries out through the driver's own ignoreQueries.txt and runs on every engine. The
# driver still draws the parameters of the queries it skips, so the other queries are the
# same instances in both mixes, and this mix is the one that compares all five engines.
MIXDIR=$(sed -n 's/^querymix=//p' "bsbm/$UCF" | head -1)
RDFXML_NRS=$(grep -ilE '^ *(describe|construct)' "bsbm/$MIXDIR"/query[0-9]*.txt 2> /dev/null | sed 's/.*query\([0-9]*\)\.txt$/\1/' | sort -n | tr '\n' ' ' || true)
RDFXML_NRS=${RDFXML_NRS% }
MIXES="std"
if [ -n "$RDFXML_NRS" ]; then
  MIXES="std select"
  rm -rf "bsbm/$MIXDIR-select" "bsbm/usecases/$USECASE-select"
  cp -a "bsbm/$MIXDIR" "bsbm/$MIXDIR-select"
  {
    cat "bsbm/$MIXDIR/ignoreQueries.txt" 2> /dev/null || true
    echo
    echo "$RDFXML_NRS"
  } | tr ' ' '\n' | sed '/^$/d' > "bsbm/$MIXDIR-select/ignoreQueries.txt"
  mkdir -p "bsbm/usecases/$USECASE-select"
  echo "querymix=$MIXDIR-select" > "bsbm/usecases/$USECASE-select/sparql.txt"
fi
# rdfxml_refused <engine>: prints the HTTP status and the query when the engine refuses the
# first DESCRIBE or CONSTRUCT instance in the form the driver sends it (GET, Accept:
# application/rdf+xml), and nothing when it answers them all
rdfxml_refused() {
  local n code
  for n in "${NAMES[@]}"; do
    case "$(qform "queries/$n.rq")" in describe | construct) ;; *) continue ;; esac
    code=$(curl -s -G -o /dev/null -w '%{http_code}' --max-time "$MAX_TIME" -H 'Accept: application/rdf+xml' \
      --data-urlencode "query@queries/$n.rq" "http://${URL[$1]}") || true
    case $code in
      2??) ;;
      *)
        echo "HTTP $code to $n"
        return 0
        ;;
    esac
  done
}
bsbm_qmph() {
  local e c mix ucf sfx out label code
  python3 -I - results/extra.json "$BSBM_MAX_TIME" "$BSBM_KILL_AFTER" "$BSBM_WARMUP" "$BSBM_RUNS" "$CLIENTS" << 'EOF'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
data = json.loads(path.read_text()) if path.exists() else {}
data.setdefault("settings", {}).update({
    "bsbmMaxTimeSeconds": float(sys.argv[2]),
    "bsbmKillAfterSeconds": float(sys.argv[3]),
    "bsbmWarmup": int(sys.argv[4]),
    "bsbmRuns": int(sys.argv[5]),
    "bsbmClients": [int(c) for c in sys.argv[6].split()],
})
path.write_text(json.dumps(data, indent=1))
EOF
  [ -z "${SKIP_QMPH:-}" ] || return 0
  echo
  echo "== query mixes per hour ($BSBM_WARMUP warm-up and $BSBM_RUNS measured mixes)"
  for e in $ENGINES; do
    for mix in $MIXES; do
      if [ "$mix" = std ]; then
        ucf=$UCF sfx="" out=results/qmph.json label=$USECASE-mix
      else
        ucf=usecases/$USECASE-$mix/sparql.txt sfx=-$mix out=results/qmph-$mix.json label=$USECASE-mix-$mix
      fi
      code=""
      if [ "$mix" = std ] && [ -n "$RDFXML_NRS" ]; then code=$(rdfxml_refused "$e"); fi
      for c in $CLIENTS; do
        rm -f "results/driver-${NAME[$e]}-$c$sfx.xml"
        if [[ " $DEAD_ENGINES " == *" $e "* ]]; then
          python3 -I "$ROOT/scripts/bench-bsbm/qmph.py" "results/driver-${NAME[$e]}-$c$sfx.xml" "$out" "${NAME[$e]}" "$c" "$label" 1 \
            "the server did not start again after an earlier failure"
          continue
        fi
        if [ -n "$code" ]; then
          python3 -I "$ROOT/scripts/bench-bsbm/qmph.py" "results/driver-${NAME[$e]}-$c$sfx.xml" "$out" "${NAME[$e]}" "$c" "$label" 1 \
            "the engine answers with $code for Accept: application/rdf+xml, the only format the BSBM driver asks for in DESCRIBE and CONSTRUCT queries ($RDFXML_NRS)"
          continue
        fi
        # a new connection per request, as the harness's curl and oha requests make: on a
        # reused connection QLever's answers wait about 40 ms for a delayed TCP acknowledgement
        echo "$e: $label x$c (whole driver wall budget ${BSBM_MAX_TIME}s)"
        python3 -I "$ROOT/scripts/bench-bsbm/driver.py" --timeout "$BSBM_MAX_TIME" --grace "$BSBM_KILL_AFTER" \
          --cwd "$WORK/bsbm" --xml "$WORK/results/driver-${NAME[$e]}-$c$sfx.xml" --out "$out" \
          --engine "${NAME[$e]}" --clients "$c" --name "$label" \
          -- "$JAVA" -Dhttp.keepAlive=false -Xmx2g -cp ".:lib/*" benchmark.testdriver.TestDriver -runs "$BSBM_RUNS" -w "$BSBM_WARMUP" \
          -mt "$c" -idir td_data -ucf "$ucf" -t "$DRIVER_TIMEOUT_MS" -o "$WORK/results/driver-${NAME[$e]}-$c$sfx.xml" \
          "http://${URL[$e]}" > "logs/driver-${NAME[$e]}-$c$sfx.log" 2>&1
        # The driver can exit 0 after connection errors, and requests that fail fast would
        # inflate the query mixes per hour. A server that no longer answers fails the cell.
        if ! answering "$e"; then
          python3 -I "$ROOT/scripts/bench-bsbm/qmph.py" "results/driver-${NAME[$e]}-$c$sfx.xml" "$out" "${NAME[$e]}" "$c" "$label" 1 \
            "the server stopped answering during the driver run"
        fi
        # a server that crashed or hung during the mixes is restarted before the next run, so
        # one failure does not fail every later client count and mix (results/restarts.json)
        revive "$e" "$label x$c" results
      done
    done
  done
}
SUITE_EXTRA=bsbm_qmph
# The driver does not look at results, so the runs are compared by their average result
# counts. For DESCRIBE and CONSTRUCT it counts the bytes of the RDF/XML, which differ between
# engines that return the same triples, so those queries are left out of the comparison.
bsbm_check() {
  local f
  for f in results/qmph.json results/qmph-*.json; do
    [ -f "$f" ] || continue
    python3 -I "$ROOT/scripts/bench-bsbm/qmph.py" --check "$f" "$(echo "$RDFXML_NRS" | tr ' ' ',')"
  done
}
SUITE_AFTER=bsbm_check

DATA_SPECS=("bsbm-$SCALE=$WORK/$DATAFILE|GPL-3.0 (generator)|BSBM tools 0.2 $GENPARAMS triples=$TRIPLES")
LICENSE_NOTES=("BSBM tools 0.2: GPL-3.0, downloaded at run time, run as separate processes and not committed")
suite_run
