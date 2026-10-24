#!/usr/bin/env bash
# Compares the stemmed full-text searches of Sparkles with jena-text's in Fuseki on a
# corpus of 2,700 literals in nine languages, with 1,590 queries.
#
#   scripts/text-lucene-compare/compare.sh [WORKDIR]
#
# gen.py makes the corpus and the queries from words.json. Fuseki 5.1.0 (jena-text with
# Lucene 9.11.1) indexes the literals with text:multilingualSupport, which analyzes each
# literal with its language's Lucene analyzer. Sparkles loads the same literals twice:
# dataset "lucene" selects the analyzers that follow Lucene's (english-lucene,
# french-light, spanish-light, italian-light, portuguese-light, german-light,
# dutch-lucene, russian-lucene and swedish-lucene), and dataset "snowball" keeps the
# default Snowball analyzers. The report, WORKDIR/report.md, gives the agreement of both
# and lists the queries on which "lucene" differs from Fuseki.
#
# Fuseki comes from this flake's pinned package, and FUSEKI overrides it. SPARKLES names
# the sparkles binary (default target/debug/sparkles). PORT_BASE (default 47600) gives the
# ports: Fuseki listens on PORT_BASE+1 and Sparkles on PORT_BASE+2.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${1:-$(mktemp -d)}
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
SPARKLES=${SPARKLES:-$ROOT/target/debug/sparkles}
PORT_BASE=${PORT_BASE:-47600}
FPORT=$((PORT_BASE + 1))
SPORT=$((PORT_BASE + 2))
# shellcheck source=scripts/bench-tools.sh
source "$ROOT/scripts/bench-tools.sh"
has() { [ "$1" = jena ]; }
resolve_tools

PIDS=()
cleanup() {
  for p in "${PIDS[@]}"; do kill "$p" 2> /dev/null || true; done
  wait 2> /dev/null || true
}
trap cleanup EXIT
wait_for() {
  for _ in $(seq 1 240); do
    curl -sf "$1" > /dev/null 2>&1 && return 0
    sleep 0.5
  done
  echo "timeout waiting for $1" >&2
  exit 1
}

python3 "$ROOT/scripts/text-lucene-compare/gen.py" "$ROOT/scripts/text-lucene-compare/words.json" "$WORK"

cat > "$WORK/fuseki.ttl" << 'TTL'
@prefix fuseki: <http://jena.apache.org/fuseki#> .
@prefix ja:     <http://jena.hpl.hp.com/2005/11/Assembler#> .
@prefix rdf:    <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs:   <http://www.w3.org/2000/01/rdf-schema#> .
@prefix text:   <http://jena.apache.org/text#> .

<#service> rdf:type fuseki:Service ;
    fuseki:name "cmp" ;
    fuseki:endpoint [ fuseki:operation fuseki:query ; fuseki:name "sparql" ] ;
    fuseki:endpoint [ fuseki:operation fuseki:gsp-rw ; fuseki:name "data" ] ;
    fuseki:dataset <#text_dataset> .

<#text_dataset> rdf:type text:TextDataset ;
    text:dataset <#mem> ;
    text:index <#lucene> .

<#mem> rdf:type ja:MemoryDataset .

<#lucene> rdf:type text:TextIndexLucene ;
    text:directory "mem" ;
    text:storeValues true ;
    text:multilingualSupport true ;
    text:entityMap <#entMap> .

<#entMap> rdf:type text:EntityMap ;
    text:entityField "uri" ;
    text:uidField "uid" ;
    text:defaultField "label" ;
    text:langField "lang" ;
    text:map ( [ text:field "label" ; text:predicate rdfs:label ] ) .
TTL

LUCENE=(en=english-lucene fr=french-light es=spanish-light it=italian-light
  pt=portuguese-light de=german-light nl=dutch-lucene ru=russian-lucene sv=swedish-lucene)
for ds in lucene snowball; do
  rm -rf "$WORK/$ds.db"
  "$SPARKLES" load --loc "$WORK/$ds.db" "$WORK/corpus.nt" > "$WORK/$ds-load.log" 2>&1
  args=()
  if [ "$ds" = lucene ]; then
    for l in "${LUCENE[@]}"; do args+=(--language "$l"); done
  else
    for l in en fr es it pt de nl ru sv; do args+=(--language "$l"); done
  fi
  "$SPARKLES" text-index --loc "$WORK/$ds.db" \
    --predicate http://www.w3.org/2000/01/rdf-schema#label "${args[@]}" > "$WORK/$ds-text.log" 2>&1
done

env -u JAVA_HOME JVM_ARGS="-Xmx2G" "$FUSEKI" --config="$WORK/fuseki.ttl" --port "$FPORT" > "$WORK/fuseki.log" 2>&1 &
PIDS+=($!)
"$SPARKLES" --result-cache-mb 0 serve --loc lucene="$WORK/lucene.db" --loc snowball="$WORK/snowball.db" \
  --port "$SPORT" > "$WORK/sparkles.log" 2>&1 &
PIDS+=($!)
wait_for "localhost:$FPORT/\$/ping"
wait_for "localhost:$SPORT/\$/ping"
curl -sf -X POST -H 'Content-Type: application/n-triples' --data-binary "@$WORK/corpus.nt" \
  "localhost:$FPORT/cmp/data?default" > /dev/null

python3 "$ROOT/scripts/text-lucene-compare/compare.py" "$WORK/queries.tsv" \
  "http://localhost:$FPORT/cmp/sparql" \
  lucene="http://localhost:$SPORT/lucene/sparql" snowball="http://localhost:$SPORT/snowball/sparql" \
  > "$WORK/report.md"
sed -n '1,/^## /p' "$WORK/report.md"
echo "report: $WORK/report.md"
