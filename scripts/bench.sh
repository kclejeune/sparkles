#!/usr/bin/env bash
# Load + query benchmark: Sparkles vs Apache Jena (TDB2 + Fuseki) vs QLever vs Fluree,
# via hyperfine.
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
# Jena, Fuseki and QLever come from nixpkgs when not on PATH. Fluree (BUSL-1.1, not in
# nixpkgs) is the checksum-verified release binary, downloaded to WORKDIR (Linux x86_64 /
# aarch64, macOS) unless FLUREE points at one.
# Env: WARMUP (default 2), RUNS (default 10), SKIP_LOAD=1 to reuse existing indexes,
# SKIP_QUERIES=1 to reuse existing per-query results (re-runs updates/throughput/RSS),
# ENGINES="sparkles jena qlever fluree" (default) to run a subset. Results are merged per engine
# into existing results/*.json, so e.g. ENGINES=qlever re-measures only QLever and keeps
# the other engines' numbers. QUERIES="name …" limits the row check and timings to those
# queries (e.g. to resume after an engine crashed).
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
ENGINES=${ENGINES:-sparkles jena qlever fluree}
has() { [[ " $ENGINES " == *" $1 "* ]]; }
if has jena; then
  TDBLOADER=$(nixbin apache-jena tdb2.tdbloader)
  FUSEKI=$(nixbin apache-jena-fuseki fuseki-server)
fi
if has qlever; then
  QINDEX=$(nixbin qlever qlever-index)
  QSERVER=$(nixbin qlever qlever-server)
fi

FLUREE_VERSION=${FLUREE_VERSION:-4.2.2}
if has fluree && [ -z "${FLUREE:-}" ]; then
  case "$(uname -sm)" in
    "Linux x86_64") FT=x86_64-unknown-linux-gnu ;;
    "Linux aarch64") FT=aarch64-unknown-linux-gnu ;;
    "Darwin arm64") FT=aarch64-apple-darwin ;;
    "Darwin x86_64") FT=x86_64-apple-darwin ;;
    *) echo "no Fluree release binary for $(uname -sm); set FLUREE" >&2; exit 1 ;;
  esac
  FDIR="fluree-$FLUREE_VERSION"; FLUREE="$WORK/$FDIR/fluree-db-cli-$FT/fluree"
  if [ ! -x "$FLUREE" ]; then
    mkdir -p "$FDIR"
    FURL="https://github.com/fluree/db/releases/download/v$FLUREE_VERSION/fluree-db-cli-$FT.tar.xz"
    curl -sfL "$FURL" -o "$FDIR/fluree.tar.xz"
    want=$(curl -sfL "$FURL.sha256" | cut -d' ' -f1)
    got=$(sha256sum "$FDIR/fluree.tar.xz" 2>/dev/null || shasum -a 256 "$FDIR/fluree.tar.xz"); got=${got%% *}
    [ "$want" = "$got" ] || { echo "Fluree checksum mismatch ($got != $want)" >&2; exit 1; }
    tar xJf "$FDIR/fluree.tar.xz" -C "$FDIR"
  fi
fi

# merge <new.json> <dest.json>: replace/add hyperfine results by command name
merge() {
  python3 - "$1" "$2" <<'EOF'
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
  python3 - "$@" <<'EOF'
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
  hyperfine --runs 1 --style basic "${LOAD[@]}" --export-json results/load.new.json
  merge results/load.new.json results/load.json
fi

# ---------------------------------------------------------------------------- servers
SPORT=3931; JPORT=3933; QPORT=3932; FPORT=3934
PIDS=()
trap '[ ${#PIDS[@]} -gt 0 ] && kill "${PIDS[@]}" 2>/dev/null; true' EXIT
wait_for() { for _ in $(seq 1 240); do curl -sf "$1" >/dev/null 2>&1 && return 0; sleep 0.5; done; echo "timeout waiting for $1" >&2; exit 1; }
# per engine: result name, query endpoint, update command, port (for RSS)
declare -A NAME URL UPDATE PORT
if has sparkles; then
  "$SPARKLES" --result-cache-mb 0 serve --data sparkles-server --loc bench="$WORK/sparkles.db" --port $SPORT --timeout 600 > sparkles.log 2>&1 &
  PIDS+=($!); wait_for "localhost:$SPORT/\$/ping"
  NAME[sparkles]=sparkles; URL[sparkles]=localhost:$SPORT/bench/sparql; PORT[sparkles]=$SPORT
  UPDATE[sparkles]="curl -sf -o /dev/null --data-urlencode update@queries/_update.ru localhost:$SPORT/bench/update"
fi
if has jena; then
  JVM_ARGS="-Xmx8G" "$FUSEKI" --update --port $JPORT --loc "$WORK/jena.db" /bench > fuseki.log 2>&1 &
  PIDS+=($!); wait_for "localhost:$JPORT/\$/ping"
  NAME[jena]=jena-fuseki; URL[jena]=localhost:$JPORT/bench/sparql; PORT[jena]=$JPORT
  UPDATE[jena]="curl -sf -o /dev/null --data-urlencode update@queries/_update.ru localhost:$JPORT/bench/update"
fi
if has qlever; then
  (cd qlever-index && exec "$QSERVER" -i bench -p $QPORT -m 8G -c 2G -s 600s -a bench -j 16 > server.log 2>&1) &
  PIDS+=($!); wait_for "localhost:$QPORT/?cmd=stats"
  NAME[qlever]=qlever; URL[qlever]=localhost:$QPORT/; PORT[qlever]=$QPORT
  UPDATE[qlever]="curl -sf -o /dev/null --data-urlencode update@queries/_update.ru --data-urlencode access-token=bench localhost:$QPORT/"
fi
if has fluree; then
  # property-path traversal is capped at 1M visited nodes by default (knows-reach at 10M)
  (cd fluree && FLUREE_CACHE_MAX_MB=4096 FLUREE_PATH_MAX_VISITED=20000000 FLUREE_QUERY_TIMEOUT_MS=600000 \
    exec "$FLUREE" server run --listen-addr 127.0.0.1:$FPORT --storage-path "$WORK/fluree/.fluree/storage" --log-level warn > ../fluree.log 2>&1) &
  PIDS+=($!); wait_for "localhost:$FPORT/health"
  NAME[fluree]=fluree; URL[fluree]=localhost:$FPORT/v1/fluree/query/bench:main; PORT[fluree]=$FPORT
  UPDATE[fluree]="curl -sf -o /dev/null --data-urlencode update@queries/_update.ru localhost:$FPORT/v1/fluree/update/bench:main"
fi
# hyperfine arguments for every selected engine: engine_args <command-fn>
engine_args() { for e in $ENGINES; do printf '%s\0' --command-name "${NAME[$e]}" "$($1 "$e")"; done; }

# ---------------------------------------------------------------------------- queries
P='PREFIX ex: <http://example.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> '
declare -a NAMES
add() { NAMES+=("$1"); printf '%s%s' "$P" "$2" > "queries/$1.rq"; }
selected() { [ -z "${QUERIES:-}" ] || [[ " $QUERIES " == *" $1 "* ]]; }
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
echo; printf '%-16s' rows; for e in $ENGINES; do printf ' %12s' "${NAME[$e]}"; done; echo
for n in "${NAMES[@]}"; do
  selected "$n" || continue
  rows() { curl -sf --max-time "${MAX_TIME:-300}" -H 'Accept: text/tab-separated-values' --data-urlencode "query@queries/$n.rq" "$1" > "rows.$$.tsv" && tail -n +2 "rows.$$.tsv" | wc -l || echo error; }
  printf '%-16s' "$n"; kv=()
  for e in $ENGINES; do r=$(rows "${URL[$e]}"); printf ' %12s' "$r"; kv+=("${NAME[$e]}=$r"); done; echo
  setj results/rows.json "$n" "${kv[@]}"
done
rm -f "rows.$$.tsv"

# clears QLever's result cache before every run (a no-op without QLever)
CLEAR="true"; has qlever && CLEAR="curl -sf -o /dev/null 'localhost:$QPORT/?cmd=clear-cache&access-token=bench'"
for n in "${NAMES[@]}"; do
  selected "$n" || continue
  [ -n "${SKIP_QUERIES:-}" ] && [ -f "results/$n.json" ] && continue
  echo; echo "== $n"
  qcmd() { q "${URL[$1]}" "$n"; }
  mapfile -d '' ARGS < <(engine_args qcmd)
  hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure --prepare "$CLEAR" \
    "${ARGS[@]}" --export-json "results/$n.new.json"
  merge "results/$n.new.json" "results/$n.json"
done

# ------------------------------------------------------------------------ update latency
# a single-triple INSERT DATA (idempotent, so every run does the same work), into a named
# graph so the persisted triple never shows up in the default-graph queries above
printf '%s' 'INSERT DATA { GRAPH <http://example.org/bench/g> { <http://example.org/bench/s> <http://example.org/bench/p> "v" } }' > queries/_update.ru
echo; echo "== update-latency"
ucmd() { echo "${UPDATE[$1]}"; }
mapfile -d '' ARGS < <(engine_args ucmd)
hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure \
  "${ARGS[@]}" --export-json results/update-latency.new.json
merge results/update-latency.new.json results/update-latency.json

# ------------------------------------------------------------------- concurrent throughput
# 160 star-join requests from 16 parallel clients
CONC=${CONC:-16}; NREQ=${NREQ:-160}
par() { echo "seq $NREQ | xargs -P $CONC -I{} $(q "$1" star-join)"; }
echo; echo "== throughput ($NREQ requests, $CONC clients)"
pcmd() { par "${URL[$1]}"; }
mapfile -d '' ARGS < <(engine_args pcmd)
hyperfine --warmup 1 --runs 3 --style basic --ignore-failure --prepare "$CLEAR" \
  "${ARGS[@]}" --export-json results/throughput.new.json
merge results/throughput.new.json results/throughput.json

# --------------------------------------------------------------------------- memory (RSS)
rss() { local pid; pid=$(ss -ltnp 2>/dev/null | grep ":$1 " | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | head -1); \
  awk '/VmRSS/ {printf "%.0f", $2/1024}' "/proc/$pid/status" 2>/dev/null || echo "?"; }
kv=(); for e in $ENGINES; do kv+=("${NAME[$e]}=$(rss "${PORT[$e]}")"); done
setj results/rss.json "" "${kv[@]}"
echo; echo "RSS (MiB) after the run: $(cat results/rss.json)"

# ---------------------------------------------------------------------------- summary
python3 - "$WORK/results" "${NAMES[@]}" <<'EOF'
import json, sys, os
d, names = sys.argv[1], sys.argv[2:]
ORDER = ["sparkles", "jena-fuseki", "qlever", "fluree"]
LOADNAME = {"jena-fuseki": "jena-tdb2"}
def load(f):
    return {r["command"]: r for r in json.load(open(f))["results"]} if os.path.exists(f) else {}
seen = set()
for n in names + ["update-latency", "throughput"]:
    seen |= set(load(f"{d}/{n}.json"))
cmds = [c for c in ORDER if c in seen] + sorted(seen - set(ORDER))
out = ["| query | " + " | ".join(f"{c} (ms)" for c in cmds) + " |", "|---|" + "---:|" * len(cmds)]
def fmt(r): return f"{r['mean']*1000:.1f} ± {r['stddev']*1000:.1f}"
def failed(r): return r is None or any(e != 0 for e in r.get("exit_codes", []))
def row(label, rs, bad=frozenset(), f=fmt, better=min, key=lambda r: r["mean"], err="error", mark=frozenset()):
    ok = {c: rs[c] for c in cmds if c in rs and c not in bad and not failed(rs[c])}
    best = better(ok, key=lambda c: key(ok[c])) if ok else None
    cells = []
    for c in cmds:
        if c not in rs: cells.append("—")
        elif c not in ok: cells.append(err)
        else: cells.append(("**%s**" if c == best else "%s") % f(ok[c]) + (" †" if c in mark else ""))
    out.append(f"| {label} | " + " | ".join(cells) + " |")
lr = load(f"{d}/load.json")
if lr:
    row("**load** (s)", {c: lr[LOADNAME.get(c, c)] for c in cmds if LOADNAME.get(c, c) in lr}, f=lambda r: f"{r['mean']:.2f}")
rows = json.load(open(f"{d}/rows.json")) if os.path.exists(f"{d}/rows.json") else {}
notes = []
for n in names:
    rn = rows.get(n, {})
    counts = {c: v for c, v in rn.items() if v != "error"}
    # an engine returning a different row count than the majority gets a footnote
    ref = max(set(counts.values()), key=list(counts.values()).count) if counts else None
    bad = {c for c, v in rn.items() if v == "error"}
    wrong = {c for c, v in counts.items() if v != ref and list(counts.values()).count(ref) > 1}
    rs = load(f"{d}/{n}.json")
    row(n, rs, bad, err="error", mark=wrong)
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
if notes:
    out += [""] + notes
open(f"{d}/summary.md", "w").write("\n".join(out) + "\n")
print("\n".join(out))
EOF
