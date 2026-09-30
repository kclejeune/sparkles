#!/usr/bin/env bash
# Write-time SHACL validation cost: the latency of a 1-triple INSERT DATA with validation
# off, `warn` and `reject`, over a generated dataset, with 0 and DELTA pending delta quads.
#
#   scripts/bench-shacl-write.sh [N_PEOPLE] [WORKDIR]
#
# The shapes are the SHACL benchmark's (crates/sparkles-shacl/examples/bench.rs), which the
# generated data does not conform to: `warn` commits and reports the results; `reject` uses
# the same shapes at severity sh:Warning, so the same validation runs and every write
# passes. The timed triple goes into the default graph (the data graph), so every timed
# write is validated; an untimed --prepare deletes it first, so every run inserts. The
# standalone full validation time is what `sparkles validation` reports when it turns
# validation on. The delta quads are foaf:name triples of untyped subjects: no new
# results, but every foaf:name scan merges them.
# Results go to WORKDIR/results/shacl-write.{json,md}.
# Env: WARMUP (default 3), RUNS (default 20), DELTA (default 10000), SPARKLES (binary,
# default target/release/sparkles), PORT (default 3937).
set -euo pipefail

N=${1:-10000}
WORK=${2:-/tmp/sparkles-bench-shacl-write}
WARMUP=${WARMUP:-3}
RUNS=${RUNS:-20}
DELTA=${DELTA:-10000}
PORT=${PORT:-3937}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
mkdir -p "$WORK/results"
WORK=$(cd "$WORK" && pwd)
cd "$WORK"

if ss -ltn | grep -q ":$PORT "; then
  echo "port $PORT is in use (set PORT)" >&2
  exit 1
fi
SPID=
# stop the server we started, and whatever still listens on its port
stop_server() {
  if [ -n "$SPID" ]; then
    kill "$SPID" 2> /dev/null || true
    wait "$SPID" 2> /dev/null || true
    SPID=
  fi
  pid=$(ss -ltnp 2> /dev/null | grep ":$PORT " | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | head -1 || true)
  [ -n "$pid" ] && { kill "$pid" 2> /dev/null || true; }
  true
}
trap stop_server EXIT
start_server() { # start_server <db>
  rm -rf server
  "$SPARKLES" --result-cache-mb 0 serve --data server --loc bench="$1" --port "$PORT" --no-access-log > server.log 2>&1 &
  SPID=$!
  for _ in $(seq 1 240); do
    curl -sf "localhost:$PORT/\$/ping" > /dev/null 2>&1 && return 0
    kill -0 "$SPID" 2> /dev/null || break
    sleep 0.5
  done
  echo "the server did not start:" >&2
  cat server.log >&2
  exit 1
}

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

DATA=data-$N.nt
if [ ! -f "$DATA" ]; then
  echo "generating dataset ($N people)…"
  python3 "$ROOT/scripts/gen-data.py" "$N" > "$DATA"
fi
echo "dataset: $(wc -l < "$DATA") triples"

# the SHACL benchmark's shapes (keep in sync with crates/sparkles-shacl/examples/bench.rs)
cat > shapes-warn.ttl << 'EOF'
@prefix ex: <http://example.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path foaf:name ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ;
  sh:property [ sh:path foaf:age ; sh:datatype xsd:integer ; sh:minInclusive 18 ; sh:maxInclusive 65 ] ;
  sh:property [ sh:path ex:salary ; sh:datatype xsd:decimal ; sh:minExclusive 0 ] ;
  sh:property [ sh:path foaf:knows ; sh:class ex:Person ; sh:nodeKind sh:IRI ] ;
  sh:property [ sh:path ex:authorOf ; sh:class ex:Document ; sh:maxCount 1 ] ;
  sh:property [ sh:path ( foaf:knows ex:worksFor ) ; sh:maxCount 4 ] .

ex:EmployeeShape a sh:NodeShape ; sh:targetClass ex:Employee ;
  sh:property [ sh:path ex:worksFor ; sh:minCount 1 ; sh:class ex:Organization ] .

ex:StudentShape a sh:NodeShape ; sh:targetClass ex:Student ;
  sh:property [ sh:path ex:advisor ; sh:minCount 1 ; sh:node ex:ResearcherShape ] .
ex:ResearcherShape a sh:NodeShape ; sh:class ex:Researcher .

ex:DocumentShape a sh:NodeShape ; sh:targetClass ex:Document ;
  sh:property [ sh:path ex:title ; sh:minCount 1 ; sh:uniqueLang true ; sh:pattern "^On the theory" ; sh:languageIn ( "en" ) ] ;
  sh:property [ sh:path ex:year ; sh:datatype xsd:integer ; sh:maxInclusive 2025 ] ;
  sh:property [ sh:path ex:cites ; sh:class ex:Document ] .

ex:OrgShape a sh:NodeShape ; sh:targetClass ex:Organization ;
  sh:closed true ; sh:ignoredProperties ( rdf:type ) ;
  sh:property [ sh:path foaf:name ; sh:minCount 1 ] ;
  sh:property [ sh:path ex:city ; sh:in ( "Kyoto" "Paris" "Berlin" "Boston" "Zurich" "Toronto" "Freiburg" "London" ) ] ;
  sh:property [ sh:path ex:founded ; sh:datatype xsd:date ] .
EOF
# the same shapes at severity sh:Warning: below the `reject` threshold (violation)
sed -e 's/a sh:NodeShape ;/a sh:NodeShape ; sh:severity sh:Warning ;/' \
  -e 's/\[ sh:path/[ sh:severity sh:Warning ; sh:path/' shapes-warn.ttl > shapes-reject.ttl

T='<http://example.org/bench/s> <http://example.org/bench/p> "v"'
printf 'INSERT DATA { %s }' "$T" > insert.ru
printf 'DELETE DATA { %s }' "$T" > delete.ru
upd() { echo "curl -sf -o /dev/null --data-urlencode update@$WORK/$1 localhost:$PORT/bench/update"; }

declare -A FULL_MS
for delta in 0 "$DELTA"; do
  db=$WORK/db-$delta
  rm -rf "$db"
  "$SPARKLES" load --loc "$db" "$DATA"
  if [ "$delta" -gt 0 ]; then
    {
      echo 'INSERT DATA {'
      seq "$delta" | awk '{ printf "<http://example.org/bench/d%d> <http://xmlns.com/foaf/0.1/name> \"d%d\" .\n", $1, $1 }'
      echo '}'
    } > delta.ru
    "$SPARKLES" update --loc "$db" --update delta.ru
  fi
  echo
  echo "== pending delta $delta: $("$SPARKLES" stats --loc "$db" | grep 'delta +/-' | tr -s ' ')"
  for mode in off warn reject; do
    if [ "$mode" = off ]; then
      "$SPARKLES" validation --loc "$db" --off > /dev/null
    else
      out=$("$SPARKLES" validation --loc "$db" --mode "$mode" --shapes "shapes-$mode.ttl")
      echo "$mode: $out"
      FULL_MS["$delta-$mode"]=$(echo "$out" | sed -n 's/.* in \([0-9]*\) ms$/\1/p')
    fi
    start_server "$db"
    eval "$(upd delete.ru)"
    # every timed write must be a validated insertion
    hdr=$(curl -sf -D - -o /dev/null --data-urlencode "update@insert.ru" "localhost:$PORT/bench/update" | tr -d '\r' | grep -i '^sparkles-validation:' || true)
    echo "$mode: ${hdr:-no Sparkles-Validation header}"
    case "$mode" in
      off) [ -z "$hdr" ] ;;
      warn) [[ "$hdr" == *"status=warned"* ]] ;;
      reject) [[ "$hdr" == *"status=passed"* ]] ;;
    esac || {
      echo "unexpected validation status for $mode" >&2
      exit 1
    }
    hyperfine --warmup "$WARMUP" --runs "$RUNS" --style basic \
      --prepare "$(upd delete.ru)" --command-name "$mode/delta=$delta" "$(upd insert.ru)" \
      --export-json results/shacl-write.new.json
    merge results/shacl-write.new.json results/shacl-write.json
    stop_server
  done
done

args=()
for k in "${!FULL_MS[@]}"; do args+=("$k=${FULL_MS[$k]}"); done
python3 - results/shacl-write.json results/shacl-write.md "$N" "$(wc -l < "$DATA")" "${args[@]}" << 'EOF'
import json, sys
src, dest, people, triples = sys.argv[1:5]
full = dict(kv.split("=", 1) for kv in sys.argv[5:])
res = {r["command"]: r for r in json.load(open(src))["results"]}
deltas = sorted({c.split("delta=")[1] for c in res}, key=int)
ms = lambda r: f'{r["mean"] * 1e3:.2f} ± {r["stddev"] * 1e3:.2f}' if r.get("stddev") is not None else f'{r["mean"] * 1e3:.2f}'
out = [f"1-triple INSERT DATA latency (ms, mean ± sd) over {triples} triples ({people} people)", "",
       "| pending delta | off | warn | reject | full validation (warn / reject) | reject ≤ full + 10 ms |",
       "|---:|---:|---:|---:|---:|:---:|"]
for d in deltas:
    cell = lambda m: ms(res[f"{m}/delta={d}"]) if f"{m}/delta={d}" in res else "–"
    fw, fr = full.get(f"{d}-warn", "?"), full.get(f"{d}-reject", "?")
    rj = res.get(f"reject/delta={d}")
    ok = "yes" if rj and fr.isdigit() and rj["mean"] * 1e3 <= int(fr) + 10 else "no"
    out.append(f"| {d} | {cell('off')} | {cell('warn')} | {cell('reject')} | {fw} / {fr} ms | {ok} |")
open(dest, "w").write("\n".join(out) + "\n")
print("\n".join(out))
EOF
