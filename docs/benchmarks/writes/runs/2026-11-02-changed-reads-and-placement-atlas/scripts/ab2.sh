#!/usr/bin/env bash
# ab2.sh <port> <bin...>: server-side exec_ms medians of two-hop-count on the loaded and changed 1.05M stores
set -uo pipefail
cd ~/lanes/comp-452/syn1m
exec 9> ~/lanes/.heavy.lock; flock 9
cat /proc/loadavg
Q='PREFIX foaf: <http://xmlns.com/foaf/0.1/> SELECT (COUNT(*) AS ?n) WHERE { ?a foaf:knows ?b . ?b foaf:knows ?c }'
port=$1; shift
for b in "$@"; do
  B=~/lanes/comp-452/bin/$b
  rm -rf srv-ab; $B --result-cache-mb 0 serve --data srv-ab --loc base=base.db --loc changed=changed.db --port $port > serve-ab.log 2>&1 &
  pid=$!
  for i in $(seq 100); do curl -sf localhost:$port/\$/ready >/dev/null && break; sleep 0.2; done
  for ds in base changed; do
    for i in $(seq 60); do curl -sf -o /dev/null --data-urlencode "query=$Q" localhost:$port/$ds/sparql; done
  done
  kill $pid; wait $pid
  echo "== $b"
  sed "s/\x1b\[[0-9;]*m//g" serve-ab.log | grep 'operation="query"' | python3 -c '
import sys,re,statistics as st
rows=[dict(re.findall(r"(\w+)=\"?([0-9a-z.]+)", l)) for l in sys.stdin]
for ds in ("base","changed"):
  r=[x for x in rows if x.get("dataset")==ds][10:]
  print(ds, {k: round(st.median(float(x[k]) for x in r),3) for k in ("plan_ms","exec_ms","total_ms")})'
done
echo AB2DONE
