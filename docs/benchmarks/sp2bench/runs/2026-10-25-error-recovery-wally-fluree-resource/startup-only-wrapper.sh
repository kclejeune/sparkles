#!/usr/bin/env bash
set -euo pipefail
exec 8> "$HOME/lanes/.bench.lock"
flock -n 8 || exit 75
exec 9> "$HOME/lanes/.heavy.lock"
flock -n 9 || exit 75
lane="$HOME/lanes/benchmark-error-recovery-20261025"
export ROOT="$lane/src-reviewed" SPARKLES="$HOME/lanes/competitive-births-release-20261025/target-D/release/sparkles" SPARKLES_COMMIT=6b7f32ce
export FLUREE="$lane/bin/fluree-4.2.2" FLUREE_VERSION=4.2.2
export SERVER_MEM_MAX=26G MAX_TIME=600 CLIENT_CPU=11 PORT_BASE=28510 ENGINES=fluree SUITE=sp2bench SCALE=10m ANSWERS_RAW_DIR=raw-answers
WORK="$lane/fluree-cache-$(date -u +%Y%m%dT%H%M%SZ)"
export WORK
mkdir -p "$WORK/results" "$WORK/queries"
echo "$WORK" > "$lane/active-fluree-cache"
prior=$(cat "$lane/active-sp2b")
ln -s "$prior/data/data.nt" "$WORK/data.nt"
cp -a --reflink=auto "$prior/fluree/fluree" "$WORK/fluree"
cp "$prior/data/queries/q4.rq" "$prior/data/queries/q8.rq" "$WORK/queries/"
(date -u +%FT%TZ; hostname; uptime; free -m; df -h "$lane"; ps -eo pid,pcpu,rss,comm --sort=-pcpu | awk 'NR<=15') > "$WORK/preflight.txt"
sha256sum "$FLUREE" "$ROOT/scripts/bench-answers.py" "$WORK/data.nt" > "$WORK/inputs.sha256"
sync
cd "$WORK"
source "$ROOT/scripts/bench-lib.sh"
source "$ROOT/scripts/bench-suite-lib.sh"
RES=results
DATA_SPECS=("sp2bench-10m=$WORK/data.nt|BSD|SP2Bench1.01 triples10000000")
suite_manifest
trap stop_all EXIT
mkdir -p raw-answers
FLUREE_CACHE_MAX_MB=512 FLUREE_PATH_MAX_VISITED=20000000 FLUREE_QUERY_TIMEOUT_MS=615000 \
 "${CAP[@]}" "$FLUREE" server run --listen-addr "127.0.0.1:$PORT_BASE" --storage-path "$WORK/fluree/.fluree/storage" --log-level warn > server.log 2>&1 &
PID[fluree]=$!
URL[fluree]="localhost:$PORT_BASE/v1/fluree/query/bench:main"
ready=0
for _ in $(seq 1 300); do
 if curl -sf --max-time 2 -o /dev/null "localhost:$PORT_BASE/health"; then ready=1; break; fi
 kill -0 "${PID[fluree]}" || break
 sleep 0.1
done
[ "$ready" = 1 ] || exit 1
curl --silent --show-error --max-time 60 -H 'Accept: text/tab-separated-values' --data-urlencode 'query@queries/q8.rq' "${URL[fluree]}" -D format-q8.headers -o format-q8.response
python3 "$ROOT/scripts/bench-answers.py" "${URL[fluree]}" queries/q4.rq results/answers.json q4 fluree --stream-tsv > query.log 2>&1
stop_all
python3 "$ROOT/scripts/bench-record.py" finish "$RES/manifest.json"
python3 - "$WORK" <<'PY'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1]);a=json.loads((p/'results/answers.json').read_text())['q4']['fluree'];expected='row-sha256-v1:94c3adc4557a846c5f12f6c39d3ecb0958b3d45b1dc014640f615e40019a374c'
r={'probe_version':1,'method':'separate complete-answer resource-feasibility probe, no warm latency estimate','server_memory_cap':'26G','cache_max_mb':512,'request_limit_seconds':600,'server_timeout_seconds':615,'rows':a.get('rows'),'value':a.get('value'),'error':a.get('error'),'source_oracle_agreement':a.get('rows')==41073618 and a.get('value')==expected};(p/'validation.json').write_text(json.dumps(r,indent=2)+'\n');print(json.dumps(r),flush=True)
PY
date -u +%FT%TZ > complete.txt
