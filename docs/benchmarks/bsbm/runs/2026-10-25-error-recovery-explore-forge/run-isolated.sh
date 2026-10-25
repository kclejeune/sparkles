#!/usr/bin/env bash
set -euo pipefail
exec 8> "$HOME/lanes/.bench.lock"
flock -n 8 || exit 75
exec 9> "$HOME/lanes/.heavy.lock"
flock -n 9 || exit 75
lane="$HOME/lanes/benchmark-error-recovery-20261025"
export ROOT="$lane/src-reviewed" PATH="$HOME/lanes/competitive-losses-20261024/tools:$PATH"
export SPARKLES="$HOME/lanes/competitive-births-20261025/bin/sparkles-6b7f32ce" SPARKLES_COMMIT=6b7f32ce
export QSERVER=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-server QINDEX=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-index
export OXIGRAPH=/nix/store/g7ajfdk0g8bypdbsj23hnfq71izm3iz1-oxigraph-0.5.11/bin/oxigraph
export JAVA=/nix/store/0fwxx5ym8irn3ik35ibpf2x7881p9q8v-openjdk-headless-17.0.20.1+1/bin/java
export PORT_BASE=28500 SERVER_MEM_MAX=12G MAX_TIME=300 CLIENT_CPU=11
receipt="$lane/explore-select-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$receipt"
echo "$receipt" > "$lane/active-explore"
old="$HOME/lanes/item1-bench/runs/bsbm-10m-explore"
archive="$HOME/lanes/item1-bench/runs/bsbm-10m-bi/bsbmtools-v0.2.zip"
source_data="$HOME/lanes/competitive-losses-20261024/biq2-verify/run-20261025T041433Z/dataset.nt"
test "$(sha256sum "$archive" | cut -d ' ' -f 1)" = 40f5e59baadec3af0014b7647989d3e0fc0476af25e84a4bc9d7f8cd81520aaa
sha256sum "$source_data" "$archive" "$JAVA" "$QSERVER" "$QINDEX" "$OXIGRAPH" "$lane/src/scripts/bench-bsbm/driver.py" "$lane/src/scripts/bench-bsbm/qmph.py" > "$receipt/inputs.sha256"
(date -u +%FT%TZ; hostname; uptime; free -m; df -h "$lane"; ps -eo pid,pcpu,rss,comm --sort=-pcpu | awk 'NR<=15') > "$receipt/preflight.txt"
for engine in qlever oxigraph; do
 work="$receipt/$engine"
 mkdir -p "$work/results" "$work/logs" "$work/queries"
 ln -s "$source_data" "$work/data.nt"
 python3 - "$archive" "$work" "$old/logs/driver-$engine-16-select.log" <<'PY'
import pathlib,re,sys,urllib.parse,zipfile
archive,work,log=sys.argv[1:];w=pathlib.Path(work)
with zipfile.ZipFile(archive) as f:f.extractall(w)
(w/'bsbmtools-0.2').rename(w/'bsbm')
text=pathlib.Path(log).read_text();urls=re.findall(r'for URL: (http[^\s]+)',text)
query=urllib.parse.parse_qs(urllib.parse.urlsplit(urls[0]).query)['query'][0]
(w/'queries/retained-failure.rq').write_text(query)
PY
 cp -a "$old/bsbm/td_data" "$work/bsbm/td_data"
 cp -a "$old/bsbm/queries/explore-select" "$work/bsbm/queries/explore-select"
 cp -a "$old/bsbm/usecases/explore-select" "$work/bsbm/usecases/explore-select"
 (
  export WORK="$work" ENGINES="$engine" SUITE=bsbm-explore SCALE=10m
  cd "$WORK"
  source "$ROOT/scripts/bench-lib.sh"
  source "$ROOT/scripts/bench-suite-lib.sh"
  RES=results
  DATA_SPECS=("bsbm-10m=$WORK/data.nt|GPL-3.0 generator|BSBMtools0.2 products28480 fixedseed")
  suite_manifest
  load_all
  trap stop_all EXIT
  start_all
  for media in json xml; do
   if [ "$media" = json ]; then accept=application/sparql-results+json; else accept=application/sparql-results+xml; fi
   curl --silent --show-error --max-time 300 -G -H "Accept: $accept" --data-urlencode 'query@queries/retained-failure.rq' "http://${URL[$engine]}" -D "$media.headers" -o "$media.response" -w '%{http_code} %{time_total} %{size_download}\n' > "$media.metrics" || true
  done
  python3 -I "$lane/src/scripts/bench-bsbm/driver.py" --timeout 1800 --grace 5 --cwd "$WORK/bsbm" --xml "$WORK/results/driver-$engine-1-select.xml" --out "$WORK/results/qmph-select.json" --engine "$engine" --clients 1 --name explore-mix-select -- "$JAVA" -Dhttp.keepAlive=false -Xmx2g -cp '.:lib/*' benchmark.testdriver.TestDriver -runs 500 -w 50 -mt 1 -idir td_data -ucf usecases/explore-select/sparql.txt -t 300000 -o "$WORK/results/driver-$engine-1-select.xml" "http://${URL[$engine]}" > "$WORK/driver.log" 2>&1
  stop_all
  python3 - results/extra.json <<'META'
import json,sys
json.dump({'settings':{'bsbmWarmup':50,'bsbmRuns':500,'bsbmClients':[1],'bsbmMaxTimeSeconds':1800,'requestTimeoutSeconds':300,'serverTimeoutSeconds':315,'qleverMemory':'8G','serverMemoryCap':'12G'},'driver_contract':{'class':'benchmark.testdriver.TestDriver','usecase':'explore-select','jdk':'17.0.20.1+1','driver_heap':'2g','http_keepalive':False,'correctness':'Full driver average counts require comparison before ranking; direct failed-query probes are preserved separately.'}},open(sys.argv[1],'w'),indent=2)
META
  python3 "$ROOT/scripts/bench-record.py" finish "$RES/manifest.json"
  python3 "$ROOT/scripts/bench-record.py" build --suite bsbm-explore --scale 10m --results "$RES" --manifest "$RES/manifest.json" --out "$RES/record.json" --max-time 300
 ) > "$receipt/$engine.log" 2>&1
 date -u +%FT%TZ > "$receipt/$engine-complete.txt"
done
date -u +%FT%TZ > "$receipt/complete.txt"
