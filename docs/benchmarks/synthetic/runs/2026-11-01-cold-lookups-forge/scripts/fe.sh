#!/usr/bin/env bash
# fe.sh TAG ROUNDS BIN... : warm front-end counters per request of group-avg and expr-agg-arg, under the heavy lock
set -uo pipefail
L=~/lanes/cold-lookups-4
tag=$1 rounds=$2; shift 2
exec 9> "$HOME/lanes/.heavy.lock"; flock 9
out=$L/out/$tag; mkdir -p $out
PERF=$HOME/lanes/cold-lookups-4/perf-root/bin/perf
EV=cpu_core/cycles/u,cpu_core/instructions/u,cpu_core/frontend_retired.any_dsb_miss/u,cpu_core/frontend_retired.l1i_miss/u,cpu_core/frontend_retired.itlb_miss/u,cpu_core/dsb2mite_switches.penalty_cycles/u,cpu_core/br_misp_retired.all_branches/u,cpu_core/topdown-fe-bound/u,cpu_core/slots/u
Q=~/.cache/sparkles-bench/10m/queries
N=20
req() { curl -s -o /dev/null -H 'Content-Type: application/sparql-query' -H 'Accept: application/sparql-results+json' --data-binary @$Q/$1.rq http://127.0.0.1:43810/bench/sparql; }
for round in $(seq 1 $rounds); do
for bin in "$@"; do
  rm -rf $out/server; mkdir -p $out/server
  taskset -c 0-11 $L/sparkles-$bin --result-cache-mb 0 serve --data $out/server --loc bench=$L/db10m --port 43810 --timeout 600 --read-only > $out/server.log 2>&1 &
  pid=$!
  until curl -sf http://127.0.0.1:43810/\$/ready >/dev/null; do sleep 0.1; done
  for q in group-avg expr-agg-arg; do
    for i in 1 2 3; do req $q; done
    $PERF stat -x, -e $EV -p $pid -o $out/fe-$bin-$q-$round.csv -- bash -c "$(declare -f req); Q=$Q; for i in \$(seq 1 $N); do req $q; done"
    t0=$(date +%s%N); for i in $(seq 1 $N); do req $q; done; t1=$(date +%s%N)
    echo "$round $bin $q ms_per_req=$(( (t1 - t0) / N / 1000 ))us" >> $out/fe.txt
  done
  kill $pid; wait $pid 2>/dev/null
done
done
echo done >> $out/fe.txt
