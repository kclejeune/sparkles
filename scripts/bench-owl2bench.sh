#!/usr/bin/env bash
# OWL2Bench: materialization of the OWL 2 RL university data with Sparkles (and, on request,
# Jena's rule reasoners), then the OWL2Bench SPARQL queries on Sparkles, Apache Jena
# (TDB2 + Fuseki), QLever, Fluree and Oxigraph over the same materialized data, via hyperfine.
#
#   scripts/bench-owl2bench.sh [UNIVERSITIES] [WORKDIR]
#
# UNIVERSITIES is the OWL2Bench scale. The paper publishes 1, 2, 5, 10, 20, 50, 100 and 200,
# and 1 university gives about 50,000 axioms, 200 about 14 million. The generator is the
# OWL2Bench jar of 2020 that made the paper's datasets, at the commit pinned in
# scripts/bench-owl2bench/pins.tsv (Apache-2.0), run with the OWL 2 RL TBox and seed 1, the
# seed of the paper's datasets. It needs Java 8, which comes from nixpkgs (JAVA8 overrides).
# The pins file says why the later jar is not used. The same scale and seed give the same
# file. The jar, the files it reads and the 22 queries (a Zenodo record under CC-BY-4.0) are
# downloaded at run time, checked against the pins and never committed.
#
# The generated RDF/XML holds the TBox and the ABox. `sparkles convert` turns it into
# N-Triples once, and the blank nodes of the TBox's restrictions are relabeled in order of
# appearance, so the file depends only on the scale and the seed.
#
# Two measurements, as in docs/specs/X06 section 3.4.
# 1. Inference. `sparkles infer` materializes the data with the owl-rl profile (and the
#    others of INFER_PROFILES) on its own copy; WORKDIR/results/inference.json gets the time,
#    the peak RSS, the reasoner's peak heap estimate and the derived triples.
#    JENA_REASONERS="owl-mini" (or rdfs, owl-micro, owl) times Jena's rule reasoners on the
#    same file (scripts/bench-suite-lib.sh, jena_reason).
# 2. Queries. Every engine loads the owl-rl materialization, and the queries whose OWL2Bench
#    profile list contains RL run on all of them (18 of the 22; WORKDIR/queries/profiles.tsv
#    lists which). Answers are fingerprinted and compared before timing, and the majority of
#    the engines is the reference. The EL, QL and DL queries are left out because an RL
#    materialization does not define their answers.
#
# Results go to WORKDIR/results (manifest.json, record.json, summary.md, inference.json).
#
# Env: INFER_PROFILES (default owl-rl), INFER_THREADS, DERIVED_DIGEST, JENA_REASONERS,
# JENA_REASON_TIMEOUT, JENA_HEAP as in scripts/bench-lubm.sh; WARMUP (default 2), RUNS
# (default 10), COLD_RUNS (default 0), SKIP_LOAD=1, ANSWERS_ONLY=1, QUERIES="q2 q12 …",
# ENGINES (default the five; "" only generates and materializes), PORT_BASE (default 48300),
# SPARKLES, SERVER_MEM_MAX, CLIENT_CPU, FLUREE and FLUREE_VERSION as in scripts/bench.sh, and
# JAVA8 (a Java 8 binary for the generator) and GEN_HEAP (its heap, default 12G).
set -euo pipefail

UNIV=${1:-1}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${2:-$ROOT/target/bench-owl2bench}
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
ENGINES=${ENGINES-sparkles jena qlever fluree oxigraph}
PORT_BASE=${PORT_BASE:-48300}
MAX_TIME=${MAX_TIME:-600}
export MAX_TIME
[[ $UNIV =~ ^[1-9][0-9]*$ ]] || {
  echo "UNIVERSITIES must be a positive integer" >&2
  exit 1
}
SUITE=owl2bench-rl
SCALE=$UNIV
SEED=1

mkdir -p "$WORK/results" "$WORK/queries" "$WORK/logs" "$WORK/gen"
WORK=$(cd "$WORK" && pwd)
cd "$WORK"
# shellcheck source=scripts/bench-lib.sh
source "$ROOT/scripts/bench-lib.sh"
# shellcheck source=scripts/bench-suite-lib.sh
source "$ROOT/scripts/bench-suite-lib.sh"
PINS=$ROOT/scripts/bench-owl2bench/pins.tsv

# ---------------------------------------------------------------------------- generator
GENFILES=(OWL2Bench.jar RandomNames.xlsx UNIV-BENCH-OWL2RL.owl UNIV-BENCH-OWL2EL.owl UNIV-BENCH-OWL2QL.owl UNIV-BENCH-OWL2DL.owl)
for f in "${GENFILES[@]}" LICENSE queries.txt; do
  fetch_pinned "$PINS" "$f" "gen/$f"
done
rm -f queries/*.rq
read -r -a NAMES <<< "$(python3 -I "$ROOT/scripts/bench-owl2bench/queries.py" gen/queries.txt queries RL)"

if [ -z "${JAVA8:-}" ]; then
  # not nixbin: the java on PATH is usually newer, and the jar's dependency injection
  # fails on Java 9 and later
  mkdir -p "$WORK/gcroots"
  nix build --inputs-from "$ROOT" nixpkgs#jdk8_headless --out-link "$WORK/gcroots/jdk8" > /dev/null
  JAVA8=$WORK/gcroots/jdk8/bin/java
fi
GENPARAMS="universities=$UNIV profile=RL seed=$SEED"
if [ -f gen/params ] && [ "$(cat gen/params)" != "$GENPARAMS" ]; then
  die "$WORK holds data generated with $(cat gen/params), not $GENPARAMS; use another workdir"
fi
if [ ! -f base.nt ]; then
  log "generating OWL2Bench ($GENPARAMS)"
  # The jar reads the TBoxes and the name list from its working directory and writes
  # OWL2RL-<n>.owl there.
  rm -rf gen/run
  mkdir -p gen/run
  for f in "${GENFILES[@]}"; do ln -s "$WORK/gen/$f" "gen/run/$f"; done
  (cd gen/run && "$JAVA8" -Xmx"${GEN_HEAP:-12G}" -jar OWL2Bench.jar "$UNIV" RL "$SEED" > ../../logs/generate.log 2>&1) ||
    die "the OWL2Bench generator failed, see logs/generate.log"
  [ -s "gen/run/OWL2RL-$UNIV.owl" ] || die "the generator wrote no gen/run/OWL2RL-$UNIV.owl, see logs/generate.log"
  # the jar of 2020 writes its data in the TBox's namespace; a jar that does not would
  # leave the reasoner nothing to do, so the run stops
  grep -q 'kracr.iiitd.edu.in' "gen/run/OWL2RL-$UNIV.owl" && die "the generated ABox is not in the TBox's namespace"
  "$SPARKLES" convert --syntax rdfxml "gen/run/OWL2RL-$UNIV.owl" -o base.raw.nt 2> logs/convert.log
  # The converter picks random blank node labels; relabel them by order of appearance.
  awk '{
    s = $0; r = ""
    while (match(s, /_:[A-Za-z0-9]+/)) {
      l = substr(s, RSTART, RLENGTH)
      if (!(l in m)) m[l] = "_:b" ++n
      r = r substr(s, 1, RSTART - 1) m[l]
      s = substr(s, RSTART + RLENGTH)
    }
    print r s }' base.raw.nt > base.nt.tmp
  mv base.nt.tmp base.nt
  rm -f base.raw.nt
  rm -rf gen/run
  echo "$GENPARAMS" > gen/params
fi
log "generated TBox and ABox: $(wc -l < base.nt) triples"

# --------------------------------------------------------------------------- inference
RES_DIR=$WORK/results
INFER_NOTES="Sparkles' reasoner (sparkles infer); Jena's rule reasoners run only when JENA_REASONERS names them
QLever and Oxigraph have no reasoner. Fluree documents query-time reasoning but it is not enabled here. All three answer over the materialized file
The queries are the OWL2Bench queries whose profile list contains RL; queries/profiles.tsv lists them"
for p in ${INFER_PROFILES:-owl-rl}; do
  [ -f "$p.nt" ] && inferred "$p" && continue
  log "materializing with the $p profile"
  suite_infer "$p"
done
for r in ${JENA_REASONERS:-}; do
  inferred "jena-$r" && continue
  case $r in rdfs) jp=rdfs ;; *) jp=owl-rl ;; esac
  jena_reason "$jp" "$r"
done
[ -z "${ENGINES// /}" ] || [ -f owl-rl.nt ] || die "owl-rl.nt is missing: INFER_PROFILES must include owl-rl"

# --------------------------------------------------------------------------- the suite
DATAFILE=owl-rl.nt
COUNT_ONLY=""
DATA_SPECS=("owl2bench-$UNIV=$WORK/$DATAFILE|Apache-2.0 (OWL2Bench)|OWL2Bench $GENPARAMS, materialized with owl-rl"
  "owl2bench-queries=$WORK/gen/queries.txt|CC-BY-4.0|OWL2Bench SPARQL queries, Zenodo 10.5281/zenodo.3838735")
LICENSE_NOTES=("OWL2Bench generator and TBoxes: Apache-2.0; queries: CC-BY-4.0; both downloaded at run time and not committed")
suite_run
