#!/usr/bin/env bash
# SP2Bench: Sparkles vs Apache Jena (TDB2 + Fuseki) vs QLever vs Fluree vs Oxigraph on the 17
# queries of the SP2Bench SPARQL performance benchmark over DBLP-like data, via hyperfine.
#
#   scripts/bench-sp2b.sh [SCALE] [WORKDIR]
#
# SCALE is the number of triples the generator writes, with a k or m suffix: 50k (the smoke
# default), 1m, 10m or 100m. The generator is SP2Bench v1.01 (BSD license), built from its
# checksum-pinned release with scripts/bench-sp2b/sp2b.nix against the nixpkgs revision in
# flake.lock and cached in WORKDIR/sp2b-gen. It seeds its random numbers with a fixed value,
# so a size always gives the same document. The Turtle it writes is converted to N-Triples
# once (sparkles convert), and every engine loads that file. The queries come from the same
# release (the nix output holds them), so none of the benchmark is committed here.
#
# The rest is the method of scripts/bench.sh: every engine is served over HTTP with TSV
# results (JSON for the ASK queries), no result cache, each answer fingerprinted and compared
# before any timing (an engine that differs from the majority is marked and not ranked),
# a request timeout of MAX_TIME seconds (600, as the spec asks), and timed samples that fail
# reported as errors or timeouts. WORKDIR/results holds hyperfine's JSON per query, the
# answer fingerprints, manifest.json, record.json (the result record, docs/specs/X06 section
# 7) and summary.md.
#
# Env: WARMUP (default 2), RUNS (default 10), COLD_RUNS (restarts per query for the cold
# measurement, default 0), SKIP_LOAD=1, ANSWERS_ONLY=1, QUERIES="q1 q5a …",
# ENGINES="sparkles jena qlever fluree oxigraph" (default; ENGINES="" only generates the
# data and the queries), PORT_BASE (default 28300), SPARKLES (default target/release/sparkles),
# SP2B (a prefix holding bin/sp2b_gen and share/sp2b, instead of the nix build), SERVER_MEM_MAX,
# CLIENT_CPU, FLUREE and FLUREE_VERSION as in scripts/bench.sh.
set -euo pipefail

SCALE=${1:-50k}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${2:-$ROOT/target/bench-sp2b}
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
ENGINES=${ENGINES-sparkles jena qlever fluree oxigraph}
PORT_BASE=${PORT_BASE:-28300}
MAX_TIME=${MAX_TIME:-600}
export MAX_TIME
SUITE=sp2bench
case $SCALE in
  *k) TRIPLES=$((${SCALE%k} * 1000)) ;;
  *m) TRIPLES=$((${SCALE%m} * 1000000)) ;;
  *[!0-9]*)
    echo "SCALE must be a number of triples with an optional k or m suffix" >&2
    exit 1
    ;;
  *) TRIPLES=$SCALE ;;
esac
NAMES=(q1 q2 q3a q3b q3c q4 q5a q5b q6 q7 q8 q9 q10 q11 q12a q12b q12c)

mkdir -p "$WORK/results" "$WORK/queries" "$WORK/logs"
WORK=$(cd "$WORK" && pwd)
cd "$WORK"
# shellcheck source=scripts/bench-lib.sh
source "$ROOT/scripts/bench-lib.sh"
# shellcheck source=scripts/bench-suite-lib.sh
source "$ROOT/scripts/bench-suite-lib.sh"

# ---------------------------------------------------------------------------- generator
if [ -z "${SP2B:-}" ]; then
  SP2B=$WORK/sp2b-gen
  if [ ! -x "$SP2B/bin/sp2b_gen" ]; then
    log "building the SP2Bench generator (scripts/bench-sp2b/sp2b.nix)"
    pinned_build "$ROOT/scripts/bench-sp2b/sp2b.nix" "$SP2B"
  fi
fi

# ---------------------------------------------------------------------------- dataset
DATAFILE=data.nt
PARAMS="triples=$TRIPLES"
if [ -f gen/params ] && [ "$(cat gen/params)" != "$PARAMS" ]; then
  die "$WORK holds a dataset generated with $(cat gen/params), not $PARAMS; use another workdir"
fi
if [ ! -f "$DATAFILE" ]; then
  log "generating the SP2Bench document ($PARAMS)"
  mkdir -p gen
  cp "$SP2B"/share/sp2b/*.txt gen/
  (cd gen && "$SP2B/bin/sp2b_gen" -t "$TRIPLES" data.ttl > ../logs/generate.log)
  "$SPARKLES" convert gen/data.ttl -o "$DATAFILE.tmp.nt" 2> logs/convert.log
  mv "$DATAFILE.tmp.nt" "$DATAFILE"
  rm -f gen/data.ttl
  echo "$PARAMS" > gen/params
fi
log "dataset: $(wc -l < "$DATAFILE") triples"

# --------------------------------------------------------------------------- queries
for n in "${NAMES[@]}"; do
  src=$SP2B/share/sp2b/queries/$n.sparql
  [ -f "$src" ] || die "the SP2Bench release has no $src"
  cat "$src" > "queries/$n.rq"
done
COUNT_ONLY=""
DATA_SPECS=("sp2bench-$SCALE=$WORK/$DATAFILE|BSD|SP2Bench v1.01 $PARAMS")
LICENSE_NOTES=("SP2Bench v1.01 generator and queries: BSD license, fetched at build time")

suite_run
