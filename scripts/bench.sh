#!/usr/bin/env bash
# Load + query benchmark: Sparkles vs Apache Jena (TDB2 + Fuseki) vs QLever vs Fluree vs
# Oxigraph, via hyperfine.
#
#   scripts/bench.sh [N_PEOPLE] [WORKDIR]
#
# All engines are queried over HTTP (SPARQL protocol, TSV results) so JVM start-up is not
# measured. No engine may answer from a result cache: Sparkles runs with
# --result-cache-mb 0, QLever with --cache-max-size-single-entry 0B (and its cache is also
# cleared before every timed run), and Fuseki, Fluree and Oxigraph have none. Before timing, every
# engine's answer to every query is fingerprinted (scripts/bench-answers.py) and
# compared: an engine whose answer differs in value from the majority is footnoted and
# not ranked. Timed samples that fail are reported as errors, with the failure count.
# Results go to WORKDIR/results/*.{md,json} and a combined
# WORKDIR/results/summary.md.
#
# Jena, Fuseki, QLever and Oxigraph come from nixpkgs when not on PATH. Fluree (BUSL-1.1, not in
# nixpkgs) is the checksum-verified release binary, downloaded to WORKDIR (Linux x86_64 /
# aarch64, macOS) unless FLUREE points at one.
# Env: WARMUP (default 2), RUNS (default 10), SKIP_LOAD=1 to reuse existing indexes,
# SKIP_QUERIES=1 to reuse existing per-query results (re-runs updates/throughput/RSS),
# SKIP_PROBE=1 to skip the Sparkles memory probe (scripts/rss-probe.sh),
# ANSWERS_ONLY=1 to re-check answers and rebuild the summary without timing anything,
# ENGINES="sparkles jena qlever fluree oxigraph" (default) to run a subset. Results are merged per engine
# into existing results/*.json, so e.g. ENGINES=qlever re-measures only QLever and keeps
# the other engines' numbers. QUERIES="name …" limits the row check and timings to those
# queries (e.g. to resume after an engine crashed). SPARKLES_DB (default
# WORKDIR/sparkles.db) serves another Sparkles database, and SPARKLES_ARGS adds `serve`
# flags (e.g. "--no-access-log --no-metrics"), for comparing configurations.
set -euo pipefail

N=${1:-100000}
WORK=${2:-/tmp/sparkles-bench}
WARMUP=${WARMUP:-2}
RUNS=${RUNS:-10}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
SPARKLES_ARGS=${SPARKLES_ARGS:-}
# absolute, so "$WORK/..." paths stay valid after the cd below
mkdir -p "$WORK/results" "$WORK/queries"
WORK=$(cd "$WORK" && pwd)
cd "$WORK"

nixbin() { # nixbin <pkg> <bin>
  if command -v "$2" > /dev/null; then command -v "$2"; else echo "$(nix build "nixpkgs#$1" --no-link --print-out-paths | tail -1)/bin/$2"; fi
}
ENGINES=${ENGINES:-sparkles jena qlever fluree oxigraph}
has() { [[ " $ENGINES " == *" $1 "* ]]; }
if has jena; then
  TDBLOADER=$(nixbin apache-jena tdb2.tdbloader)
  FUSEKI=$(nixbin apache-jena-fuseki fuseki-server)
fi
if has qlever; then
  QINDEX=$(nixbin qlever qlever-index)
  QSERVER=$(nixbin qlever qlever-server)
fi
if has oxigraph; then OXIGRAPH=$(nixbin oxigraph oxigraph); fi

FLUREE_VERSION=${FLUREE_VERSION:-4.2.2}
if has fluree && [ -z "${FLUREE:-}" ]; then
  case "$(uname -sm)" in
    "Linux x86_64") FT=x86_64-unknown-linux-gnu ;;
    "Linux aarch64") FT=aarch64-unknown-linux-gnu ;;
    "Darwin arm64") FT=aarch64-apple-darwin ;;
    "Darwin x86_64") FT=x86_64-apple-darwin ;;
    *)
      echo "no Fluree release binary for $(uname -sm); set FLUREE" >&2
      exit 1
      ;;
  esac
  FDIR="fluree-$FLUREE_VERSION"
  FLUREE="$WORK/$FDIR/fluree-db-cli-$FT/fluree"
  if [ ! -x "$FLUREE" ]; then
    mkdir -p "$FDIR"
    FURL="https://github.com/fluree/db/releases/download/v$FLUREE_VERSION/fluree-db-cli-$FT.tar.xz"
    curl -sfL "$FURL" -o "$FDIR/fluree.tar.xz"
    want=$(curl -sfL "$FURL.sha256" | cut -d' ' -f1)
    got=$(sha256sum "$FDIR/fluree.tar.xz" 2> /dev/null || shasum -a 256 "$FDIR/fluree.tar.xz")
    got=${got%% *}
    [ "$want" = "$got" ] || {
      echo "Fluree checksum mismatch ($got != $want)" >&2
      exit 1
    }
    tar xJf "$FDIR/fluree.tar.xz" -C "$FDIR"
  fi
fi

# merge <new.json> <dest.json>: replace/add hyperfine results by command name
merge() {
  python3 - "$1" "$2" << 'EOF'
import json, os, sys
new, dest = sys.argv[1], sys.argv[2]
n = json.load(open(new))
old = json.load(open(dest)) if os.path.exists(dest) else {"results": []}
cmds = {r["command"] for r in n["results"]}
old["results"] = [r for r in old["results"] if r["command"] not in cmds] + n["results"]
json.dump(old, open(dest, "w"), indent=1)
EOF
  rm -f "$1"
}
# setj <file> <key> <engine>=<value>…: merge values into a JSON object (rows, RSS)
setj() {
  python3 - "$@" << 'EOF'
import json, os, sys
f, key, kvs = sys.argv[1], sys.argv[2], sys.argv[3:]
d = json.load(open(f)) if os.path.exists(f) else {}
t = d.setdefault(key, {}) if key else d
for kv in kvs:
    k, v = kv.split("=", 1)
    t[k] = v.strip()
json.dump(d, open(f, "w"), indent=1)
EOF
}

if [ ! -f data.nt ]; then
  echo "generating dataset ($N people)…"
  python3 "$ROOT/scripts/gen-data.py" "$N" > data.nt
fi
echo "dataset: $(wc -l < data.nt) triples"

# ----------------------------------------------------------------------------- load
if [ -z "${SKIP_LOAD:-}" ]; then
  LOAD=()
  if has sparkles; then LOAD+=(--prepare 'rm -rf sparkles.db' --command-name sparkles "$SPARKLES load --loc sparkles.db data.nt"); fi
  if has jena; then LOAD+=(--prepare 'rm -rf jena.db' --command-name jena-tdb2 "$TDBLOADER --loc jena.db data.nt"); fi
  if has qlever; then
    mkdir -p qlever-index
    echo '{"num-triples-per-batch": 1000000}' > qlever-index/settings.json
    LOAD+=(--prepare 'rm -f qlever-index/bench.*' --command-name qlever "cd qlever-index && $QINDEX -i bench -F nt -f ../data.nt -p true -s settings.json")
  fi
  # Fluree's bulk import (`create --from`) builds the index directly; without
  # --chunk-size-mb it parses the file as a single chunk (about 3x slower)
  if has fluree; then LOAD+=(--prepare 'rm -rf fluree' --command-name fluree "mkdir -p fluree && cd fluree && $FLUREE init -q && $FLUREE --memory-budget-mb 8192 create bench --from ../data.nt --chunk-size-mb 16"); fi
  # Oxigraph's bulk loader (parallel, writes RocksDB files directly), then the compaction
  # it recommends before read-heavy workloads (`optimize`); both are timed as the load
  if has oxigraph; then LOAD+=(--prepare 'rm -rf oxigraph.db' --command-name oxigraph "$OXIGRAPH load --location oxigraph.db --file data.nt && $OXIGRAPH optimize --location oxigraph.db"); fi
  hyperfine --runs 1 --style basic "${LOAD[@]}" --export-json results/load.new.json
  merge results/load.new.json results/load.json
fi

# ---------------------------------------------------------------------------- servers
SPORT=3931
JPORT=3933
QPORT=3932
FPORT=3934
OPORT=3935
PIDS=()
# on exit, also stop whatever listens on a selected engine's port: `fuseki-server` is a
# wrapper script whose JVM outlives it
stop_all() {
  # a server may already be gone: a failed lookup or kill must not fail the run (set -e,
  # pipefail)
  [ ${#PIDS[@]} -gt 0 ] && { kill "${PIDS[@]}" 2> /dev/null || true; }
  for p in "${PORT[@]}"; do
    pid=$(ss -ltnp 2> /dev/null | grep ":$p " | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | head -1 || true)
    [ -n "$pid" ] && { kill "$pid" 2> /dev/null || true; }
  done
  true
}
trap stop_all EXIT
wait_for() {
  for _ in $(seq 1 240); do
    curl -sf "$1" > /dev/null 2>&1 && return 0
    sleep 0.5
  done
  echo "timeout waiting for $1" >&2
  exit 1
}
# per engine: result name, query endpoint, update command (printf template taking the
# update file under queries/), port (for RSS)
declare -A NAME URL UPDATE PORT
if has sparkles; then
  # shellcheck disable=SC2086 # SPARKLES_ARGS is a list of flags
  "$SPARKLES" --result-cache-mb 0 serve --data sparkles-server --loc bench="${SPARKLES_DB:-$WORK/sparkles.db}" --port $SPORT --timeout 600 $SPARKLES_ARGS > sparkles.log 2>&1 &
  SPID=$!
  PIDS+=("$SPID")
  wait_for "localhost:$SPORT/\$/ping"
  NAME[sparkles]=sparkles
  URL[sparkles]=localhost:$SPORT/bench/sparql
  PORT[sparkles]=$SPORT
  UPDATE[sparkles]="curl -sf -o /dev/null --data-urlencode update@queries/%s localhost:$SPORT/bench/update"
fi
if has jena; then
  JVM_ARGS="-Xmx8G" "$FUSEKI" --update --port $JPORT --loc "$WORK/jena.db" /bench > fuseki.log 2>&1 &
  PIDS+=($!)
  wait_for "localhost:$JPORT/\$/ping"
  NAME[jena]=jena-fuseki
  URL[jena]=localhost:$JPORT/bench/sparql
  PORT[jena]=$JPORT
  UPDATE[jena]="curl -sf -o /dev/null --data-urlencode update@queries/%s localhost:$JPORT/bench/update"
fi
if has qlever; then
  (cd qlever-index && exec "$QSERVER" -i bench -p $QPORT -m 8G -c 2G -e 0B -s 600s -a bench -j 16 > server.log 2>&1) &
  PIDS+=($!)
  wait_for "localhost:$QPORT/?cmd=stats"
  NAME[qlever]=qlever
  URL[qlever]=localhost:$QPORT/
  PORT[qlever]=$QPORT
  UPDATE[qlever]="curl -sf -o /dev/null --data-urlencode update@queries/%s --data-urlencode access-token=bench localhost:$QPORT/"
fi
if has fluree; then
  # property-path traversal is capped at 1M visited nodes by default (knows-reach at 10M)
  (cd fluree && FLUREE_CACHE_MAX_MB=4096 FLUREE_PATH_MAX_VISITED=20000000 FLUREE_QUERY_TIMEOUT_MS=600000 \
    exec "$FLUREE" server run --listen-addr 127.0.0.1:$FPORT --storage-path "$WORK/fluree/.fluree/storage" --log-level warn > ../fluree.log 2>&1) &
  PIDS+=($!)
  wait_for "localhost:$FPORT/health"
  NAME[fluree]=fluree
  URL[fluree]=localhost:$FPORT/v1/fluree/query/bench:main
  PORT[fluree]=$FPORT
  UPDATE[fluree]="curl -sf -o /dev/null --data-urlencode update@queries/%s localhost:$FPORT/v1/fluree/update/bench:main"
fi
if has oxigraph; then
  "$OXIGRAPH" serve --location "$WORK/oxigraph.db" --bind 127.0.0.1:$OPORT --timeout-s 600 > oxigraph.log 2>&1 &
  PIDS+=($!)
  wait_for "localhost:$OPORT/query?query=ASK%7B%7D"
  NAME[oxigraph]=oxigraph
  URL[oxigraph]=localhost:$OPORT/query
  PORT[oxigraph]=$OPORT
  UPDATE[oxigraph]="curl -sf -o /dev/null --data-urlencode update@queries/%s localhost:$OPORT/update"
fi
# the update command of engine $1 for the update file $2 (UPDATE[] values are printf
# templates taking the file name)
upd() {
  # shellcheck disable=SC2059
  printf "${UPDATE[$1]}" "$2"
}
# hyperfine arguments for every selected engine: engine_args <command-fn>
engine_args() { for e in $ENGINES; do printf '%s\0' --command-name "${NAME[$e]}" "$($1 "$e")"; done; }

# ---------------------------------------------------------------------------- queries
P='PREFIX ex: <http://example.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> '
declare -a NAMES
add() {
  NAMES+=("$1")
  printf '%s%s' "$P" "$2" > "queries/$1.rq"
}
selected() { [ -z "${QUERIES:-}" ] || [[ " $QUERIES " == *" $1 "* ]]; }
add count-all 'SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }'
add types-grouped 'SELECT ?t (COUNT(?s) AS ?c) WHERE { ?s a ?t } GROUP BY ?t ORDER BY DESC(?c)'
add star-join 'SELECT ?p ?n ?a ?o WHERE { ?p a ex:Researcher ; foaf:name ?n ; foaf:age ?a ; ex:worksFor ?o . ?o ex:city "Kyoto" }'
add two-hop-count 'SELECT (COUNT(*) AS ?n) WHERE { ?a foaf:knows ?b . ?b foaf:knows ?c }'
add range-topk 'SELECT ?p ?s WHERE { ?p ex:salary ?s FILTER(?s > 150000) } ORDER BY DESC(?s) LIMIT 10'
add optional-count 'SELECT (COUNT(*) AS ?c) WHERE { ?p a ex:Student OPTIONAL { ?p ex:worksFor ?o } }'
add contains 'SELECT (COUNT(*) AS ?c) WHERE { ?p foaf:name ?n FILTER(CONTAINS(?n, "Ada")) }'
add group-avg 'SELECT ?o (AVG(?a) AS ?avg) (COUNT(?p) AS ?n) WHERE { ?p ex:worksFor ?o ; foaf:age ?a } GROUP BY ?o ORDER BY DESC(?n) ?o LIMIT 10'
add path-plus 'SELECT (COUNT(*) AS ?c) WHERE { <http://example.org/doc/1> ex:cites+ ?d }'
add distinct-obj 'SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s foaf:knows ?o }'
add export-500k 'SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 500000'
# extended shapes: known strengths of the other engines and stress cases
add predicate-counts 'SELECT ?p (COUNT(*) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p'
add order-by-full 'SELECT ?p ?n WHERE { ?p foaf:name ?n } ORDER BY ?n'
add optional-chain 'SELECT ?p ?o ?c WHERE { ?p a ex:Student OPTIONAL { ?p ex:advisor ?a . ?a ex:worksFor ?o OPTIONAL { ?o ex:city ?c } } }'
add minus 'SELECT (COUNT(*) AS ?c) WHERE { ?p a ex:Researcher MINUS { ?p ex:authorOf ?d } }'
add not-exists 'SELECT (COUNT(*) AS ?c) WHERE { ?p a ex:Researcher FILTER NOT EXISTS { ?p ex:authorOf ?d } }'
add exists-join 'SELECT (COUNT(*) AS ?c) WHERE { ?s a ex:Student ; ex:advisor ?a FILTER EXISTS { ?a ex:worksFor ?o . ?o ex:city "Kyoto" } }'
add subquery-agg 'SELECT ?o ?n WHERE { { SELECT ?o (COUNT(?p) AS ?n) WHERE { ?p ex:worksFor ?o } GROUP BY ?o } FILTER(?n > 55) }'
add regex-iri 'SELECT (COUNT(*) AS ?c) WHERE { ?s foaf:name ?o FILTER(REGEX(STR(?s), "person/1[0-9]{3}$")) }'
add knows-reach 'SELECT (COUNT(*) AS ?c) WHERE { <http://example.org/person/0> foaf:knows* ?x }'
add distinct-join 'SELECT DISTINCT ?o WHERE { ?a foaf:knows ?b . ?b ex:worksFor ?o }'
add lang-filter 'SELECT (COUNT(*) AS ?c) WHERE { ?d ex:title ?t FILTER(LANGMATCHES(LANG(?t), "en")) }'
# selective inputs into large patterns: lookups by key rather than full scans
add star-lookup 'SELECT ?p ?n ?a ?s WHERE { ?p ex:worksFor <http://example.org/org/7> ; foaf:name ?n ; foaf:age ?a ; ex:salary ?s }'
add values-star 'SELECT ?p ?n ?a ?k WHERE { VALUES ?p { <http://example.org/person/1> <http://example.org/person/10> <http://example.org/person/100> <http://example.org/person/1000> <http://example.org/person/10000> } ?p foaf:name ?n ; foaf:age ?a ; foaf:knows ?k }'
add employee-docs 'SELECT ?p ?d ?t WHERE { ?p ex:worksFor <http://example.org/org/7> . ?p ex:authorOf ?d . ?d ex:title ?t }'
# expressions over values that repeat (years, ages): BIND, ORDER BY keys and aggregate arguments
add expr-bind-group 'SELECT ?decade (COUNT(*) AS ?c) WHERE { ?d ex:year ?y BIND(FLOOR(?y / 10) * 10 AS ?decade) } GROUP BY ?decade ORDER BY ?decade'
add expr-order-key 'SELECT ?p ?a WHERE { ?p foaf:age ?a } ORDER BY DESC(ABS(?a - 50)) ?p LIMIT 10'
add expr-agg-arg 'SELECT ?o (SUM(?a * 12) AS ?months) WHERE { ?p ex:worksFor ?o ; foaf:age ?a } GROUP BY ?o ORDER BY DESC(?months) ?o LIMIT 10'

q() { # curl command for endpoint + query name (fails on HTTP errors)
  echo "curl -sf --max-time ${MAX_TIME:-300} -o /dev/null -H 'Accept: text/tab-separated-values' --data-urlencode query@queries/$2.rq $1"
}
# the update-latency triple (named graph, see below): removed before the answer check and
# after the update timing, so every engine is compared on exactly the loaded data
T='GRAPH <http://example.org/bench/g> { <http://example.org/bench/s> <http://example.org/bench/p> "v" }'
printf 'INSERT DATA { %s }' "$T" > queries/_update.ru
printf 'DELETE DATA { %s }' "$T" > queries/_delete.ru
printf 'ASK { %s }' "$T" > queries/_ask.rq
for e in $ENGINES; do eval "$(upd "$e" _delete.ru)" || true; done

# answer check: every engine's answer to every query is fingerprinted (row count, exact
# RDF terms, numeric values) into results/answers.json; the summary compares engines. An
# engine that errors (HTTP failure) is reported as "error" instead of a time. LIMIT
# without ORDER BY may legitimately return different solutions: row counts only.
COUNT_ONLY="export-500k"
echo
printf '%-16s' rows
for e in $ENGINES; do printf ' %12s' "${NAME[$e]}"; done
echo
for n in "${NAMES[@]}"; do
  selected "$n" || continue
  flag=()
  [[ " $COUNT_ONLY " == *" $n "* ]] && flag=(--count-only)
  printf '%-16s' "$n"
  for e in $ENGINES; do
    r=$(python3 "$ROOT/scripts/bench-answers.py" "${URL[$e]}" "queries/$n.rq" results/answers.json "$n" "${NAME[$e]}" "${flag[@]}")
    printf ' %12s' "$r"
  done
  echo
done

if [ -z "${ANSWERS_ONLY:-}" ]; then
  # clears QLever's result cache before every run (a no-op without QLever)
  CLEAR="true"
  has qlever && CLEAR="curl -sf -o /dev/null 'localhost:$QPORT/?cmd=clear-cache&access-token=bench'"
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    [ -n "${SKIP_QUERIES:-}" ] && [ -f "results/$n.json" ] && continue
    echo
    echo "== $n"
    qcmd() { q "${URL[$1]}" "$n"; }
    mapfile -d '' ARGS < <(engine_args qcmd)
    hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure --prepare "$CLEAR" \
      "${ARGS[@]}" --export-json "results/$n.new.json"
    merge "results/$n.new.json" "results/$n.json"
  done

  # ------------------------------------------------------------------------ update latency
  # a single-triple INSERT DATA of a triple that is not present: an untimed --prepare deletes
  # it before every run, so every timed request performs a real insertion. It goes into a
  # named graph so it never shows up in the default-graph queries above. Durability is each
  # engine's default: Sparkles fsyncs its WAL, TDB2 commits durably, QLever keeps updates
  # in memory only, Fluree commits to its log, and Oxigraph commits a RocksDB transaction
  # (see docs/BENCHMARKS.md).
  ask() { curl -sf --max-time 30 -H 'Accept: application/sparql-results+json' --data-urlencode query@queries/_ask.rq "$1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["boolean"])' 2> /dev/null || echo error; }
  echo
  echo "== update-latency"
  ARGS=()
  for e in $ENGINES; do
    # the mutation must be observable, or the engine's timings would measure a no-op
    eval "$(upd "$e" _delete.ru)"
    a=$(ask "${URL[$e]}")
    eval "$(upd "$e" _update.ru)"
    b=$(ask "${URL[$e]}")
    if [ "$a" != False ] || [ "$b" != True ]; then
      echo "${NAME[$e]}: insert/delete not observable (before=$a after=$b); skipping its update timing" >&2
      continue
    fi
    ARGS+=(--prepare "$(upd "$e" _delete.ru)" --command-name "${NAME[$e]}" "$(upd "$e" _update.ru)")
  done
  if [ ${#ARGS[@]} -gt 0 ]; then
    hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure \
      "${ARGS[@]}" --export-json results/update-latency.new.json
    merge results/update-latency.new.json results/update-latency.json
  fi
  for e in $ENGINES; do eval "$(upd "$e" _delete.ru)" || true; done

  # ------------------------------------------------------------------- concurrent throughput
  # 160 star-join requests from 16 parallel clients (all engines without result caches, so
  # every request executes the query)
  CONC=${CONC:-16}
  NREQ=${NREQ:-160}
  par() { echo "seq $NREQ | xargs -P $CONC -I{} $(q "$1" star-join)"; }
  echo
  echo "== throughput ($NREQ requests, $CONC clients)"
  pcmd() { par "${URL[$1]}"; }
  mapfile -d '' ARGS < <(engine_args pcmd)
  hyperfine --warmup 1 --runs 3 --style basic --ignore-failure --prepare "$CLEAR" \
    "${ARGS[@]}" --export-json results/throughput.new.json
  merge results/throughput.new.json results/throughput.json

  # --------------------------------------------------------------------------- memory (RSS)
  rss() {
    local pid
    pid=$(ss -ltnp 2> /dev/null | grep ":$1 " | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | head -1 || true)
    awk '/VmRSS/ {printf "%.0f", $2/1024}' "/proc/$pid/status" 2> /dev/null || echo "?"
  }
  sleep 2 # idle servers may hand free memory back (Sparkles does after 1 s)
  kv=()
  for e in $ENGINES; do kv+=("${NAME[$e]}=$(rss "${PORT[$e]}")"); done
  setj results/rss.json "" "${kv[@]}"
  echo
  echo "RSS (MiB) after the run: $(cat results/rss.json)"

  # Sparkles memory probe (scripts/rss-probe.sh): a fresh server, every query once, then
  # 3 x 160 concurrent star-join requests; RSS at each step, block-cache bytes and peak RSS
  if has sparkles && [ -z "${SKIP_PROBE:-}" ]; then
    echo
    echo "== memory probe"
    # the probe opens the database itself: stop this run's Sparkles server first
    kill "$SPID" 2> /dev/null
    wait "$SPID" 2> /dev/null || true
    SPARKLES="$SPARKLES" "$ROOT/scripts/rss-probe.sh" "$WORK"
  fi
fi # ANSWERS_ONLY

# ---------------------------------------------------------------------------- summary
python3 - "$WORK/results" "${NAMES[@]}" << 'EOF'
import json, sys, os
d, names = sys.argv[1], sys.argv[2:]
ORDER = ["sparkles", "jena-fuseki", "qlever", "fluree", "oxigraph"]
LOADNAME = {"jena-fuseki": "jena-tdb2"}
def load(f):
    return {r["command"]: r for r in json.load(open(f))["results"]} if os.path.exists(f) else {}
seen = set()
for n in names + ["update-latency", "throughput"]:
    seen |= set(load(f"{d}/{n}.json"))
cmds = [c for c in ORDER if c in seen] + sorted(seen - set(ORDER))
out = ["| query | " + " | ".join(f"{c} (ms)" for c in cmds) + " |", "|---|" + "---:|" * len(cmds)]
def fmt(r): return f"{r['mean']*1000:.1f} ± {r['stddev']*1000:.1f}"
def nfailed(r): return sum(1 for e in r.get("exit_codes", []) if e != 0)
def row(label, rs, bad=frozenset(), f=fmt, better=min, key=lambda r: r["mean"], err="error",
        mark=None, unranked=frozenset()):
    mark = mark or {}
    ok = {c: rs[c] for c in cmds if c in rs and c not in bad and nfailed(rs[c]) == 0}
    rank = {c: r for c, r in ok.items() if c not in unranked}
    best = better(rank, key=lambda c: key(rank[c])) if rank else None
    cells = []
    for c in cmds:
        if c not in rs: cells.append("—")
        elif c in bad: cells.append(err)
        elif c not in ok:
            k = nfailed(rs[c]); cells.append(f"{err} ({k}/{len(rs[c].get('exit_codes', []))} runs failed)")
        else: cells.append(("**%s**" if c == best else "%s") % f(ok[c]) + mark.get(c, ""))
    out.append(f"| {label} | " + " | ".join(cells) + " |")
def majority(vals):
    """the value most engines agree on, if at least two do"""
    vs = list(vals.values())
    if not vs: return None
    ref = max(set(vs), key=vs.count)
    return ref if vs.count(ref) > 1 else None
lr = load(f"{d}/load.json")
if lr:
    row("**load** (s)", {c: lr[LOADNAME.get(c, c)] for c in cmds if LOADNAME.get(c, c) in lr}, f=lambda r: f"{r['mean']:.2f}")
answers = json.load(open(f"{d}/answers.json")) if os.path.exists(f"{d}/answers.json") else {}
rows = json.load(open(f"{d}/rows.json")) if os.path.exists(f"{d}/rows.json") else {}
notes = []
for n in names:
    rs = load(f"{d}/{n}.json")
    if n in answers:
        an = answers[n]
        bad = {c for c, v in an.items() if v["rows"] == "error"}
        ok = {c: v for c, v in an.items() if c not in bad}
        # value-level disagreement with the majority: shown, footnoted, not ranked
        by_value = {c: v.get("value", v["rows"]) for c, v in ok.items()}
        ref = majority(by_value)
        wrong = {c for c, v in by_value.items() if ref is not None and v != ref}
        # same values but different RDF terms (lexical forms): footnoted, still ranked
        same = {c: v["exact"] for c, v in ok.items() if c not in wrong and "exact" in v}
        eref = majority(same)
        lexical = {c for c, v in same.items() if eref is not None and v != eref}
        mark = {c: " †" for c in wrong} | {c: " ‡" for c in lexical}
        row(n, rs, bad, err="error", mark=mark, unranked=wrong)
        notes += [f"† `{n}`: {c} returned a different answer ({an[c]['rows']} rows) than the majority; not ranked" for c in sorted(wrong)]
        notes += [f"‡ `{n}`: {c} returned equal values as different RDF terms (numeric datatype or lexical form)" for c in sorted(lexical)]
    else:
        # older runs recorded row counts only
        rn = rows.get(n, {})
        counts = {c: v for c, v in rn.items() if v != "error"}
        ref = majority(counts)
        bad = {c for c, v in rn.items() if v == "error"}
        wrong = {c for c, v in counts.items() if ref is not None and v != ref}
        row(n, rs, bad, err="error", mark={c: " †" for c in wrong})
        notes += [f"† `{n}`: {c} returned {rn[c]} rows, the majority {ref}" for c in sorted(wrong)]
rs = load(f"{d}/update-latency.json")
if rs: row("**update** (1-triple INSERT DATA)", rs)
rs = load(f"{d}/throughput.json")
if rs:
    n = int(os.environ.get("NREQ", "160"))
    row("**throughput** star-join, 16 clients (queries/s)", rs, f=lambda r: "%.0f" % (n / r["mean"]),
        better=max, key=lambda r: n / r["mean"], err="timeout/error")
if os.path.exists(f"{d}/rss.json"):
    rss = json.load(open(f"{d}/rss.json"))
    out.append("| **server RSS** after the run (MiB) | " + " | ".join(str(rss.get(c, "—")) for c in cmds) + " |")
if os.path.exists(f"{d}/rss-probe.json"):
    pr = json.load(open(f"{d}/rss-probe.json"))
    cell = lambda c, k: str(pr[c][k]) if c in pr and k in pr[c] else "—"
    out.append("| **RSS probe**, fresh server: after the queries / after 3×160 star-join (MiB) | "
               + " | ".join(f"{cell(c, 'after_queries_mib')} / {cell(c, 'after_round3_mib')}" if c in pr else "—" for c in cmds) + " |")
if notes:
    out += [""] + notes
open(f"{d}/summary.md", "w").write("\n".join(out) + "\n")
print("\n".join(out))
EOF
