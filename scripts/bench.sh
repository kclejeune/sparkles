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
# MODE selects what runs (default: the load and the query suite):
# * default: loads every engine (peak RSS under GNU time, index size), then for one engine
#   at a time starts its server on the loaded store with no other server running, checks
#   its answers (and each query's peak RSS), times the queries, the update latency and the
#   throughput, reads the server's memory and stops it.
# * updates: serves a copy of every store (cp --reflink=auto into WORKDIR/scratch),
#   commits CHURN (default 5000) single-triple updates to each engine, the same for all
#   (scripts/bench-writes.py, WORKDIR/churn-CHURN.ru), then runs the query suite of the
#   default mode on the changed stores into WORKDIR/results-updates, one engine at a time
#   with the churn and the suite in the same server process.
# * mixed: for each engine alone, on a fresh copy of its store: MIXED_SECONDS (default 30)
#   of CONC readers running star-join (oha) while one writer commits single-triple
#   INSERT DATA requests as fast as it can, or at WRITE_RATE per second.
# * cold: per engine, query and run (COLD_RUNS, default 3): stops the server, evicts its
#   files from the page cache, starts it (time to ready) and times one query.
#
# Comparison engines come from this flake's pinned packages, including Fluree's release
# binary. Explicit TDBLOADER, FUSEKI, QINDEX, QSERVER, OXIGRAPH and FLUREE paths override
# them. FLUREE_VERSION, if set without FLUREE, must match the packaged version.
# Auxiliary tools prefer PATH, then the locked nixpkgs. See docs/BENCHMARKING.md.
# Env: WARMUP (default 2), RUNS (default 10), SKIP_LOAD=1 to reuse existing indexes,
# SKIP_QUERIES=1 to reuse a query's existing results when they hold every engine of the run
# (re-runs updates/throughput/RSS),
# SKIP_PROBE=1 to skip the Sparkles memory probe (scripts/rss-probe.sh),
# ANSWERS_ONLY=1 to re-check answers and rebuild the summary without timing anything,
# ENGINES="sparkles jena qlever fluree oxigraph" (default) to run a subset. Results are merged per engine
# into existing results/*.json, so e.g. ENGINES=qlever re-measures only QLever and keeps
# the other engines' numbers. QUERIES="name …" limits the row check and timings to those
# queries (e.g. to resume after an engine crashed). SPARKLES_DB (default
# WORKDIR/sparkles.db) serves another Sparkles database, and SPARKLES_ARGS adds `serve`
# flags (e.g. "--no-access-log --no-metrics"), for comparing configurations.
# PORT_BASE (default 3931) is the first of the five server ports; the memory probe uses
# PORT_BASE + 30. KEEP_SCRATCH=1 keeps the store copies of the updates and mixed modes.
# OHA names the load generator of the mixed mode instead of the flake's package.
# The query and update latency runs pin hyperfine and its curl processes to one CPU (see
# client_cpu in scripts/bench-lib.sh); CLIENT_CPU=N picks the CPU and CLIENT_CPU=none
# turns pinning off.
set -euo pipefail

N=${1:-100000}
WORK=${2:-/tmp/sparkles-bench}
WARMUP=${WARMUP:-2}
RUNS=${RUNS:-10}
MODE=${MODE:-default}
CHURN=${CHURN:-5000}
COLD_RUNS=${COLD_RUNS:-3}
MIXED_SECONDS=${MIXED_SECONDS:-30}
WRITE_RATE=${WRITE_RATE:-0}
CONC=${CONC:-16}
NREQ=${NREQ:-160}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
SPARKLES_ARGS=${SPARKLES_ARGS:-}
ENGINES=${ENGINES:-sparkles jena qlever fluree oxigraph}
case $MODE in
  default | updates | mixed | cold) ;;
  *)
    echo "MODE must be default, updates, mixed or cold" >&2
    exit 1
    ;;
esac
# absolute, so "$WORK/..." paths stay valid after the cd below
mkdir -p "$WORK/results" "$WORK/queries" "$WORK/logs"
WORK=$(cd "$WORK" && pwd)
cd "$WORK"
# shellcheck source=scripts/bench-lib.sh
source "$ROOT/scripts/bench-lib.sh"
resolve_tools
export CONC NREQ

if [ ! -f data.nt ]; then
  echo "generating dataset ($N people)…"
  python3 "$ROOT/scripts/gen-data.py" "$N" > data.nt
fi
echo "dataset: $(wc -l < data.nt) triples"

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
# predicate listings that the predicate patterns answer (X11): subjects per predicate,
# overall and for the instances of a class, and the predicates a class uses
add predicate-subjects 'SELECT ?p (COUNT(DISTINCT ?s) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p'
add class-predicates 'SELECT ?p (COUNT(DISTINCT ?s) AS ?c) WHERE { ?s a ex:Researcher . ?s ?p ?o } GROUP BY ?p'
add class-predicate-list 'SELECT DISTINCT ?p WHERE { ?s a ex:Student . ?s ?p ?o }'
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

# the update-latency triple (named graph, see below): removed before the answer check and
# after the update timing, so every engine is compared on exactly the loaded data
T='GRAPH <http://example.org/bench/g> { <http://example.org/bench/s> <http://example.org/bench/p> "v" }'
printf 'INSERT DATA { %s }' "$T" > queries/_update.ru
printf 'DELETE DATA { %s }' "$T" > queries/_delete.ru
printf 'ASK { %s }' "$T" > queries/_ask.rq

# ------------------------------------------------------------------------- the suite
suite() {
  local e n a b kv
  for e in $ENGINES; do eval "$(upd "$e" queries/_delete.ru)" || true; done
  answer_check
  [ -n "${ANSWERS_ONLY:-}" ] && return
  # timed_all <hyperfine json>: true when it has a result for every engine of ENGINES
  timed_all() {
    [ -f "$1" ] || return 1
    local names=() e
    for e in $ENGINES; do names+=("${NAME[$e]}"); done
    python3 -I - "$1" "${names[@]}" << 'EOF'
import json, sys
with open(sys.argv[1]) as f:
    have = {r.get("command") for r in json.load(f).get("results", [])}
sys.exit(0 if set(sys.argv[2:]) <= have else 1)
EOF
  }
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    # SKIP_QUERIES: a query is skipped only when its results hold every engine of this run
    [ -n "${SKIP_QUERIES:-}" ] && timed_all "$RES/$n.json" && continue
    echo
    echo "== $n"
    time_query "$n" "$RES"
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
    eval "$(upd "$e" queries/_delete.ru)"
    a=$(ask "${URL[$e]}")
    eval "$(upd "$e" queries/_update.ru)"
    b=$(ask "${URL[$e]}")
    if [ "$a" != False ] || [ "$b" != True ]; then
      echo "${NAME[$e]}: insert/delete not observable (before=$a after=$b); skipping its update timing" >&2
      continue
    fi
    ARGS+=(--prepare "$(upd "$e" queries/_delete.ru)" --command-name "${NAME[$e]}" "$(upd "$e" queries/_update.ru)")
  done
  if [ ${#ARGS[@]} -gt 0 ]; then
    "${CLIENT[@]}" hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure \
      "${ARGS[@]}" --export-json "$RES/update-latency.new.json"
    merge "$RES/update-latency.new.json" "$RES/update-latency.json"
  fi
  for e in $ENGINES; do eval "$(upd "$e" queries/_delete.ru)" || true; done

  # ------------------------------------------------------------------- concurrent throughput
  # NREQ star-join requests from CONC parallel clients (all engines without result caches,
  # so every request executes the query). Each server's peak RSS is reset before and read
  # after: each engine runs alone in its part of the hyperfine run.
  # shellcheck disable=SC2329 # called by pcmd
  par() { echo "seq $NREQ | xargs -P $CONC -I{} $(q "$1" star-join)"; }
  echo
  echo "== throughput ($NREQ requests, $CONC clients)"
  # shellcheck disable=SC2329 # called by engine_args
  pcmd() { par "${URL[$1]}"; }
  mapfile -d '' ARGS < <(engine_args pcmd)
  for e in $ENGINES; do reset_peak "$e"; done
  hyperfine --warmup 1 --runs 3 --style basic --ignore-failure --prepare "$(cache_prepare)" \
    "${ARGS[@]}" --export-json "$RES/throughput.new.json"
  merge "$RES/throughput.new.json" "$RES/throughput.json"
  kv=()
  for e in $ENGINES; do kv+=("${NAME[$e]}=$(hwm_of "$e")"); done
  setj "$RES/mem.json" throughput_peak "${kv[@]}"

  # --------------------------------------------------------------------------- memory (RSS)
  sleep 2 # idle servers may hand free memory back (Sparkles does after 1 s)
  kv=()
  for e in $ENGINES; do kv+=("${NAME[$e]}=$(rss_of "$e")"); done
  setj "$RES/rss.json" "" "${kv[@]}"
  # the decoded-block cache is part of Sparkles' RSS: the summary also shows RSS without it
  has sparkles && setj "$RES/mem.json" block_cache "sparkles=$(block_cache_of)"
  echo
  echo "RSS (MiB) after the run: $(cat "$RES/rss.json")"
}

# --------------------------------------------------------------------- updates (churn)
# CHURN single-triple commits, each its own request, to every engine in turn (the same
# file for all); churn.json gets each engine's commits per second and latency per commit
churn() {
  local cf="$WORK/churn-$CHURN.ru" e r fields
  [ -f "$cf" ] || python3 "$ROOT/scripts/bench-writes.py" gen data.nt "$CHURN" "$cf"
  for e in $ENGINES; do
    fields=()
    [ -n "${UPDFIELD[$e]:-}" ] && fields=(--field "${UPDFIELD[$e]}")
    log "$e: applying $(wc -l < "$cf") commits"
    r=$(python3 "$ROOT/scripts/bench-writes.py" apply "${UPDURL[$e]}" "$cf" "${fields[@]}")
    log "$e: $r"
    setj "$RES/churn.json" "" "${NAME[$e]}=$r"
  done
}

# ------------------------------------------------------------------- mixed read/write
# mixed <engine>: CONC readers (oha, star-join) and one writer for MIXED_SECONDS. Like the
# curl requests of the other measurements, every request opens a new connection and asks
# for an uncompressed answer: on a reused connection QLever's answers wait about 40 ms for
# a delayed TCP ACK.
mixed() {
  local e=$1 form=queries/_star-join.form fields=() w r
  python3 -c 'import sys, urllib.parse; sys.stdout.write(urllib.parse.urlencode({"query": open(sys.argv[1]).read()}))' \
    queries/star-join.rq > "$form"
  [ -n "${UPDFIELD[$e]:-}" ] && fields=(--field "${UPDFIELD[$e]}")
  python3 "$ROOT/scripts/bench-writes.py" loop "${UPDURL[$e]}" "$MIXED_SECONDS" --rate "$WRITE_RATE" "${fields[@]}" \
    > "logs/mixed-writer-$e.json" &
  w=$!
  "$OHA" -z "${MIXED_SECONDS}s" -c "$CONC" --disable-keepalive --disable-compression -m POST -T application/x-www-form-urlencoded -D "$form" \
    -A 'text/tab-separated-values' -t 300s --no-tui --output-format json -o "logs/mixed-readers-$e.json" "http://${URL[$e]}"
  wait "$w" || die "$e: the writer failed"
  r=$(
    python3 - "logs/mixed-readers-$e.json" "logs/mixed-writer-$e.json" << 'EOF'
import json, sys
o, w = json.load(open(sys.argv[1])), json.load(open(sys.argv[2]))
s = o["summary"]
codes = o.get("statusCodeDistribution", {})
ok = sum(v for k, v in codes.items() if k.startswith("2"))
# requests still running at the deadline are cut off, not failed
errs = sum(codes.values()) - ok + sum(v for k, v in o.get("errorDistribution", {}).items() if "deadline" not in k)
p = o.get("latencyPercentiles", {})
ms = lambda v: round(v * 1000, 2) if v is not None else None
print(json.dumps({"read_qps": round(ok / s["total"], 1), "read_p50_ms": ms(p.get("p50")), "read_p99_ms": ms(p.get("p99")),
                  "read_errors": errs, "writes_per_s": w["per_s"], "write_p50_ms": w["p50_ms"],
                  "write_p99_ms": w["p99_ms"], "write_errors": w["errors"]}))
EOF
  )
  log "$e mixed: $r"
  setj "$RES/mixed.json" "" "${NAME[$e]}=$r"
}

# ---------------------------------------------------------------------------- run
# each_alone <command>…: for every selected engine in turn, start its server alone, record
# its RSS 2 s after the start, run the command with ENGINES set to that engine, and stop it.
# No other engine's server runs while one is checked or timed (docs/specs/X06 section 5.3).
# The answer check runs inside each engine's turn too, and bench-answers.py keeps every
# engine's fingerprints in one answers.json, which the summary and the record compare.
each_alone() {
  local all=$ENGINES e
  for e in $all; do
    ENGINES=$e
    start "$e"
    sleep 2
    setj "$RES/mem.json" idle "${NAME[$e]}=$(rss_of "$e")"
    "$@"
    stop "$e"
  done
  ENGINES=$all
}
# the churn and then the suite on the changed store, without a restart in between: QLever
# keeps updates in memory and would lose them
churn_suite() {
  churn
  suite
}

RES=results
trap stop_all EXIT
case $MODE in
  default)
    [ -n "${SKIP_LOAD:-}" ] || load_all
    each_alone suite
    # Sparkles memory probe (scripts/rss-probe.sh): a fresh server, every query once, then
    # 3 x 160 concurrent star-join requests; RSS at each step, block-cache bytes and peak RSS
    if [ -z "${ANSWERS_ONLY:-}" ] && has sparkles && [ -z "${SKIP_PROBE:-}" ]; then
      echo
      echo "== memory probe"
      # the probe opens the database itself: stop this run's Sparkles server first
      stop sparkles
      SPARKLES="$SPARKLES" PROBE_PORT=${PROBE_PORT:-$((PORT_BASE + 30))} "$ROOT/scripts/rss-probe.sh" "$WORK"
    fi
    ;;
  updates)
    # the stores take writes: copies, removed at the end unless KEEP_SCRATCH=1
    RES=results-updates
    STORE=$WORK/scratch
    mkdir -p "$RES"
    trap 'stop_all; [ -n "${KEEP_SCRATCH:-}" ] || rm -rf "$STORE"' EXIT
    copy_stores
    each_alone churn_suite
    ;;
  mixed)
    [ -n "${ANSWERS_ONLY:-}" ] || {
      OHA=${OHA:-$(nixbin oha oha)}
      STORE=$WORK/scratch
      trap 'stop_all; [ -n "${KEEP_SCRATCH:-}" ] || rm -rf "$STORE"' EXIT
      copy_stores
      for e in $ENGINES; do
        start "$e"
        mixed "$e"
        stop "$e"
      done
    }
    ;;
  cold)
    [ -n "${ANSWERS_ONLY:-}" ] || for e in $ENGINES; do cold "$e"; done
    ;;
esac
stop_all

# ---------------------------------------------------------------------------- summary
python3 "$ROOT/scripts/bench-summary.py" "$WORK/$RES" "${NAMES[@]}"
