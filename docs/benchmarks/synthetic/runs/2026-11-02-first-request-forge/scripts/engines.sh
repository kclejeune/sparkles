#!/usr/bin/env bash
# engines.sh TAG ROUNDS "BIN..." [QUERIES]: bench.sh cold rows of Oxigraph and Sparkles binaries, in turn, under the bench lock
set -uo pipefail
L=~/lanes/cold-sparse-4
tag=$1 rounds=$2 bins=$3
Q=${4:-"path-plus star-lookup values-star employee-docs"}
if [ -z "${LOCKS_HELD:-}" ]; then exec 9> "$HOME/lanes/.bench.lock" 8> "$HOME/lanes/.heavy.lock"; flock 9; flock 8; fi
mkdir -p $L/out/$tag
for s in 10m 1m; do
  n=$([ $s = 10m ] && echo 1000000 || echo 100000)
  W=$L/bench$s
  for round in $(seq 1 $rounds); do
    for e in oxigraph $bins; do
      rm -rf $W/results
      if [ $e = oxigraph ]; then
        (cd $L/src && ENGINES=oxigraph MODE=cold COLD_RUNS=5 SKIP_LOAD=1 QUERIES="$Q" PORT_BASE=43820 scripts/bench.sh $n $W) > $L/out/$tag/$s-$e-$round.log 2>&1
      else
        (cd $L/src && ENGINES=sparkles MODE=cold COLD_RUNS=5 SKIP_LOAD=1 QUERIES="$Q" PORT_BASE=43820 SPARKLES=$L/bin/sparkles-$e SPARKLES_DB=$L/db$s scripts/bench.sh $n $W) > $L/out/$tag/$s-$e-$round.log 2>&1
      fi
      mkdir -p $L/out/$tag/$s-$e-$round && cp -a $W/results/. $L/out/$tag/$s-$e-$round/
    done
  done
done
python3 - $L/out/$tag <<'PY'
import json, os, statistics, sys, collections
d = sys.argv[1]; acc = collections.defaultdict(list)
for r in sorted(os.listdir(d)):
    p = os.path.join(d, r, 'cold-runs.json')
    if not os.path.exists(p): continue
    s, e, _ = r.split('-', 2)
    for eng, qs in json.load(open(p)).items():
        for q, v in qs.items():
            acc[(s, e, q)] += [t for t in v['times'] if isinstance(t, float)]
for k in sorted(acc):
    print(*k, f"{1000 * statistics.median(acc[k]):.2f} ms over {len(acc[k])}")
PY
echo ENGINESDONE
