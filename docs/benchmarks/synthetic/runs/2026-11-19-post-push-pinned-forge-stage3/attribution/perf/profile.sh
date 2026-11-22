#!/usr/bin/env bash
# Flat user-space cycle profiles of the drift queries (streaming, one Rayon thread) for each build.
set -euo pipefail
L=.artifacts/lanes/forge-backlog
PERF=/nix/store/ciwqn0d1ybrvzw9sdlklfhg58njv3inc-perf-linux-7.3.3/bin/perf
port=${PORT:-41977}
for rev in ${REVS:-cc94ac7f 71c277b0}; do
  bin=$L/bin/sparkles-$rev-matrix; d=$L/perf/${OUTP:-}$rev; rm -rf $d; mkdir -p $d/srv
  $bin load --loc $d/store $L/insn/people-100000.nt.zst > $d/load.log 2>&1
  RAYON_NUM_THREADS=1 taskset -c 0 $bin --result-cache-mb 0 serve --data $d/srv --loc bench=$d/store --port $port --timeout 600 --no-access-log > $d/server.log 2>&1 &
  pid=$!
  for i in $(seq 100); do curl -sf -o /dev/null http://127.0.0.1:$port/ && break || sleep 0.2; done
  for q in ${QS:-optional-count optional-chain lang-filter regex-iri}; do
    n=$(case $q in optional-count) echo 4000;; optional-chain) echo 1500;; *) echo 1500;; esac)
    taskset -c 1 python3 $L/perf/drive.py $port $L/insn/queries/$q.rq 100
    $PERF record -q -e ${EVENT:-cycles:u} -c ${PERIOD:-200003} ${CALLG:-} -p $pid -o $d/$q.data -- taskset -c 1 python3 $L/perf/drive.py $port $L/insn/queries/$q.rq $n
    echo "$q $n" >> $d/counts.txt
  done
  kill $pid; wait $pid || true
done
echo PROFILE-DONE
