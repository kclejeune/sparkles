#!/usr/bin/env bash
# BSBM (Berlin SPARQL Benchmark): Sparkles vs Apache Jena (TDB2 + Fuseki) vs QLever vs Fluree
# vs Oxigraph on the Explore or the Business Intelligence use case.
#
#   SPARKLES_ALLOW_GPL_BENCHMARK=1 scripts/bench-bsbm.sh [SCALE] [WORKDIR]
#
# SCALE is the number of products (2000, the smoke default, gives about 377k triples) or an
# approximate size in triples with an m suffix: 1m, 10m or 100m (about 190 triples per
# product, so 10m is 53,000 products). USECASE=explore (default) or bi picks the use case.
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
#    results go to results/qmph.json and the record.
#
# Results go to WORKDIR/results (manifest.json, record.json, summary.md, hyperfine's JSON, the
# driver's XML files).
#
# Env: USECASE, INSTANCES, CLIENTS, BSBM_RUNS, BSBM_WARMUP, WARMUP (default 2), RUNS (default
# 10), COLD_RUNS (default 0), SKIP_LOAD=1, ANSWERS_ONLY=1 (checks answers, skips every
# timing), SKIP_QMPH=1, QUERIES="explore-q2 …", ENGINES="sparkles jena qlever fluree
# oxigraph" (default; ENGINES="" only generates the data and the queries), PORT_BASE (default
# 48300; the driver stub uses PORT_BASE + 90), SPARKLES, JAVA, BSBM_GEN_HEAP (default 4g),
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
PORT_BASE=${PORT_BASE:-48300}
USECASE=${USECASE:-explore}
INSTANCES=${INSTANCES:-3}
CLIENTS=${CLIENTS:-1 4 16}
BSBM_RUNS=${BSBM_RUNS:-500}
BSBM_WARMUP=${BSBM_WARMUP:-50}
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
  *m) PRODUCTS=$((${SCALE_ARG%m} * 5300)) ;;
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
log "dataset: $(wc -l < "$DATAFILE") triples"

# --------------------------------------------------------------------------- queries
if [ ! -f "queries/names-$USECASE-$INSTANCES" ]; then
  rm -f queries/"$USECASE"-q*.rq
  python3 -I "$ROOT/scripts/bench-bsbm/instantiate.py" --tools "$WORK/bsbm" --usecase "$USECASE" --out queries \
    --instances "$INSTANCES" --runs $((4 * INSTANCES + 8)) --java "$JAVA" --port $((PORT_BASE + 90)) \
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
bsbm_qmph() {
  local e c rc
  [ -z "${SKIP_QMPH:-}" ] || return 0
  echo
  echo "== query mixes per hour ($BSBM_WARMUP warm-up and $BSBM_RUNS measured mixes)"
  for e in $ENGINES; do
    for c in $CLIENTS; do
      rm -f "results/driver-${NAME[$e]}-$c.xml"
      (cd bsbm && "$JAVA" -Xmx2g -cp ".:lib/*" benchmark.testdriver.TestDriver -runs "$BSBM_RUNS" -w "$BSBM_WARMUP" \
        -mt "$c" -idir td_data -ucf "$UCF" -t "$DRIVER_TIMEOUT_MS" -o "$WORK/results/driver-${NAME[$e]}-$c.xml" \
        "http://${URL[$e]}") > "logs/driver-${NAME[$e]}-$c.log" 2>&1 && rc=0 || rc=$?
      python3 -I "$ROOT/scripts/bench-bsbm/qmph.py" "results/driver-${NAME[$e]}-$c.xml" results/qmph.json \
        "${NAME[$e]}" "$c" "$USECASE-mix" "$rc"
    done
  done
}
SUITE_EXTRA=bsbm_qmph
# the driver does not look at results, so the runs are compared by their average result counts
# DESCRIBE results are left to the engine by the SPARQL specification, so those queries are skipped
bsbm_check() {
  local skip f
  [ -f results/qmph.json ] || return 0
  skip=""
  while read -r f; do
    f=${f##*/query}
    skip="$skip,${f%.txt}"
  done < <(grep -il '^ *describe' bsbm/queries/"$USECASE"/query[0-9]*.txt 2> /dev/null || true)
  python3 -I "$ROOT/scripts/bench-bsbm/qmph.py" --check results/qmph.json "${skip#,}"
}
SUITE_AFTER=bsbm_check

DATA_SPECS=("bsbm-$SCALE=$WORK/$DATAFILE|GPL-3.0 (generator)|BSBM tools 0.2 $GENPARAMS")
LICENSE_NOTES=("BSBM tools 0.2: GPL-3.0, downloaded at run time, run as separate processes and not committed")
suite_run
