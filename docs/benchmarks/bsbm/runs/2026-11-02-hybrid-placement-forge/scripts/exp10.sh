#!/usr/bin/env bash
# births-by-decade as an explicit cursor (execution=streaming) and eagerly, base against new, server time from the access log
set -euo pipefail
L=~/lanes/thread-placement-449
[ -n "${NOLOCK:-}" ] || { exec 9> ~/lanes/.heavy.lock; flock 9; }
echo "lock held $(date -u +%FT%TZ) load $(cat /proc/loadavg)"
if pgrep -f "sparkles serve|qlever-server" >/dev/null; then echo refusing; exit 1; fi
Q=$L/src-4bd70845/scripts/bench-billion/queries/births-by-decade.rq
OUT=$L/${EXP:-exp10}; mkdir -p $OUT
for round in 1 2 3; do
for bin in sparkles-4bd70845 ${NEW:-sparkles-wip1}; do
  for mode in streaming eager; do
    tag=r$round-$bin-$mode
    NO_COLOR=1 RUST_LOG=info $L/bin/$bin --result-cache-mb 0 serve --data $OUT/side-$tag --loc dbp=$L/billion/50m/sparkles.db --port 28479 --timeout 600 > $OUT/$tag.log 2>&1 &
    pid=$!
    until curl -sf -o /dev/null localhost:28479/\$/ready; do sleep 0.1; done
    for i in $(seq 25); do taskset -c 19 curl -sf -o /dev/null --data-urlencode "query@$Q" --data-urlencode nocache=true --data-urlencode execution=$mode -H 'Accept: application/sparql-results+json' localhost:28479/dbp/sparql; done
    kill $pid; wait $pid 2>/dev/null || true
    echo "$tag $(grep exec_ms $OUT/$tag.log | tail -20 | sed -E 's/.*exec_ms=([0-9.]+).*/\1/' | sort -n | sed -n 10,11p | tr '\n' ' ')"
  done
done
done
echo EXP10-DONE
