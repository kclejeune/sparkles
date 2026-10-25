#!/usr/bin/env bash
set -euo pipefail
exec 8> "$HOME/lanes/.bench.lock"
flock -n 8 || exit 75
exec 9> "$HOME/lanes/.heavy.lock"
flock -n 9 || exit 75
lane="$HOME/lanes/benchmark-error-recovery-20261025"
export ROOT="$lane/src-reviewed" SPARKLES="$HOME/lanes/competitive-births-20261025/bin/sparkles-6b7f32ce" SPARKLES_COMMIT=6b7f32ce
export QSERVER=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-server
export JAVA=/nix/store/0fwxx5ym8irn3ik35ibpf2x7881p9q8v-openjdk-headless-17.0.20.1+1/bin/java
export SERVER_MEM_MAX=12G MAX_TIME=300 CLIENT_CPU=11 PORT_BASE=28520 ENGINES=qlever SUITE=bsbm-explore SCALE=10m
WORK="$lane/qlever-conservative-$(date -u +%Y%m%dT%H%M%SZ)"
export WORK
mkdir -p "$WORK/results"
echo "$WORK" > "$lane/active-qlever-conservative"
prior=$(cat "$lane/active-explore")
cp -a --reflink=auto "$prior/qlever/qlever-index" "$WORK/qlever-index"
cp -a "$prior/qlever/bsbm" "$WORK/bsbm"
ln -s "$prior/qlever/data.nt" "$WORK/data.nt"
: > "$WORK/qlever-index/server.log"
(date -u +%FT%TZ; hostname; uptime; free -m; df -h "$lane"; ps -eo pid,pcpu,rss,comm --sort=-pcpu | awk 'NR<=15') > "$WORK/preflight.txt"
sha256sum "$QSERVER" "$JAVA" "$WORK/bsbm/lib/bsbm.jar" "$lane/src/scripts/bench-bsbm/driver.py" "$lane/src/scripts/bench-bsbm/qmph.py" > "$WORK/inputs.sha256"
cd "$WORK"
source "$ROOT/scripts/bench-lib.sh"
source "$ROOT/scripts/bench-suite-lib.sh"
RES=results
DATA_SPECS=("bsbm-10m=$WORK/data.nt|GPL-3.0 generator|BSBMtools0.2 products28480 fixedseed")
suite_manifest
trap stop_all EXIT
(
 cd qlever-index
 exec "${CAP[@]}" "$QSERVER" -i bench -p "$((PORT_BASE+1))" -m 8G -c 0B -e 0B -E 0B -s 315s -a bench -j 1 >> server.log 2>&1
) &
PID[qlever]=$!
URL[qlever]="localhost:$((PORT_BASE+1))/"
ready=0
for _ in $(seq 1 300); do
 if curl -sf --max-time 2 -o /dev/null "${URL[qlever]}?cmd=stats"; then ready=1; break; fi
 kill -0 "${PID[qlever]}" || break
 sleep 0.1
done
[ "$ready" = 1 ] || exit 1
python3 -I "$lane/src/scripts/bench-bsbm/driver.py" --timeout 1800 --grace 5 --cwd "$WORK/bsbm" --xml "$WORK/results/driver-qlever-1-select.xml" --out "$WORK/results/qmph-select.json" --engine qlever --clients 1 --name explore-mix-select -- "$JAVA" -Dhttp.keepAlive=false -Xmx2g -cp '.:lib/*' benchmark.testdriver.TestDriver -runs 500 -w 50 -mt 1 -idir td_data -ucf usecases/explore-select/sparql.txt -t 300000 -o "$WORK/results/driver-qlever-1-select.xml" "http://${URL[qlever]}" > driver.log 2>&1
if ! answering qlever; then
 python3 -I "$lane/src/scripts/bench-bsbm/qmph.py" --unanswered results/qmph-select.json qlever 1 'the server stopped answering during the conservative driver run'
fi
stop_all
python3 - results/extra.json <<'PY'
import json,sys
json.dump({'settings':{'bsbmWarmup':50,'bsbmRuns':500,'bsbmClients':[1],'bsbmMaxTimeSeconds':1800,'requestTimeoutSeconds':300,'serverTimeoutSeconds':315,'qleverMemory':'8G','qleverCache':'0B','qleverLazyCache':'0B','qleverSimultaneousQueries':1,'serverMemoryCap':'12G'},'driver_contract':{'class':'benchmark.testdriver.TestDriver','usecase':'explore-select','jdk':'17.0.20.1+1','driver_heap':'2g','http_keepalive':False,'correctness':'Count screening required before ranking; conservative runtime settings are not the default-run contract.'}},open(sys.argv[1],'w'),indent=2)
PY
python3 "$ROOT/scripts/bench-record.py" finish "$RES/manifest.json"
python3 "$ROOT/scripts/bench-record.py" build --suite bsbm-explore --scale 10m --results "$RES" --manifest "$RES/manifest.json" --out "$RES/record.json" --max-time 300
date -u +%FT%TZ > complete.txt
