#!/usr/bin/env bash
# Write an on-disk format fixture with a given Sparkles binary: a server data directory
# and a backup repository, as that release writes them, for the format tests
# (crates/sparkles-core/tests/formats.rs and crates/sparkles-server/tests/cli_formats.rs).
#
#   scripts/format-fixture.sh BIN LABEL [OUT]
#
# BIN is the `sparkles` binary of the release (built with the `backup-encryption`
# feature for the encrypted backup). LABEL names the fixture, the release version or,
# before the first release, a commit. OUT defaults to testsuite/formats/LABEL.
#
# OUT gets fixture.tar.gz (the data directory `data/` with the catalog, and the backup
# repositories `repo/` and `repo-encrypted/` with its key files in `keys/`), the queries
# in queries/, the cases in cases.txt and each case's answer, as BIN printed it, in
# expected/. A case line is `NAME QUERY [FLAGS...]`: the answer of queries/QUERY.rq run
# with `sparkles query --loc data/databases/books --results json FLAGS...`.
#
# The dataset `books` has two generations (a bulk load, then a compaction), commits in
# the log of the second with new terms in its delta vocabulary, named graphs, blank
# nodes, RDF 1.2 triple terms, a named snapshot kept from the first generation, a branch
# with its own commit, a stored query, DESCRIBE and compaction settings, and the
# full-text, spatial and vector indexes. `facts.txt` records the commits the cases read.
set -euo pipefail

[ $# -ge 2 ] || {
  sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
  exit 1
}
BIN=$(realpath "$1")
LABEL=$2
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=$(realpath -m "${3:-$ROOT/testsuite/formats/$LABEL}")
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
D=$W/data
DB=$D/databases/books
mkdir -p "$OUT/queries" "$OUT/expected" "$W/keys"
s() { "$BIN" "$@"; }

s dataset create --data-dir "$D" books > /dev/null

cat > "$W/books.trig" << 'EOF'
PREFIX ex: <http://ex.org/>
PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX spk: <urn:x-sparkles:>
ex:alice ex:name "Alice" ; ex:age 42 ; ex:wrote ex:b1 .
ex:bob ex:name "Bob" ; ex:age 17 ; ex:knows [ ex:name "Carol" ] .
ex:b1 ex:title "Dragons of the north" ; ex:emb "[1, 0, 0]"^^spk:vector ;
  geo:hasGeometry [ geo:asWKT "POINT(10 50)"^^geo:wktLiteral ] .
ex:b2 ex:title "A quiet garden" ; ex:emb "[0, 1, 0]"^^spk:vector ;
  geo:hasGeometry [ geo:asWKT "POINT(-70 40)"^^geo:wktLiteral ] .
ex:g1 { ex:alice ex:likes ex:b2 . _:n ex:note "in a named graph" . }
ex:g2 { ex:bob ex:likes ex:b1 . }
EOF
s load --loc "$DB" "$W/books.trig" > /dev/null
s update --loc "$DB" --message 'first commit' 'PREFIX ex: <http://ex.org/>
  INSERT DATA { ex:dave ex:name "Dave" ; ex:age 30 }' > /dev/null
s update --loc "$DB" 'PREFIX ex: <http://ex.org/>
  PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
  INSERT DATA { ex:claim1 rdf:reifies <<( ex:alice ex:knows ex:bob )>> ; ex:source ex:dave }' > /dev/null
s snapshot create --loc "$DB" release-1 --note 'before the compaction' > /dev/null
s compact --loc "$DB" > /dev/null
s update --loc "$DB" --message 'after the compaction' 'PREFIX ex: <http://ex.org/>
  INSERT DATA { ex:erin ex:name "Erin" ; ex:age 51 . GRAPH ex:g1 { ex:erin ex:likes ex:b1 } }' > /dev/null
s update --loc "$DB" 'PREFIX ex: <http://ex.org/>
  DELETE DATA { ex:bob ex:age 17 } ; INSERT DATA { ex:bob ex:age 18 }' > /dev/null
s branch create --loc "$DB" dev --note 'a branch' > /dev/null
s update --loc "$DB" --branch dev 'PREFIX ex: <http://ex.org/>
  INSERT DATA { ex:frank ex:name "Frank on dev" }' > /dev/null

cat > "$W/adults.rq" << 'EOF'
PREFIX ex: <http://ex.org/>
SELECT ?name WHERE { ?p ex:age ?age ; ex:name ?name FILTER(?age >= ?minAge) } ORDER BY ?name
EOF
s queries put --loc "$DB" adults --query "$W/adults.rq" --param minAge:integer=18 \
  --description 'People at least minAge years old' > /dev/null
s describe-settings --loc "$DB" --set mode=scbd > /dev/null
s compaction --loc "$DB" --set deltaRatio=0.5 > /dev/null
s text-index --loc "$DB" --predicate http://ex.org/title > /dev/null 2>&1
s geo-index --loc "$DB" > /dev/null 2>&1
s vector create --loc "$DB" --name emb --predicate http://ex.org/emb --dim 3 > /dev/null

# the commit the history cases read: the head before the compaction (the snapshot's)
s log --loc "$DB" --limit 100 > "$W/log.txt"

# backups: a plaintext fs repository, and an encrypted one when BIN has the feature
s backup create --loc "$DB" --repo "file://$W/repo" --name b1 > /dev/null 2>&1
hexkey() {
  head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$1"
  chmod 600 "$1"
}
hexkey "$W/keys/online"
hexkey "$W/keys/recovery"
cfg=$W/backup.toml
if SPARKLES_BACKUP_CONFIG=$cfg s repo add secure --path "$W/repo-encrypted" --encrypt \
  --key-file "$W/keys/online" --key-file "$W/keys/recovery" > /dev/null 2>&1; then
  SPARKLES_BACKUP_CONFIG=$cfg s backup create --loc "$DB" --repo secure --name b1 > /dev/null 2>&1
else
  echo "format-fixture: $BIN has no backup encryption; no encrypted backup" >&2
  rm -rf "$W/repo-encrypted" "$W/keys"
fi

# queries and cases
q() { printf '%s\n' "$2" > "$OUT/queries/$1.rq"; }
P='PREFIX ex: <http://ex.org/>'
q count "SELECT (COUNT(*) AS ?n) WHERE { { ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } } }"
q names "$P SELECT ?name WHERE { ?p ex:name ?name } ORDER BY ?name"
q ages "$P SELECT ?p ?age WHERE { ?p ex:age ?age } ORDER BY ?p"
q graphs "SELECT ?g (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g ORDER BY ?g"
q bnodes "SELECT (COUNT(DISTINCT ?b) AS ?n) WHERE { { ?b ?p ?o } UNION { GRAPH ?g { ?b ?p ?o } } FILTER(isBlank(?b)) }"
q triple-terms "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
SELECT ?r ?s ?o WHERE { ?r rdf:reifies ?t BIND(SUBJECT(?t) AS ?s) BIND(OBJECT(?t) AS ?o) } ORDER BY ?r"
q text "PREFIX text: <http://jena.apache.org/text#>
SELECT ?s WHERE { ?s text:query 'dragons' } ORDER BY ?s"
q geo "PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX geof: <http://www.opengis.net/def/function/geosparql/>
$P SELECT ?b WHERE { ?b geo:hasGeometry/geo:asWKT ?w
  FILTER(geof:sfWithin(?w, 'POLYGON((0 40, 20 40, 20 60, 0 60, 0 40))'^^geo:wktLiteral)) } ORDER BY ?b"
q vector "PREFIX spk: <urn:x-sparkles:>
$P SELECT ?s WHERE { (?s ?score) spk:vectorSearch (ex:emb '[1, 0.1, 0]'^^spk:vector 1) }"
cat > "$OUT/cases.txt" << 'EOF'
count count
names names
ages ages
graphs graphs
bnodes bnodes
triple-terms triple-terms
text text
geo geo
vector vector
names-at-snapshot names --at snapshot:release-1
count-at-snapshot count --at snapshot:release-1
names-at-4 names --at commit:4
names-dev names --branch dev
EOF
while read -r name query flags; do
  # shellcheck disable=SC2086 # FLAGS is a list of flags
  s query --loc "$DB" --results json $flags "$(cat "$OUT/queries/$query.rq")" > "$OUT/expected/$name.json"
done < "$OUT/cases.txt"
s queries run --loc "$DB" adults --set minAge=40 > "$OUT/expected/stored-adults.txt"

{
  echo "label $LABEL"
  echo "written-by $("$BIN" --version)"
  echo "head $(s log --loc "$DB" --limit 1 | sed -n "s/.*  head \([0-9]*\).*/\1/p")"
} > "$OUT/facts.txt"
cp "$W/log.txt" "$OUT/log.txt"

find "$D" \( -name sparkles.lock -o -name catalog.lock \) -delete
parts=(data repo)
[ -d "$W/repo-encrypted" ] && parts+=(repo-encrypted keys)
tar -C "$W" -czf "$OUT/fixture.tar.gz" "${parts[@]}"
echo "wrote $OUT ($(du -h "$OUT/fixture.tar.gz" | cut -f1) compressed)"
