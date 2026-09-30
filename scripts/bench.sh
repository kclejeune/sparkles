#!/usr/bin/env bash
# Load + query benchmark: Sparkles vs Apache Jena TDB2 vs QLever on the synthetic dataset.
#
#   scripts/bench.sh [N_PEOPLE] [WORKDIR]
#
# Jena and QLever are taken from nixpkgs (`nix shell nixpkgs#apache-jena nixpkgs#qlever`)
# when not on PATH. Each query is run 1 + RUNS times; the first run is discarded (warm-up)
# and the median of the remaining runs is reported. QLever's result cache is cleared before
# every run so repeated runs measure execution, not cache hits.
set -euo pipefail

N=${1:-100000}
WORK=${2:-/tmp/sparkles-bench}
RUNS=${RUNS:-3}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
mkdir -p "$WORK"
cd "$WORK"

nixbin() { # nixbin <pkg> <bin>
  if command -v "$2" >/dev/null; then command -v "$2"; else echo "$(nix build "nixpkgs#$1" --no-link --print-out-paths 2>/dev/null | tail -1)/bin/$2"; fi
}
TDBLOADER=$(nixbin apache-jena tdb2.tdbloader)
TDBQUERY=$(nixbin apache-jena tdb2.tdbquery)
QINDEX=$(nixbin qlever qlever-index)
QSERVER=$(nixbin qlever qlever-server)

if [ ! -f data.nt ]; then
  echo "generating dataset ($N people)…"
  python3 "$ROOT/scripts/gen-data.py" "$N" > data.nt
fi
TRIPLES=$(wc -l < data.nt)
echo "dataset: $TRIPLES triples"

now() { date +%s.%N; }
elapsed() { echo "$(echo "$(now) - $1" | bc)"; }

# ----------------------------------------------------------------------------- load
declare -A LOAD
rm -rf sparkles.db jena.db qlever-index && mkdir -p qlever-index
t=$(now); "$SPARKLES" load --loc sparkles.db data.nt 2>/dev/null; LOAD[sparkles]=$(elapsed "$t")
t=$(now); "$TDBLOADER" --loc jena.db data.nt >/dev/null 2>&1; LOAD[jena]=$(elapsed "$t")
t=$(now)
(cd qlever-index && cat ../data.nt | "$QINDEX" -i bench -F nt -f - -s '{"num-triples-per-batch": 1000000}' >/dev/null 2>&1)
LOAD[qlever]=$(elapsed "$t")

# ---------------------------------------------------------------------------- queries
P='PREFIX ex: <http://example.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> '
declare -a NAMES QUERIES
add() { NAMES+=("$1"); QUERIES+=("$P$2"); }
add "count-all"      'SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }'
add "types-grouped"  'SELECT ?t (COUNT(?s) AS ?c) WHERE { ?s a ?t } GROUP BY ?t ORDER BY DESC(?c)'
add "star-join"      'SELECT ?p ?n ?a ?o WHERE { ?p a ex:Researcher ; foaf:name ?n ; foaf:age ?a ; ex:worksFor ?o . ?o ex:city "Kyoto" }'
add "two-hop-count"  'SELECT (COUNT(*) AS ?c) WHERE { ?a foaf:knows ?b . ?b foaf:knows ?c }'
add "range-topk"     'SELECT ?p ?s WHERE { ?p ex:salary ?s FILTER(?s > 150000) } ORDER BY DESC(?s) LIMIT 10'
add "optional-count" 'SELECT (COUNT(*) AS ?c) WHERE { ?p a ex:Student OPTIONAL { ?p ex:worksFor ?o } }'
add "contains"       'SELECT (COUNT(*) AS ?c) WHERE { ?p foaf:name ?n FILTER(CONTAINS(?n, "Ada")) }'
add "group-avg"      'SELECT ?o (AVG(?a) AS ?avg) (COUNT(?p) AS ?n) WHERE { ?p ex:worksFor ?o ; foaf:age ?a } GROUP BY ?o ORDER BY DESC(?n) LIMIT 10'
add "path-plus"      'SELECT (COUNT(*) AS ?c) WHERE { <http://example.org/doc/1> ex:cites+ ?d }'
add "distinct-obj"   'SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s foaf:knows ?o }'

median() { sort -n | awk '{a[NR]=$1} END {print (NR%2 ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2)}'; }

SPORT=3931; QPORT=3932
"$SPARKLES" serve --data sparkles-server --loc bench="$WORK/sparkles.db" --port $SPORT --timeout 600 >/dev/null 2>&1 &
SPID=$!
(cd qlever-index && "$QSERVER" -i bench -p $QPORT -m 8G -c 2G -s 600s >/dev/null 2>&1) &
QPID=$!
trap 'kill $SPID $QPID 2>/dev/null || true' EXIT
until curl -sf localhost:$SPORT/\$/ping >/dev/null; do sleep 0.2; done
until curl -sf "localhost:$QPORT/?cmd=stats" >/dev/null; do sleep 0.5; done

http_time() { # url query -> seconds, rows
  curl -s -o /dev/null -w '%{time_total}' "$1" -H 'Accept: text/tab-separated-values' --data-urlencode "query=$2"
}

printf '\n%-16s %12s %12s %12s\n' "load (s)" "${LOAD[sparkles]:0:8}" "${LOAD[jena]:0:8}" "${LOAD[qlever]:0:8}"
printf '%-16s %12s %12s %12s\n' "query (ms)" sparkles jena-tdb2 qlever
for i in "${!NAMES[@]}"; do
  q=${QUERIES[$i]}
  s=(); j=(); k=()
  for r in $(seq 0 "$RUNS"); do
    ts=$(http_time "localhost:$SPORT/bench/sparql" "$q")
    curl -s "localhost:$QPORT/?cmd=clear-cache" >/dev/null
    tq=$(http_time "localhost:$QPORT/" "$q")
    # Jena: in-process time reported by tdbquery --time (excludes JVM start)
    tj=$("$TDBQUERY" --loc jena.db --time --results=tsv "$q" 2>&1 >/dev/null | sed -n 's/^Time: \([0-9.]*\) sec/\1/p')
    [ "$r" -gt 0 ] && { s+=("$ts"); k+=("$tq"); j+=("${tj:-nan}"); }
  done
  ms() { printf '%s\n' "$@" | median | awk '{printf "%.1f", $1*1000}'; }
  printf '%-16s %12s %12s %12s\n' "${NAMES[$i]}" "$(ms "${s[@]}")" "$(ms "${j[@]}")" "$(ms "${k[@]}")"
done
