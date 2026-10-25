#!/usr/bin/env bash
set -euo pipefail
exec 8> "$HOME/lanes/.bench.lock"
flock -n 8 || exit 75
exec 9> "$HOME/lanes/.heavy.lock"
flock -n 9 || exit 75
lane="$HOME/lanes/benchmark-error-recovery-20261025"
export ROOT="$lane/src-reviewed" SPARKLES="$HOME/lanes/competitive-births-20261025/bin/sparkles-6b7f32ce"
export QSERVER=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-server
WORK="$lane/query-semantics-$(date -u +%Y%m%dT%H%M%SZ)"
export WORK ENGINES=qlever PORT_BASE=28490 SERVER_MEM_MAX=12G MAX_TIME=30 QLEVER_MEMORY=8G
export FUSEKI=/nix/store/q0dscszid7jzs66xq9dip95z2hc1zmsh-apache-jena-fuseki-5.1.0/bin/fuseki-server
export SPARKLES_COMMIT=6b7f32ce
mkdir -p "$WORK/queries" "$WORK/results"
echo "$WORK" > "$lane/active-query-semantics"
peer=$(cat "$lane/active-peers")
cp -a --reflink=auto "$peer/sp2b/qlever-index" "$WORK/qlever-index"
first=$(cat "$lane/active-first")
cold=$(cat "$lane/active-synthetic-cold")
cp -a --reflink=auto "$first/sp2b/sparkles.db" "$WORK/sparkles.db"
cp -a --reflink=auto "$cold/1.05m/jena.db" "$WORK/jena.db"
cat > "$WORK/queries/optional-values.rq" <<'Q'
SELECT ?author ?yr WHERE {
  VALUES (?author ?yr) { (<urn:author:a> 2000) (<urn:author:a> 2001) }
  OPTIONAL {
    VALUES (?author2 ?yr2) { (<urn:author:a> 2000) }
    FILTER (?author = ?author2 && ?yr2 < ?yr)
  }
  FILTER (!BOUND(?author2))
}
Q
cat > "$WORK/queries/string-less.rq" <<'Q'
SELECT ?first ?second WHERE {
  VALUES (?first ?second) { ("Abdullah Decker" "AbdulLateef Schriber") }
  FILTER (?first < ?second)
}
Q
cd "$WORK"
source "$ROOT/scripts/bench-lib.sh"
RES=results
trap stop_all EXIT
for engine in qlever sparkles jena; do
 ENGINES=$engine
 start_all
 for name in optional-values string-less; do
 curl --silent --show-error --max-time 30 -D "$engine-$name.headers" -H 'Accept: application/sparql-results+json' --data-urlencode "query@queries/$name.rq" "${URL[$engine]}" -o "$engine-$name.response.json" -w '%{http_code} %{time_total} %{size_download}\n' > "$engine-$name.metrics"
 done
 stop_all
done
sha256sum "$SPARKLES" "$FUSEKI" "$QSERVER" "$ROOT/scripts/bench-lib.sh" queries/*.rq > inputs.sha256
printf '%s\n' 'Dataset-independent VALUES queries only; source stores contain SP2Bench or synthetic RDF but these queries access no graph patterns; original published benchmark queries and evidence are unchanged.' > scope.txt
date -u +%FT%TZ > complete.txt
