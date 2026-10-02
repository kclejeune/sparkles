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
# * default: loads every engine (peak RSS under GNU time, index size), starts the servers
#   on the loaded stores, checks answers (and each query's peak RSS), times the queries,
#   the update latency and the throughput, and reads the servers' memory.
# * updates: serves a copy of every store (cp --reflink=auto into WORKDIR/scratch),
#   commits CHURN (default 5000) single-triple updates to each engine, the same for all
#   (scripts/bench-writes.py, WORKDIR/churn-CHURN.ru), then runs the query suite of the
#   default mode on the changed stores into WORKDIR/results-updates.
# * mixed: for each engine alone, on a fresh copy of its store: MIXED_SECONDS (default 30)
#   of CONC readers running star-join (oha) while one writer commits single-triple
#   INSERT DATA requests as fast as it can, or at WRITE_RATE per second.
# * cold: per engine, query and run (COLD_RUNS, default 3): stops the server, evicts its
#   files from the page cache, starts it (time to ready) and times one query.
#
# Jena, Fuseki, QLever, Oxigraph and oha come from nixpkgs when not on PATH. Fluree
# (BUSL-1.1, not in nixpkgs) is the checksum-verified release binary, downloaded to WORKDIR
# (Linux x86_64 / aarch64, macOS) unless FLUREE points at one.
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
# PORT_BASE (default 3931) is the first of the five server ports; the memory probe uses
# PORT_BASE + 30. KEEP_SCRATCH=1 keeps the store copies of the updates and mixed modes.
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
# hyperfine arguments for every selected engine: engine_args <command-fn>
engine_args() { for e in $ENGINES; do printf '%s\0' --command-name "${NAME[$e]}" "$($1 "$e")"; done; }
# the rise a - b as JSON (0 if negative: the kernel's RSS counters are approximate), or "?"
minus() { if [[ $1 =~ ^[0-9]+$ && $2 =~ ^[0-9]+$ ]]; then echo $(($1 > $2 ? $1 - $2 : 0)); else echo '"?"'; fi; }
# a reading as JSON: the number, or "?"
num() { if [[ $1 =~ ^[0-9]+$ ]]; then echo "$1"; else echo '"?"'; fi; }

# ------------------------------------------------------------------------------- load
# Each load runs under GNU time for its peak RSS (the largest of the processes it waits
# for); load.json and load-details.json get the time, peak RSS and index size.
load_all() {
  local gtime e cmd t0 t1
  gtime=$(gnu_time)
  for e in $ENGINES; do
    case $e in
      sparkles) cmd="'$SPARKLES' load --loc sparkles.db data.nt" ;;
      jena) cmd="'$TDBLOADER' --loc jena.db data.nt" ;;
      qlever)
        cmd="mkdir -p qlever-index && cd qlever-index && echo '{\"num-triples-per-batch\": 1000000}' > settings.json &&
          '$QINDEX' -i bench -F nt -f ../data.nt -p true -s settings.json"
        ;;
      # Fluree's bulk import (`create --from`) builds the index directly; without
      # --chunk-size-mb it parses the file as a single chunk (about 3x slower)
      fluree) cmd="mkdir -p fluree && cd fluree && '$FLUREE' init -q && '$FLUREE' --memory-budget-mb 8192 create bench --from ../data.nt --chunk-size-mb 16" ;;
      # Oxigraph's bulk loader (parallel, writes RocksDB files directly), then the compaction
      # it recommends before read-heavy workloads (`optimize`); both are timed as the load
      oxigraph) cmd="'$OXIGRAPH' load --location oxigraph.db --file data.nt && '$OXIGRAPH' optimize --location oxigraph.db" ;;
    esac
    rm -rf "${WORK:?}/${FILEOF[$e]}"
    log "$e: loading (log: logs/load-$e.log)"
    t0=$(date +%s.%N)
    "$gtime" -v -o "logs/load-$e.time" bash -c "$cmd" > "logs/load-$e.log" 2>&1 || die "$e: load failed, see logs/load-$e.log"
    t1=$(date +%s.%N)
    record_load results "${LOADNAME[$e]}" "$(awk "BEGIN {print $t1 - $t0}")" "logs/load-$e.time" "$WORK/${FILEOF[$e]}"
  done
}

# ------------------------------------------------------------------------- the suite
# start_all: every selected engine's server on STORE; mem.json gets each one's RSS
# 2 s after all are ready, before any query
start_all() {
  local e kv=()
  for e in $ENGINES; do start "$e"; done
  sleep 2
  for e in $ENGINES; do kv+=("${NAME[$e]}=$(rss_of "$e")"); done
  setj "$RES/mem.json" idle "${kv[@]}"
}

# answer check: every engine's answer to every query is fingerprinted (row count, exact
# RDF terms, numeric values) into answers.json; the summary compares engines. An engine
# that errors (HTTP failure) is reported as "error" instead of a time. LIMIT without
# ORDER BY may legitimately return different solutions: row counts only.
# Each request is also the query's memory reading: the server's peak RSS is reset
# (clear_refs) before it, and mem.json gets the peak (VmHWM) after it and its rise over
# the RSS at the reset.
COUNT_ONLY="export-500k"
answer_check() {
  local n e r before peak kv
  echo
  printf '%-16s' rows
  for e in $ENGINES; do printf ' %12s' "${NAME[$e]}"; done
  echo
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    local flag=()
    [[ " $COUNT_ONLY " == *" $n "* ]] && flag=(--count-only)
    printf '%-16s' "$n"
    kv=()
    for e in $ENGINES; do
      # right after the reset, the peak is the RSS: the baseline (a JVM may shrink between
      # a separate RSS reading and the reset)
      reset_peak "$e"
      before=$(hwm_of "$e")
      r=$(python3 "$ROOT/scripts/bench-answers.py" "${URL[$e]}" "queries/$n.rq" "$RES/answers.json" "$n" "${NAME[$e]}" "${flag[@]}")
      peak=$(hwm_of "$e")
      kv+=("${NAME[$e]}={\"before\": $(num "$before"), \"peak\": $(num "$peak"), \"delta\": $(minus "$peak" "$before")}")
      printf ' %12s' "$r"
    done
    setj "$RES/mem.json" "queries.$n" "${kv[@]}"
    echo
  done
  # the RSS after every query ran once (a fixed warm-up), read once the servers are idle
  sleep 2
  kv=()
  for e in $ENGINES; do kv+=("${NAME[$e]}=$(rss_of "$e")"); done
  setj "$RES/mem.json" warm "${kv[@]}"
}

suite() {
  local e n a b kv
  for e in $ENGINES; do eval "$(upd "$e" queries/_delete.ru)" || true; done
  answer_check
  [ -n "${ANSWERS_ONLY:-}" ] && return
  # clears QLever's result cache before every run (a no-op without QLever)
  local clear=true
  has qlever && clear="curl -sf -o /dev/null 'localhost:${PORT[qlever]}/?cmd=clear-cache&access-token=bench'"
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    [ -n "${SKIP_QUERIES:-}" ] && [ -f "$RES/$n.json" ] && continue
    echo
    echo "== $n"
    qcmd() { q "${URL[$1]}" "$n"; }
    mapfile -d '' ARGS < <(engine_args qcmd)
    hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure --prepare "$clear" \
      "${ARGS[@]}" --export-json "$RES/$n.new.json"
    merge "$RES/$n.new.json" "$RES/$n.json"
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
    hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure \
      "${ARGS[@]}" --export-json "$RES/update-latency.new.json"
    merge "$RES/update-latency.new.json" "$RES/update-latency.json"
  fi
  for e in $ENGINES; do eval "$(upd "$e" queries/_delete.ru)" || true; done

  # ------------------------------------------------------------------- concurrent throughput
  # NREQ star-join requests from CONC parallel clients (all engines without result caches,
  # so every request executes the query). Each server's peak RSS is reset before and read
  # after: each engine runs alone in its part of the hyperfine run.
  par() { echo "seq $NREQ | xargs -P $CONC -I{} $(q "$1" star-join)"; }
  echo
  echo "== throughput ($NREQ requests, $CONC clients)"
  pcmd() { par "${URL[$1]}"; }
  mapfile -d '' ARGS < <(engine_args pcmd)
  for e in $ENGINES; do reset_peak "$e"; done
  hyperfine --warmup 1 --runs 3 --style basic --ignore-failure --prepare "$clear" \
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

# --------------------------------------------------------------------------- cold
# cold <engine>: per query, COLD_RUNS times: stop, evict, start, one timed query.
# cold.json gets the median time (the format bench-billion.sh writes), cold-runs.json
# every run's time and time to ready.
cold() {
  local e=$1 n i t times ready
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    times=()
    ready=()
    for i in $(seq 1 "$COLD_RUNS"); do
      stop "$e"
      evict "$e"
      start "$e"
      ready+=("${READY_S[$e]}")
      # curl prints the time even when the request fails, so a failure replaces it
      if ! t=$(curl -sf --max-time "${MAX_TIME:-300}" -o /dev/null -w '%{time_total}' -H 'Accept: text/tab-separated-values' \
        --data-urlencode "query@queries/$n.rq" "${URL[$e]}"); then
        t=error
      fi
      times+=("$t")
      log "cold $e $n run $i: ready ${READY_S[$e]} s, query $t s"
    done
    stop "$e"
    python3 - "$RES" "${NAME[$e]}" "$n" "${times[*]}" "${ready[*]}" << 'EOF'
import json, os, statistics, sys
d, e, n, times, ready = sys.argv[1:]
times, ready = times.split(), [float(r) for r in ready.split()]
ok = [float(t) for t in times if t != "error"]
def put(f, v):
    j = json.load(open(f)) if os.path.exists(f) else {}
    j.setdefault(e, {})[n] = v
    json.dump(j, open(f, "w"), indent=1)
put(f"{d}/cold.json", statistics.median(ok) if ok else "error")
put(f"{d}/cold-runs.json", {"times": [float(t) if t != "error" else t for t in times], "ready": ready})
EOF
  done
}

# ---------------------------------------------------------------------------- run
RES=results
trap stop_all EXIT
case $MODE in
  default)
    [ -n "${SKIP_LOAD:-}" ] || load_all
    start_all
    suite
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
    start_all
    churn
    suite
    ;;
  mixed)
    [ -n "${ANSWERS_ONLY:-}" ] || {
      OHA=$(nixbin oha oha)
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
