#!/usr/bin/env bash
# prof.sh BIN TAG SCALE RUNS QUERIES... : cold --io profile under the heavy lock
set -euo pipefail
L=~/lanes/cold-lookups-4
bin=$1 tag=$2 scale=$3 runs=$4; shift 4
exec 9> "$HOME/lanes/.heavy.lock"
flock 9
mkdir -p $L/out/$tag
env ${XENV:-} python3 $L/bench-regressions.py --binary $L/sparkles-$bin --db $L/db$scale \
  --queries ~/.cache/sparkles-bench/$scale/queries --names "$@" --out $L/out/$tag/result.json \
  --runs $runs --cold --io --port 43800 --server-cpus 0,1,2,3,4,5,6,7,8,9,10,11 ${XARGS:-} > $L/out/$tag/stdout.log 2>&1
echo done >> $L/out/$tag/stdout.log
