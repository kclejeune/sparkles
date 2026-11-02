#!/usr/bin/env bash
# ab3.sh <port> <rounds> <binA> <binB>: server-side exec_ms medians of star-join and predicate-counts on the loaded
# 1.05M store, A,B,B,A rounds, each binary in a fresh server
set -uo pipefail
cd ~/lanes/comp-452/syn1m
exec 9> ~/lanes/.heavy.lock; flock 9
cat /proc/loadavg
P='PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX ex: <http://example.org/> '
Q1="$P SELECT ?p ?n ?a ?o WHERE { ?p a ex:Researcher ; foaf:name ?n ; foaf:age ?a ; ex:worksFor ?o . ?o ex:city \"Kyoto\" }"
Q2='SELECT ?p (COUNT(*) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p'
port=$1 rounds=$2
for r in $(seq $rounds); do for b in $3 $4 $4 $3; do
  B=~/lanes/comp-452/bin/$b
  rm -rf srv-ab; taskset -c 0-11 $B --result-cache-mb 0 serve --data srv-ab --loc base=base.db --port $port > serve-ab.log 2>&1 &
  pid=$!
  for i in $(seq 100); do curl -sf localhost:$port/\$/ready >/dev/null && break; sleep 0.2; done
  for q in "$Q1" "$Q2"; do for i in $(seq 60); do taskset -c 12 curl -sf -o /dev/null --data-urlencode "query=$q" localhost:$port/base/sparql; done; done
  kill $pid; wait $pid
  sed "s/\x1b\[[0-9;]*m//g" serve-ab.log | grep 'operation="query"' | python3 -c '
import sys,re,statistics as st
rows=[dict(re.findall(r"(\w+)=\"?([0-9a-z.]+)", l)) for l in sys.stdin]
print(sys.argv[1], " ".join("%.4f" % st.median(float(x["exec_ms"]) for x in rows[k*60+10:(k+1)*60]) for k in (0,1)))' $b
done; done
echo AB3DONE
