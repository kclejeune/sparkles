#!/usr/bin/env bash
# Load + query benchmark: Sparkles vs Apache Jena (TDB2 + Fuseki) vs QLever, via hyperfine.
#
#   scripts/bench.sh [N_PEOPLE] [WORKDIR]
#
# All three engines are queried over HTTP (SPARQL protocol, TSV results) so JVM start-up
# is not measured. QLever's result cache is cleared in hyperfine's --prepare step (outside
# the timed region) and Sparkles runs with its result cache disabled (--result-cache-mb 0),
# so repeated runs measure execution rather than cache hits; Fuseki has no result cache.
# Results go to WORKDIR/results/*.{md,json} and a combined
# WORKDIR/results/summary.md.
#
# Jena, Fuseki and QLever come from nixpkgs when not on PATH.
# Env: WARMUP (default 2), RUNS (default 10), SKIP_LOAD=1 to reuse existing indexes.
set -euo pipefail

N=${1:-100000}
WORK=${2:-/tmp/sparkles-bench}
WARMUP=${WARMUP:-2}
RUNS=${RUNS:-10}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
mkdir -p "$WORK/results" "$WORK/queries"
cd "$WORK"

nixbin() { # nixbin <pkg> <bin>
  if command -v "$2" >/dev/null; then command -v "$2"; else echo "$(nix build "nixpkgs#$1" --no-link --print-out-paths | tail -1)/bin/$2"; fi
}
TDBLOADER=$(nixbin apache-jena tdb2.tdbloader)
FUSEKI=$(nixbin apache-jena-fuseki fuseki-server)
QINDEX=$(nixbin qlever qlever-index)
QSERVER=$(nixbin qlever qlever-server)

if [ ! -f data.nt ]; then
  echo "generating dataset ($N people)…"
  python3 "$ROOT/scripts/gen-data.py" "$N" > data.nt
fi
echo "dataset: $(wc -l < data.nt) triples"

# ----------------------------------------------------------------------------- load
if [ -z "${SKIP_LOAD:-}" ]; then
  mkdir -p qlever-index
  echo '{"num-triples-per-batch": 1000000}' > qlever-index/settings.json
  hyperfine --runs 1 --style basic \
    --prepare 'rm -rf sparkles.db' --prepare 'rm -rf jena.db' --prepare 'rm -f qlever-index/bench.*' \
    --command-name sparkles "$SPARKLES load --loc sparkles.db data.nt" \
    --command-name jena-tdb2 "$TDBLOADER --loc jena.db data.nt" \
    --command-name qlever "cd qlever-index && $QINDEX -i bench -F nt -f ../data.nt -p true -s settings.json" \
    --export-markdown results/load.md --export-json results/load.json
fi

# ---------------------------------------------------------------------------- servers
SPORT=3931; JPORT=3933; QPORT=3932
"$SPARKLES" --result-cache-mb 0 serve --data sparkles-server --loc bench="$WORK/sparkles.db" --port $SPORT --timeout 600 > sparkles.log 2>&1 &
PIDS=($!)
JVM_ARGS="-Xmx8G" "$FUSEKI" --port $JPORT --loc "$WORK/jena.db" /bench > fuseki.log 2>&1 &
PIDS+=($!)
(cd qlever-index && exec "$QSERVER" -i bench -p $QPORT -m 8G -c 2G -s 600s -a bench > server.log 2>&1) &
PIDS+=($!)
trap 'kill "${PIDS[@]}" 2>/dev/null || true' EXIT
wait_for() { for _ in $(seq 1 240); do curl -sf "$1" >/dev/null 2>&1 && return 0; sleep 0.5; done; echo "timeout waiting for $1" >&2; exit 1; }
wait_for "localhost:$SPORT/\$/ping"
wait_for "localhost:$JPORT/\$/ping"
wait_for "localhost:$QPORT/?cmd=stats"

# ---------------------------------------------------------------------------- queries
P='PREFIX ex: <http://example.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> '
declare -a NAMES
add() { NAMES+=("$1"); printf '%s%s' "$P" "$2" > "queries/$1.rq"; }
add count-all      'SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }'
add types-grouped  'SELECT ?t (COUNT(?s) AS ?c) WHERE { ?s a ?t } GROUP BY ?t ORDER BY DESC(?c)'
add star-join      'SELECT ?p ?n ?a ?o WHERE { ?p a ex:Researcher ; foaf:name ?n ; foaf:age ?a ; ex:worksFor ?o . ?o ex:city "Kyoto" }'
add two-hop-count  'SELECT (COUNT(*) AS ?n) WHERE { ?a foaf:knows ?b . ?b foaf:knows ?c }'
add range-topk     'SELECT ?p ?s WHERE { ?p ex:salary ?s FILTER(?s > 150000) } ORDER BY DESC(?s) LIMIT 10'
add optional-count 'SELECT (COUNT(*) AS ?c) WHERE { ?p a ex:Student OPTIONAL { ?p ex:worksFor ?o } }'
add contains       'SELECT (COUNT(*) AS ?c) WHERE { ?p foaf:name ?n FILTER(CONTAINS(?n, "Ada")) }'
add group-avg      'SELECT ?o (AVG(?a) AS ?avg) (COUNT(?p) AS ?n) WHERE { ?p ex:worksFor ?o ; foaf:age ?a } GROUP BY ?o ORDER BY DESC(?n) ?o LIMIT 10'
add path-plus      'SELECT (COUNT(*) AS ?c) WHERE { <http://example.org/doc/1> ex:cites+ ?d }'
add distinct-obj   'SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s foaf:knows ?o }'
add export-500k    'SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 500000'

q() { # curl command for endpoint + query name (fails on HTTP errors)
  echo "curl -sf -o /dev/null -H 'Accept: text/tab-separated-values' --data-urlencode query@queries/$2.rq $1"
}
# sanity check: every engine must answer every query with the same number of rows
echo; printf '%-16s %10s %10s %10s\n' "rows" sparkles jena qlever
for n in "${NAMES[@]}"; do
  rows() { curl -sf -H 'Accept: text/tab-separated-values' --data-urlencode "query@queries/$n.rq" "$1" | tail -n +2 | wc -l; }
  printf '%-16s %10s %10s %10s\n' "$n" "$(rows localhost:$SPORT/bench/sparql)" "$(rows localhost:$JPORT/bench/sparql)" "$(rows localhost:$QPORT/)"
done

CLEAR="curl -sf -o /dev/null 'localhost:$QPORT/?cmd=clear-cache&access-token=bench'"
for n in "${NAMES[@]}"; do
  echo; echo "== $n"
  hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --prepare "$CLEAR" \
    --command-name sparkles "$(q "localhost:$SPORT/bench/sparql" "$n")" \
    --command-name jena-fuseki "$(q "localhost:$JPORT/bench/sparql" "$n")" \
    --command-name qlever "$(q "localhost:$QPORT/" "$n")" \
    --export-markdown "results/$n.md" --export-json "results/$n.json"
done

# ---------------------------------------------------------------------------- summary
python3 - "$WORK/results" "${NAMES[@]}" <<'EOF'
import json, sys, os
d, names = sys.argv[1], sys.argv[2:]
out = ["| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) |", "|---|---:|---:|---:|"]
def fmt(r): return f"{r['mean']*1000:.1f} ± {r['stddev']*1000:.1f}"
if os.path.exists(f"{d}/load.json"):
    rs = {r["command"]: r for r in json.load(open(f"{d}/load.json"))["results"]}
    out.append("| **load** (s) | " + " | ".join(f"{rs[c]['mean']:.2f}" for c in ["sparkles", "jena-tdb2", "qlever"]) + " |")
for n in names:
    rs = {r["command"]: r for r in json.load(open(f"{d}/{n}.json"))["results"]}
    best = min(rs.values(), key=lambda r: r["mean"])["command"]
    cells = [("**" + fmt(rs[c]) + "**") if c == best else fmt(rs[c]) for c in ["sparkles", "jena-fuseki", "qlever"]]
    out.append(f"| {n} | " + " | ".join(cells) + " |")
open(f"{d}/summary.md", "w").write("\n".join(out) + "\n")
print("\n".join(out))
EOF
