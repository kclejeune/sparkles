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
#    of derived triples of each. Sparkles is the only engine here with a reasoner that runs
#    on this data in one command. Jena's rule reasoners are not run by this script.
# 2. Queries. Sparkles dumps the data with the inferred triples of PROFILE (default owl-rl),
#    and every engine loads that same N-Triples file, so the comparison is over equal data.
#    Answers are fingerprinted and compared before any timing, as in scripts/bench.sh, and
#    the majority of the engines is the reference.
#
# Results go to WORKDIR/results (manifest.json, record.json, summary.md, hyperfine's JSON).
#
# Env: PROFILE (owl-rl or rdfs), WARMUP (default 2), RUNS (default 10), COLD_RUNS (default 0),
# SKIP_LOAD=1, ANSWERS_ONLY=1, QUERIES="q1 q9 …", ENGINES="sparkles jena qlever fluree oxigraph"
# (default; ENGINES="" only generates and materializes the data), PORT_BASE (default 48300),
# SPARKLES, SERVER_MEM_MAX, CLIENT_CPU, JAVA, FLUREE and FLUREE_VERSION as in scripts/bench.sh.
set -euo pipefail

UNIV=${1:-1}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${2:-$ROOT/target/bench-lubm}
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
ENGINES=${ENGINES-sparkles jena qlever fluree oxigraph}
PORT_BASE=${PORT_BASE:-48300}
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
  "$SPARKLES" convert --recursive --syntax rdfxml owl gen/univ-bench.owl -o base.nt.tmp.nt 2> logs/convert.log
  mv base.nt.tmp.nt base.nt
  echo "$GENPARAMS" > gen/params
  rm -rf owl gen/out* "gen/out\\log.txt"
fi
log "generated data and ontology: $(wc -l < base.nt) triples"

# --------------------------------------------------------------------------- inference
# infer_profile <profile>: materialize a copy of the loaded data; <profile>.nt gets the data
# with its inferred triples, and results/inference.json the cost
infer_profile() {
  local p=$1 gtime out derived t0 t1
  gtime=$(gnu_time)
  [ -d base.db ] || "$SPARKLES" load --loc base.db base.nt > logs/load-base.log 2>&1
  rm -rf "infer-$p.db"
  cp -a --reflink=auto base.db "infer-$p.db"
  t0=$(date +%s.%N)
  "$gtime" -v -o "logs/infer-$p.time" "$SPARKLES" infer --loc "infer-$p.db" --profile "$p" > "logs/infer-$p.log" 2>&1 ||
    die "infer --profile $p failed, see logs/infer-$p.log"
  t1=$(date +%s.%N)
  derived=$(sed -n 's/^\([0-9]*\) inferred triples.*/\1/p' "logs/infer-$p.log" | tail -1)
  out="$p.nt"
  "$SPARKLES" dump --loc "infer-$p.db" --merge --format nt --out "$out.tmp.nt" > /dev/null 2>&1
  mv "$out.tmp.nt" "$out"
  python3 -I - "$RES_DIR/inference.json" "$p" "$(awk "BEGIN {print $t1 - $t0}")" "logs/infer-$p.time" "${derived:-0}" "$(wc -l < base.nt)" "$(wc -l < "$out")" << 'EOF'
import json, os, re, sys
f, p, secs, timef, derived, base, total = sys.argv[1:]
rss = int(re.search(r"Maximum resident set size \(kbytes\): (\d+)", open(timef).read()).group(1))
d = json.load(open(f)) if os.path.exists(f) else {}
d[p] = {"seconds": float(secs), "max_rss_kib": rss, "derived_triples": int(derived),
        "base_triples": int(base), "total_triples": int(total)}
d["notes"] = ["Sparkles' reasoner (sparkles infer); Jena's rule reasoners were not run",
              "QLever, Oxigraph and Fluree have no reasoner for this data and answer over the materialized file"]
json.dump(d, open(f, "w"), indent=1)
print(f"{p}: {float(secs):.1f} s, peak RSS {rss >> 10} MiB, {derived} derived triples")
EOF
  rm -rf "infer-$p.db"
}
RES_DIR=$WORK/results
for p in rdfs owl-rl; do
  [ -f "$p.nt" ] && [ -f "$RES_DIR/inference.json" ] && grep -q "\"$p\"" "$RES_DIR/inference.json" && continue
  log "materializing with the $p profile"
  infer_profile "$p"
done

# --------------------------------------------------------------------------- the suite
DATAFILE=$PROFILE.nt
COUNT_ONLY=""
DATA_SPECS=("lubm-$UNIV=$WORK/$DATAFILE|none stated (UBA 1.7)|LUBM($UNIV) $GENPARAMS, materialized with $PROFILE")
LICENSE_NOTES=("UBA 1.7 and univ-bench.owl: no license stated, downloaded at run time and not committed")
suite_run
