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
# Env: WARMUP (default 2), RUNS (default 10), SKIP_LOAD=1 to reuse existing indexes,
# SKIP_QUERIES=1 to reuse existing per-query results (re-runs updates/throughput/RSS).
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
JVM_ARGS="-Xmx8G" "$FUSEKI" --update --port $JPORT --loc "$WORK/jena.db" /bench > fuseki.log 2>&1 &
PIDS+=($!)
(cd qlever-index && exec "$QSERVER" -i bench -p $QPORT -m 8G -c 2G -s 600s -a bench -j 16 > server.log 2>&1) &
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
# extended shapes: known strengths of the other engines and stress cases
add predicate-counts 'SELECT ?p (COUNT(*) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p'
add order-by-full  'SELECT ?p ?n WHERE { ?p foaf:name ?n } ORDER BY ?n'
add optional-chain 'SELECT ?p ?o ?c WHERE { ?p a ex:Student OPTIONAL { ?p ex:advisor ?a . ?a ex:worksFor ?o OPTIONAL { ?o ex:city ?c } } }'
add minus          'SELECT (COUNT(*) AS ?c) WHERE { ?p a ex:Researcher MINUS { ?p ex:authorOf ?d } }'
add subquery-agg   'SELECT ?o ?n WHERE { { SELECT ?o (COUNT(?p) AS ?n) WHERE { ?p ex:worksFor ?o } GROUP BY ?o } FILTER(?n > 55) }'
add regex-iri      'SELECT (COUNT(*) AS ?c) WHERE { ?s foaf:name ?o FILTER(REGEX(STR(?s), "person/1[0-9]{3}$")) }'
add knows-reach    'SELECT (COUNT(*) AS ?c) WHERE { <http://example.org/person/0> foaf:knows* ?x }'
add distinct-join  'SELECT DISTINCT ?o WHERE { ?a foaf:knows ?b . ?b ex:worksFor ?o }'
add lang-filter    'SELECT (COUNT(*) AS ?c) WHERE { ?d ex:title ?t FILTER(LANGMATCHES(LANG(?t), "en")) }'

q() { # curl command for endpoint + query name (fails on HTTP errors)
  echo "curl -sf --max-time ${MAX_TIME:-300} -o /dev/null -H 'Accept: text/tab-separated-values' --data-urlencode query@queries/$2.rq $1"
}
# sanity check: every engine must answer every query with the same number of rows;
# an engine that errors (HTTP failure) is reported as "error" instead of a time
echo; printf '%-16s %10s %10s %10s\n' "rows" sparkles jena qlever
echo '{}' > results/rows.json
for n in "${NAMES[@]}"; do
  rows() { curl -sf -H 'Accept: text/tab-separated-values' --data-urlencode "query@queries/$n.rq" "$1" > "rows.$$.tsv" && tail -n +2 "rows.$$.tsv" | wc -l || echo error; }
  rs=$(rows localhost:$SPORT/bench/sparql); rj=$(rows localhost:$JPORT/bench/sparql); rq=$(rows localhost:$QPORT/)
  printf '%-16s %10s %10s %10s\n' "$n" "$rs" "$rj" "$rq"
  python3 -c "import json,sys; d=json.load(open('results/rows.json')); d[sys.argv[1]]={'sparkles':sys.argv[2].strip(),'jena-fuseki':sys.argv[3].strip(),'qlever':sys.argv[4].strip()}; json.dump(d,open('results/rows.json','w'))" "$n" "$rs" "$rj" "$rq"
done
rm -f "rows.$$.tsv"

CLEAR="curl -sf -o /dev/null 'localhost:$QPORT/?cmd=clear-cache&access-token=bench'"
for n in "${NAMES[@]}"; do
  [ -n "${SKIP_QUERIES:-}" ] && [ -f "results/$n.json" ] && continue
  echo; echo "== $n"
  hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure --prepare "$CLEAR" \
    --command-name sparkles "$(q "localhost:$SPORT/bench/sparql" "$n")" \
    --command-name jena-fuseki "$(q "localhost:$JPORT/bench/sparql" "$n")" \
    --command-name qlever "$(q "localhost:$QPORT/" "$n")" \
    --export-markdown "results/$n.md" --export-json "results/$n.json"
done

# ------------------------------------------------------------------------ update latency
# a single-triple INSERT DATA (idempotent, so every run does the same work)
UPD='INSERT DATA { <http://example.org/bench/s> <http://example.org/bench/p> "v" }'
echo; echo "== update-latency"
hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure \
  --command-name sparkles "curl -sf -o /dev/null --data-urlencode 'update=$UPD' localhost:$SPORT/bench/update" \
  --command-name jena-fuseki "curl -sf -o /dev/null --data-urlencode 'update=$UPD' localhost:$JPORT/bench/update" \
  --command-name qlever "curl -sf -o /dev/null --data-urlencode 'update=$UPD' --data-urlencode access-token=bench localhost:$QPORT/" \
  --export-json results/update-latency.json

# ------------------------------------------------------------------- concurrent throughput
# 160 star-join requests from 16 parallel clients
CONC=${CONC:-16}; NREQ=${NREQ:-160}
par() { echo "seq $NREQ | xargs -P $CONC -I{} $(q "$1" star-join)"; }
echo; echo "== throughput ($NREQ requests, $CONC clients)"
hyperfine --warmup 1 --runs 3 --style basic --ignore-failure --prepare "$CLEAR" \
  --command-name sparkles "$(par "localhost:$SPORT/bench/sparql")" \
  --command-name jena-fuseki "$(par "localhost:$JPORT/bench/sparql")" \
  --command-name qlever "$(par "localhost:$QPORT/")" \
  --export-json results/throughput.json

# --------------------------------------------------------------------------- memory (RSS)
rss() { local pid; pid=$(ss -ltnp 2>/dev/null | grep ":$1 " | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | head -1); \
  awk '/VmRSS/ {printf "%.0f", $2/1024}' "/proc/$pid/status" 2>/dev/null || echo "?"; }
printf '{"sparkles": "%s", "jena-fuseki": "%s", "qlever": "%s"}\n' "$(rss $SPORT)" "$(rss $JPORT)" "$(rss $QPORT)" > results/rss.json
echo; echo "RSS (MiB) after the run: $(cat results/rss.json)"

# ---------------------------------------------------------------------------- summary
python3 - "$WORK/results" "${NAMES[@]}" <<'EOF'
import json, sys, os
d, names = sys.argv[1], sys.argv[2:]
out = ["| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) |", "|---|---:|---:|---:|"]
def fmt(r): return f"{r['mean']*1000:.1f} ± {r['stddev']*1000:.1f}"
if os.path.exists(f"{d}/load.json"):
    rs = {r["command"]: r for r in json.load(open(f"{d}/load.json"))["results"]}
    out.append("| **load** (s) | " + " | ".join(f"{rs[c]['mean']:.2f}" for c in ["sparkles", "jena-tdb2", "qlever"]) + " |")
rows = json.load(open(f"{d}/rows.json")) if os.path.exists(f"{d}/rows.json") else {}
for n in names:
    rs = {r["command"]: r for r in json.load(open(f"{d}/{n}.json"))["results"]}
    bad = {c for c, v in rows.get(n, {}).items() if v == "error"}
    ok = [r for r in rs.values() if r["command"] not in bad]
    best = min(ok, key=lambda r: r["mean"])["command"] if ok else None
    cells = ["error" if c in bad else ("**" + fmt(rs[c]) + "**") if c == best else fmt(rs[c]) for c in ["sparkles", "jena-fuseki", "qlever"]]
    out.append(f"| {n} | " + " | ".join(cells) + " |")
cmds = ["sparkles", "jena-fuseki", "qlever"]
if os.path.exists(f"{d}/update-latency.json"):
    rs = {r["command"]: r for r in json.load(open(f"{d}/update-latency.json"))["results"]}
    failed = {c for c in cmds if any(e != 0 for e in rs[c].get("exit_codes", []))}
    ok = [rs[c] for c in cmds if c not in failed]
    best = min(ok, key=lambda r: r["mean"])["command"] if ok else None
    out.append("| **update** (1-triple INSERT DATA) | " + " | ".join(
        "error" if c in failed else ("**" + fmt(rs[c]) + "**") if c == best else fmt(rs[c]) for c in cmds) + " |")
if os.path.exists(f"{d}/throughput.json"):
    rs = {r["command"]: r for r in json.load(open(f"{d}/throughput.json"))["results"]}
    n = int(os.environ.get("NREQ", "160"))
    failed = {c for c in cmds if any(e != 0 for e in rs[c].get("exit_codes", []))}
    qps = {c: n / rs[c]["mean"] for c in cmds if c not in failed}
    best = max(qps, key=qps.get) if qps else None
    out.append("| **throughput** star-join, 16 clients (queries/s) | " + " | ".join(
        "timeout/error" if c in failed else ("**%.0f**" if c == best else "%.0f") % qps[c] for c in cmds) + " |")
if os.path.exists(f"{d}/rss.json"):
    rss = json.load(open(f"{d}/rss.json"))
    out.append("| **server RSS** after the run (MiB) | " + " | ".join(str(rss[c]) for c in cmds) + " |")
open(f"{d}/summary.md", "w").write("\n".join(out) + "\n")
print("\n".join(out))
EOF
