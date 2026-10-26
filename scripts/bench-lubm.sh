#!/usr/bin/env bash
# LUBM: materialization with Sparkles, then the 14 LUBM queries on Sparkles, Apache Jena
# (TDB2 + Fuseki), QLever, Fluree and Oxigraph over the same materialized data, via hyperfine.
#
#   scripts/bench-lubm.sh [UNIVERSITIES] [WORKDIR]
#
# UNIVERSITIES is the LUBM scale: 1 (the smoke default, about 100k triples), 100 (about 13.4M)
# or 1000 (about 134M). The generator is the Univ-Bench Artificial Data Generator 1.7 with seed
# 0 and starting index 0, run on the JDK on PATH or from nixpkgs (JAVA overrides). It and the
# univ-bench ontology carry no license text, so they are downloaded at run time, checked
# against scripts/bench-lubm/pins.tsv and never committed. The 14 queries are in
# scripts/bench-lubm/queries.txt.
#
# Two measurements, as in docs/specs/X06 section 3.3.
# 1. Inference. The generated data and the ontology load into a Sparkles database, and
#    `sparkles infer` materializes it with the rdfs profile and with the owl-rl profile, each
#    on its own copy. WORKDIR/results/inference.json gets the time, the peak RSS and the number
#    of derived triples of each, with the reasoner's own peak heap estimate.
# 2. Queries. Sparkles dumps the data with the inferred triples of PROFILE (default owl-rl),
#    and every engine loads that same N-Triples file, so the comparison is over equal data.
#    Answers are fingerprinted and compared before any timing, as in scripts/bench.sh, and
#    the majority of the engines is the reference.
#
# Results go to WORKDIR/results (manifest.json, record.json, summary.md, hyperfine's JSON).
#
# INFER_PROFILES (default "rdfs owl-rl") picks the materializations, INFER_THREADS sets their
# threads and DERIVED_DIGEST=0 skips the digest of the derived triples (a sort of the whole
# closure). JENA_REASONERS (for example "rdfs owl-mini") also times Jena's rule reasoners on
# the same data (scripts/bench-suite-lib.sh, jena_reason), within JENA_REASON_TIMEOUT and
# JENA_HEAP. ENGINES="" with INFER_PROFILES=rdfs is F16's acceptance run A1.
#
# Env: PROFILE (owl-rl or rdfs), WARMUP (default 2), RUNS (default 10), COLD_RUNS (default 0),
# SKIP_LOAD=1, ANSWERS_ONLY=1, QUERIES="q1 q9 …", ENGINES="sparkles jena qlever fluree oxigraph"
# (default; ENGINES="" only generates and materializes the data), PORT_BASE (default 28300),
# SPARKLES, SERVER_MEM_MAX, CLIENT_CPU, JAVA, FLUREE and FLUREE_VERSION as in scripts/bench.sh.
set -euo pipefail

UNIV=${1:-1}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${2:-$ROOT/.artifacts/bench-lubm}
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
ENGINES=${ENGINES-sparkles jena qlever fluree oxigraph}
PORT_BASE=${PORT_BASE:-28300}
PROFILE=${PROFILE:-owl-rl}
MAX_TIME=${MAX_TIME:-600}
export MAX_TIME
case $PROFILE in rdfs | owl-rl) ;; *)
  echo "PROFILE must be rdfs or owl-rl" >&2
  exit 1
  ;;
esac
SUITE=lubm-$PROFILE
SCALE=$UNIV
NAMES=(q1 q2 q3 q4 q5 q6 q7 q8 q9 q10 q11 q12 q13 q14)

mkdir -p "$WORK/results" "$WORK/queries" "$WORK/logs" "$WORK/gen"
WORK=$(cd "$WORK" && pwd)
cd "$WORK"
# shellcheck source=scripts/bench-lib.sh
source "$ROOT/scripts/bench-lib.sh"
# shellcheck source=scripts/bench-suite-lib.sh
source "$ROOT/scripts/bench-suite-lib.sh"
PINS=$ROOT/scripts/bench-lubm/pins.tsv

# --------------------------------------------------------------------------- queries
awk -v dir=queries '
  /^## / { if (out) close(out); out = dir "/" $2 ".rq"; next }
  /^#/ { next }
  out { print > out }' "$ROOT/scripts/bench-lubm/queries.txt"

# ---------------------------------------------------------------------------- generator
fetch_pinned "$PINS" uba1.7.zip gen/uba1.7.zip
fetch_pinned "$PINS" univ-bench.owl gen/univ-bench.owl
if [ -z "${JAVA:-}" ]; then JAVA=$(nixbin jdk17_headless java); fi
GENPARAMS="universities=$UNIV seed=0 index=0"
if [ -f gen/params ] && [ "$(cat gen/params)" != "$GENPARAMS" ]; then
  die "$WORK holds data generated with $(cat gen/params), not $GENPARAMS; use another workdir"
fi
if [ ! -f base.nt ]; then
  log "generating LUBM($UNIV) ($GENPARAMS)"
  rm -rf gen/uba gen/out* owl
  mkdir -p gen/uba gen/out owl
  python3 -I -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' gen/uba1.7.zip gen/uba
  # The generator writes each file as <user.dir>\<name>, with a Windows separator, so on Linux
  # the files land next to the directory it runs in, named "out\University0_0.owl".
  (cd gen/out && "$JAVA" -cp ../uba/classes edu.lehigh.swat.bench.uba.Generator \
    -univ "$UNIV" -index 0 -seed 0 -onto http://swat.cse.lehigh.edu/onto/univ-bench.owl > ../../logs/generate.log)
  for f in gen/out\\*.owl; do mv "$f" "owl/${f#*\\}"; done
  "$SPARKLES" convert --recursive --syntax rdfxml owl gen/univ-bench.owl -o base.raw.nt 2> logs/convert.log
  # Each generated file has an ontology header with the empty IRI as its subject, which names the file
  # itself by a local path (file:///…/University0_0.owl) after conversion. Those two triples per file are
  # dropped, so the data does not depend on where it was generated. The ontology's blank nodes
  # get labels by order of appearance, because the converter picks random ones.
  grep -v '^<file://' base.raw.nt | awk '{
    s = $0; r = ""
    while (match(s, /_:[A-Za-z0-9]+/)) {
      l = substr(s, RSTART, RLENGTH)
      if (!(l in m)) m[l] = "_:b" ++n
      r = r substr(s, 1, RSTART - 1) m[l]
      s = substr(s, RSTART + RLENGTH)
    }
    print r s }' > base.nt.tmp
  mv base.nt.tmp base.nt
  rm -f base.raw.nt
  echo "$GENPARAMS" > gen/params
  rm -rf owl gen/out* "gen/out\\log.txt"
fi
log "generated data and ontology: $(wc -l < base.nt) triples"

# --------------------------------------------------------------------------- inference
RES_DIR=$WORK/results
INFER_NOTES="Sparkles' reasoner (sparkles infer); Jena's rule reasoners run only when JENA_REASONERS names them
QLever and Oxigraph have no reasoner. Fluree documents query-time reasoning but it is not enabled here. All three answer over the materialized file"
for p in ${INFER_PROFILES:-rdfs owl-rl}; do
  [ -f "$p.nt" ] && inferred "$p" && continue
  log "materializing with the $p profile"
  suite_infer "$p"
done
for r in ${JENA_REASONERS:-}; do
  inferred "jena-$r" && continue
  case $r in rdfs) jp=rdfs ;; *) jp=owl-rl ;; esac
  jena_reason "$jp" "$r"
done
[ -z "${ENGINES// /}" ] || [ -f "$PROFILE.nt" ] || die "$PROFILE.nt is missing: INFER_PROFILES must include $PROFILE"

# --------------------------------------------------------------------------- the suite
DATAFILE=$PROFILE.nt
COUNT_ONLY=""
DATA_SPECS=("lubm-$UNIV=$WORK/$DATAFILE|none stated (UBA 1.7)|LUBM($UNIV) $GENPARAMS, materialized with $PROFILE")
LICENSE_NOTES=("UBA 1.7 and univ-bench.owl: no license stated, downloaded at run time and not committed")
suite_run
