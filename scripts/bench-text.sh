#!/usr/bin/env bash
# Full-text search benchmark: Sparkles (Tantivy, text:query) vs Jena Fuseki with jena-text
# (Lucene, text:query) vs QLever (ql:contains-word over a text index built from literals),
# via hyperfine.
#
#   scripts/bench-text.sh [N_PEOPLE] [WORKDIR]
#
# The data is the synthetic dataset of scripts/bench.sh (scripts/gen-data.py, N_PEOPLE
# people, about 10.5 triples each). Each engine first loads it into its own store
# (untimed), then builds its text index, which is timed and measured on disk. Sparkles and
# Jena index foaf:name, ex:title and rdfs:label. QLever indexes every literal, since its
# text index from literals cannot be limited to predicates. The queries join with the
# predicate instead.
#
# All engines are queried over HTTP (SPARQL protocol, TSV results), without result
# caches: Sparkles runs with --result-cache-mb 0, QLever with -e 0B and a cache clear
# before every timed run, and Fuseki has none. Before timing, every engine's answer is
# fingerprinted (scripts/bench-answers.py) and compared on hit counts and hit sets.
# Scores and their order are not compared, because the engines rank differently.
# Results go to WORKDIR/results/text-*.json and WORKDIR/results/text-summary.md.
#
# Fuseki 5.1.0 (whose fuseki-server.jar bundles jena-text and Lucene), Jena's
# tdb2.tdbloader and QLever come from nixpkgs when not on PATH.
# Env: WARMUP (default 2), RUNS (default 10), ENGINES="sparkles jena qlever" (default),
# QUERIES="name …" to limit the queries, SKIP_LOAD=1 to reuse the stores and text indexes
# of an earlier run, ANSWERS_ONLY=1 to check answers and rebuild the summary without
# timing queries, DATA=path/to/data.nt to reuse a generated dataset (for example
# scripts/bench.sh's), PORT_BASE (default 3940; the servers listen on PORT_BASE+1..+3).
set -euo pipefail

N=${1:-100000}
WORK=${2:-/tmp/sparkles-bench-text}
WARMUP=${WARMUP:-2}
RUNS=${RUNS:-10}
PORT_BASE=${PORT_BASE:-3940}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
mkdir -p "$WORK/results" "$WORK/queries"
WORK=$(cd "$WORK" && pwd)
cd "$WORK"

nixbin() { # nixbin <pkg> <bin>
  if command -v "$2" > /dev/null; then command -v "$2"; else echo "$(nix build "nixpkgs#$1" --no-link --print-out-paths | tail -1)/bin/$2"; fi
}
ENGINES=${ENGINES:-sparkles jena qlever}
has() { [[ " $ENGINES " == *" $1 "* ]]; }
if has jena; then
  TDBLOADER=$(nixbin apache-jena tdb2.tdbloader)
  FUSEKI=$(nixbin apache-jena-fuseki fuseki-server)
  # jena.textindexer runs from the Fuseki jar, with the Java the Fuseki wrapper uses
  FHOME=$(dirname "$(readlink -f "$FUSEKI")")
  [ -f "$FHOME/fuseki-server.jar" ] || FHOME=$(dirname "$FHOME")
  FJAR=$FHOME/fuseki-server.jar
  JDK=$(grep -o "/nix/store/[^'\":]*openjdk[^/'\":]*" "$FUSEKI" 2> /dev/null | head -1 || true)
  JAVA=java
  [ -n "$JDK" ] && JAVA=$JDK/bin/java
fi
if has qlever; then
  QINDEX=$(nixbin qlever qlever-index)
  QSERVER=$(nixbin qlever qlever-server)
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
# setj <file> <engine>=<value>…: merge values into a JSON object
setj() {
  python3 - "$@" << 'EOF'
import json, os, sys
f, kvs = sys.argv[1], sys.argv[2:]
d = json.load(open(f)) if os.path.exists(f) else {}
for kv in kvs:
    k, v = kv.split("=", 1)
    d[k] = v.strip()
json.dump(d, open(f, "w"), indent=1)
EOF
}
bytes() { du -scb "$@" 2> /dev/null | tail -1 | cut -f1; }

if [ ! -f data.nt ]; then
  if [ -n "${DATA:-}" ]; then
    ln -s "$(readlink -f "$DATA")" data.nt
  else
    echo "generating dataset ($N people)…"
    python3 "$ROOT/scripts/gen-data.py" "$N" > data.nt
  fi
fi
echo "dataset: $(wc -l < data.nt) triples"

FOAF=http://xmlns.com/foaf/0.1/
EX=http://example.org/
RDFS=http://www.w3.org/2000/01/rdf-schema#

# the jena-text assembler: TDB2 wrapped in a text dataset whose Lucene index (standard
# analyzer, the default) has one field per predicate and stores the literals, so
# text:query can return them
cat > fuseki-text.ttl << EOF
@prefix fuseki: <http://jena.apache.org/fuseki#> .
@prefix rdf:    <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix tdb2:   <http://jena.apache.org/2016/tdb#> .
@prefix text:   <http://jena.apache.org/text#> .

<#service> rdf:type fuseki:Service ;
    fuseki:name "bench" ;
    fuseki:endpoint [ fuseki:operation fuseki:query ; fuseki:name "sparql" ] ;
    fuseki:dataset <#text_dataset> .

<#text_dataset> rdf:type text:TextDataset ;
    text:dataset <#tdb_dataset> ;
    text:index <#lucene> .

<#tdb_dataset> rdf:type tdb2:DatasetTDB2 ;
    tdb2:location "$WORK/jena.db" .

<#lucene> rdf:type text:TextIndexLucene ;
    text:directory <file:$WORK/jena-text> ;
    text:storeValues true ;
    text:entityMap <#entMap> .

<#entMap> rdf:type text:EntityMap ;
    text:entityField "uri" ;
    text:uidField "uid" ;
    text:defaultField "name" ;
    text:map (
        [ text:field "name" ; text:predicate <${FOAF}name> ]
        [ text:field "title" ; text:predicate <${EX}title> ]
        [ text:field "label" ; text:predicate <${RDFS}label> ]
    ) .
EOF

# ----------------------------------------------------------------------- stores, text indexes
if [ -z "${SKIP_LOAD:-}" ]; then
  # the stores themselves are not timed (scripts/bench.sh measures loading)
  if has sparkles; then
    rm -rf sparkles.db
    "$SPARKLES" load --loc sparkles.db data.nt > sparkles-load.log 2>&1
  fi
  if has jena; then
    rm -rf jena.db
    "$TDBLOADER" --loc jena.db data.nt > jena-load.log 2>&1
  fi
  if has qlever; then
    rm -rf qlever-index
    mkdir -p qlever-index
    echo '{"num-triples-per-batch": 1000000}' > qlever-index/settings.json
    (cd qlever-index && "$QINDEX" -i bench -F nt -f ../data.nt -p true -s settings.json > load.log 2>&1)
  fi

  BUILD=()
  if has sparkles; then
    BUILD+=(--prepare "$SPARKLES text-index --loc sparkles.db --disable > /dev/null 2>&1 || true"
      --command-name sparkles
      "$SPARKLES text-index --loc sparkles.db --predicate ${FOAF}name --predicate ${EX}title --predicate ${RDFS}label")
  fi
  if has jena; then
    BUILD+=(--prepare 'rm -rf jena-text' --command-name jena-fuseki
      "$JAVA -Xmx8G -cp $FJAR jena.textindexer --desc=fuseki-text.ttl")
  fi
  if has qlever; then
    # explicit scoring (the default): -S bm25 and -S tf-idf fail on language-tagged
    # literals in QLever 0.5.48 (an assertion in TextScoring.cpp)
    BUILD+=(--prepare 'rm -f qlever-index/bench.text.*' --command-name qlever
      "cd qlever-index && $QINDEX -i bench -A -W > text.log 2>&1")
  fi
  hyperfine --runs 1 --style basic "${BUILD[@]}" --export-json results/text-build.new.json
  merge results/text-build.new.json results/text-build.json
  kv=()
  has sparkles && kv+=("sparkles=$(bytes sparkles.db/text)")
  has jena && kv+=("jena-fuseki=$(bytes jena-text)")
  has qlever && kv+=("qlever=$(bytes qlever-index/bench.text.*)")
  setj results/text-size.json "${kv[@]}"
fi

# ---------------------------------------------------------------------------- servers
SPORT=$((PORT_BASE + 1))
QPORT=$((PORT_BASE + 2))
JPORT=$((PORT_BASE + 3))
PIDS=()
declare -A NAME URL PORT
stop_all() {
  [ ${#PIDS[@]} -gt 0 ] && { kill "${PIDS[@]}" 2> /dev/null || true; }
  # fuseki-server is a wrapper script whose JVM outlives it: stop what listens on the ports
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
if has sparkles; then
  PORT[sparkles]=$SPORT
  "$SPARKLES" --result-cache-mb 0 serve --loc bench="$WORK/sparkles.db" --port "$SPORT" --timeout 600 > sparkles.log 2>&1 &
  PIDS+=($!)
  wait_for "localhost:$SPORT/\$/ping"
  NAME[sparkles]=sparkles
  URL[sparkles]=localhost:$SPORT/bench/sparql
fi
if has jena; then
  PORT[jena]=$JPORT
  JVM_ARGS="-Xmx8G" "$FUSEKI" --config="$WORK/fuseki-text.ttl" --port "$JPORT" > fuseki.log 2>&1 &
  PIDS+=($!)
  wait_for "localhost:$JPORT/\$/ping"
  NAME[jena]=jena-fuseki
  URL[jena]=localhost:$JPORT/bench/sparql
fi
if has qlever; then
  PORT[qlever]=$QPORT
  (cd qlever-index && exec "$QSERVER" -i bench -t -p "$QPORT" -m 8G -c 2G -e 0B -s 600s -a bench -j 16 > server.log 2>&1) &
  PIDS+=($!)
  wait_for "localhost:$QPORT/?cmd=stats"
  NAME[qlever]=qlever
  URL[qlever]=localhost:$QPORT/
fi

# ---------------------------------------------------------------------------- queries
# Each query has a text:query form (Sparkles and Jena run the same text) and a QLever form.
# The words are ones the three tokenizers treat alike. Lucene's standard analyzer, Tantivy's
# simple tokenizer and QLever all split the generated literals at spaces, hyphens and
# parentheses and lowercase them, and none of them stems or drops stop words here. A
# search that should return every hit passes an explicit limit, because jena-text
# otherwise stops at 10,000 hits. Sparkles allows a million without a limit.
P='PREFIX ex: <http://example.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX text: <http://jena.apache.org/text#> PREFIX ql: <http://qlever.cs.uni-freiburg.de/builtin-functions/> '
ALL=100000000
declare -a NAMES
declare -A CHECK
add() { # add <name> <check: set|count> <text:query form> <qlever form>
  NAMES+=("$1")
  CHECK[$1]=$2
  printf '%s%s' "$P" "$3" > "queries/$1.tq.rq"
  printf '%s%s' "$P" "$4" > "queries/$1.ql.rq"
}
qtext() { # the QLever pattern binding ?lit (a literal containing the words) and ?s
  printf '?t ql:contains-word "%s" ; ql:contains-entity ?lit . ?s %s ?lit' "$1" "$2"
}
# 1. a rare word: an id number, the only rare tokens of the generated names (1-2 hits)
add rare-top10 set \
  'SELECT ?s ?lit WHERE { (?s ?score ?lit) text:query (foaf:name "1999" 10) } ORDER BY DESC(?score) LIMIT 10' \
  "SELECT ?s ?lit WHERE { $(qtext 1999 foaf:name) } ORDER BY DESC(?ql_score_t_var_lit) LIMIT 10"
# 2. a common word (a first name, about 5% of the people), top 10 by score. Every hit has
# the same score in each engine, so only the row count is compared.
add common-top10 count \
  'SELECT ?s ?lit WHERE { (?s ?score ?lit) text:query (foaf:name "ada" 10) } ORDER BY DESC(?score) LIMIT 10' \
  "SELECT ?s ?lit WHERE { $(qtext ada foaf:name) } ORDER BY DESC(?ql_score_t_var_lit) LIMIT 10"
# 3. the number of all hits of the common word
add common-count set \
  "SELECT (COUNT(*) AS ?c) WHERE { (?s ?score ?lit) text:query (foaf:name \"ada\" $ALL) }" \
  "SELECT (COUNT(*) AS ?c) WHERE { $(qtext ada foaf:name) }"
# 4. text hits joined with a structural pattern: people named Ada who work for an
# organization in Kyoto
add text-join set \
  "SELECT ?s ?lit ?o WHERE { (?s ?score ?lit) text:query (foaf:name \"ada\" $ALL) . ?s ex:worksFor ?o . ?o ex:city \"Kyoto\" }" \
  "SELECT ?s ?lit ?o WHERE { $(qtext ada foaf:name) . ?s ex:worksFor ?o . ?o ex:city \"Kyoto\" }"
# 5. two words that must both occur. The words of ql:contains-word must all occur, while
# text:query joins words with OR unless told otherwise.
add conjunction set \
  "SELECT ?s ?lit WHERE { (?s ?score ?lit) text:query (foaf:name \"ada AND lovelace\" $ALL) }" \
  "SELECT ?s ?lit WHERE { $(qtext 'ada lovelace' foaf:name) }"
# There is no prefix query. Sparkles' query syntax has a prefix only at the end of a phrase
# of two or more words ("ada lov"*), and a single-word al* finds nothing. jena-text and
# QLever take al*, but QLever returns a row per matching word, so "Alan Allen" counts twice.

selected() { [ -z "${QUERIES:-}" ] || [[ " $QUERIES " == *" $1 "* ]]; }
qfile() { if [ "$1" = qlever ]; then echo "queries/$2.ql.rq"; else echo "queries/$2.tq.rq"; fi; }

# answer check: hit counts and hit sets (?s, ?lit and the joined ?o; never the score)
echo
printf '%-14s' rows
for e in $ENGINES; do printf ' %12s' "${NAME[$e]}"; done
echo
for n in "${NAMES[@]}"; do
  selected "$n" || continue
  flag=()
  [ "${CHECK[$n]}" = count ] && flag=(--count-only)
  printf '%-14s' "$n"
  for e in $ENGINES; do
    r=$(python3 "$ROOT/scripts/bench-answers.py" "${URL[$e]}" "$(qfile "$e" "$n")" results/text-answers.json "$n" "${NAME[$e]}" "${flag[@]}")
    if [[ $n == *-count ]]; then
      # a COUNT has one row: show (and keep) the number of hits instead
      r=$(curl -sf --max-time "${MAX_TIME:-300}" -H 'Accept: text/tab-separated-values' \
        --data-urlencode "query@$(qfile "$e" "$n")" "${URL[$e]}" | sed -n '2s/^"\{0,1\}\([0-9]*\).*/\1/p' || echo error)
      setj results/text-hits.json "$n:${NAME[$e]}=$r"
    fi
    printf ' %12s' "$r"
  done
  echo
done

if [ -z "${ANSWERS_ONLY:-}" ]; then
  CLEAR="true"
  has qlever && CLEAR="curl -sf -o /dev/null 'localhost:$QPORT/?cmd=clear-cache&access-token=bench'"
  for n in "${NAMES[@]}"; do
    selected "$n" || continue
    echo
    echo "== $n"
    ARGS=()
    for e in $ENGINES; do
      ARGS+=(--command-name "${NAME[$e]}"
        "curl -sf --max-time ${MAX_TIME:-300} -o /dev/null -H 'Accept: text/tab-separated-values' --data-urlencode query@$(qfile "$e" "$n") ${URL[$e]}")
    done
    hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic --ignore-failure --prepare "$CLEAR" \
      "${ARGS[@]}" --export-json "results/text-$n.new.json"
    merge "results/text-$n.new.json" "results/text-$n.json"
  done
fi

# ---------------------------------------------------------------------------- summary
python3 - "$WORK/results" "$(wc -l < data.nt)" "${NAMES[@]}" << 'EOF'
import json, os, sys
d, triples, names = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
ORDER = ["sparkles", "jena-fuseki", "qlever"]
def load(f):
    p = f"{d}/{f}"
    return json.load(open(p)) if os.path.exists(p) else {}
def hf(f):
    return {r["command"]: r for r in load(f).get("results", [])}
build, size, answers = hf("text-build.json"), load("text-size.json"), load("text-answers.json")
hits = load("text-hits.json")
seen = set(build) | set(size) | {e for v in answers.values() for e in v}
for n in names:
    seen |= set(hf(f"text-{n}.json"))
cols = [c for c in ORDER if c in seen] + sorted(seen - set(ORDER))
out = [f"# Full-text search: {triples:,} triples", ""]
def table(head, rows):
    out.append("| " + " | ".join(head) + " |")
    out.append("|---|" + "---:|" * (len(head) - 1))
    out.extend("| " + " | ".join(r) + " |" for r in rows)
    out.append("")
out.append("## Text index build")
out.append("")
table(["", *cols], [
    ["build (s)", *[f"{build[c]['mean']:.2f}" if c in build else "—" for c in cols]],
    ["size on disk (MB)", *[f"{int(size[c]) / 1e6:.1f}" if c in size else "—" for c in cols]],
])
out.append("## Answers")
out.append("")
out.append("Rows per engine, or the number of hits for a COUNT query. Hit sets compare `?s`, the")
out.append("matched literal and any joined variables, never the score. The top 10 of a common word")
out.append("are compared by row count only, because its hits tie on score. An answer marked")
out.append("\"differs\" disagrees with the majority, and that engine's time is not ranked.")
out.append("")
rows, bad = [], {}
for n in names:
    a = answers.get(n, {})
    key = lambda r: (r.get("rows"), r.get("value"))
    vals = [key(a[c]) for c in cols if c in a and a[c].get("rows") != "error"]
    ref = max(set(vals), key=vals.count) if vals and vals.count(max(set(vals), key=vals.count)) > 1 else None
    cells = []
    for c in cols:
        if c not in a:
            cells.append("—")
            continue
        r = a[c]
        differs = ref is not None and key(r) != ref
        if differs or r.get("rows") == "error":
            bad.setdefault(n, set()).add(c)
        cells.append(f"{hits.get(f'{n}:{c}', r.get('rows'))}" + (" (differs)" if differs else ""))
    rows.append([n, *cells])
table(["query", *cols], rows)
out.append("## Query times")
out.append("")
out.append("Mean ± standard deviation in ms of an HTTP request with curl (TSV results).")
out.append("")
rows = []
for n in names:
    rs = hf(f"text-{n}.json")
    ok = {c: r for c, r in rs.items() if c not in bad.get(n, set()) and all(e == 0 for e in r.get("exit_codes", [0]))}
    best = min(ok, key=lambda c: ok[c]["mean"]) if ok else None
    cells = []
    for c in cols:
        if c not in rs:
            cells.append("—")
        elif c not in ok:
            cells.append("error" if any(e != 0 for e in rs[c].get("exit_codes", [])) else "wrong answer")
        else:
            s = f"{ok[c]['mean'] * 1000:.1f} ± {(ok[c]['stddev'] or 0) * 1000:.1f}"
            cells.append(f"**{s}**" if c == best else s)
    rows.append([n, *cells])
table(["query", *cols], rows)
open(f"{d}/text-summary.md", "w").write("\n".join(out))
print("\n".join(out))
EOF
