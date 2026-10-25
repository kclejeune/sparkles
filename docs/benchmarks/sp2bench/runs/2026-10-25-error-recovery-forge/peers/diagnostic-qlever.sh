#!/usr/bin/env bash
set -euo pipefail
exec 8> "$HOME/lanes/.bench.lock"
flock -n 8 || exit 75
exec 9> "$HOME/lanes/.heavy.lock"
flock -n 9 || exit 75
lane="$HOME/lanes/benchmark-error-recovery-20261025"
export ROOT="$lane/src-reviewed" SPARKLES="$HOME/lanes/competitive-births-20261025/bin/sparkles-6b7f32ce"
export QSERVER=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-server
WORK="$lane/qlever-diagnostic-$(date -u +%Y%m%dT%H%M%SZ)"
export WORK ENGINES=qlever PORT_BASE=28490 SERVER_MEM_MAX=12G MAX_TIME=30 QLEVER_MEMORY=8G
mkdir -p "$WORK/queries" "$WORK/results"
echo "$WORK" > "$lane/active-qlever-diagnostic"
peer=$(cat "$lane/active-peers")
cp -a --reflink=auto "$peer/sp2b/qlever-index" "$WORK/qlever-index"
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
start_all
for name in optional-values string-less; do
 curl --silent --show-error --max-time 30 -D "$name.headers" -H 'Accept: application/sparql-results+json' --data-urlencode "query@queries/$name.rq" "${URL[qlever]}" -o "$name.response.json" -w '%{http_code} %{time_total} %{size_download}\n' > "$name.metrics"
done
curl --silent --show-error --max-time 10 "${URL[qlever]}?cmd=stats" > stats.json
stop_all
sha256sum "$QSERVER" "$ROOT/scripts/bench-lib.sh" queries/*.rq > inputs.sha256
printf '%s\n' 'Diagnostic queries only; original published benchmark queries and evidence are unchanged.' > scope.txt
date -u +%FT%TZ > complete.txt
