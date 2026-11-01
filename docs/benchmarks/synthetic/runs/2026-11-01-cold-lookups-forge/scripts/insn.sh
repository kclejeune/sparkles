#!/usr/bin/env bash
# insn.sh TAG ROUNDS BIN... : warm user-mode instructions per request of the flagged queries, under the heavy lock
set -uo pipefail
L=~/lanes/cold-lookups-4
tag=$1 rounds=$2; shift 2
exec 9> "$HOME/lanes/.heavy.lock"; flock 9
out=$L/out/$tag; mkdir -p $out
PERF=$HOME/lanes/cold-lookups-4/perf-root/bin/perf
Q=~/.cache/sparkles-bench/10m/queries
N=20
for round in $(seq 1 $rounds); do
for bin in "$@"; do
  rm -rf $out/server; mkdir -p $out/server
  taskset -c 0-11 $L/sparkles-$bin --result-cache-mb 0 serve --data $out/server --loc bench=$L/db10m --port 43810 --timeout 600 --read-only > $out/server.log 2>&1 &
  pid=$!
  until curl -sf http://127.0.0.1:43810/\$/ready >/dev/null; do sleep 0.1; done
  for q in group-avg expr-agg-arg star-lookup star-join employee-docs; do
    for i in 1 2 3; do curl -s -o /dev/null -H 'Content-Type: application/sparql-query' -H 'Accept: application/sparql-results+json' --data-binary @$Q/$q.rq http://127.0.0.1:43810/bench/sparql; done
    $PERF stat -x, -e instructions:u -p $pid -o $out/perf-$bin-$q-$round.csv -- bash -c "for i in \$(seq 1 $N); do curl -s -o /dev/null -H 'Content-Type: application/sparql-query' -H 'Accept: application/sparql-results+json' --data-binary @$Q/$q.rq http://127.0.0.1:43810/bench/sparql; done"
    n=$(grep cpu_core $out/perf-$bin-$q-$round.csv | cut -d, -f1)
    echo "$round $bin $q $((n / N))" | tee -a $out/insn.txt
  done
  kill $pid; wait $pid 2>/dev/null
done
done
echo done >> $out/insn.txt
