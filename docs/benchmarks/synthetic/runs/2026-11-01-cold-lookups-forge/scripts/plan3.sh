#!/usr/bin/env bash
# bench.sh cold rows of Sparkles (base and final) and Oxigraph on the same host, under the heavy lock
set -uo pipefail
L=~/lanes/cold-lookups-4
Q4="group-avg expr-agg-arg values-star path-plus"
$L/prof.sh base prof-base-agg 10m 3 $Q4
$L/prof.sh fin prof-fin-agg 10m 3 $Q4
(cd $L && A="exp|XNOLOOK=1|||" B="exp||||" ./ab.sh look-agg 10m 7 2 $Q4)
(cd $L && A="base||||" B="fin||||" ./ab.sh flag10 10m 11 3 $Q4 expr-bind-group predicate-counts)
(cd $L && A="base||||" B="fin||||" ./ab.sh flag1 1m 11 3 optional-count lang-filter expr-bind-group regex-iri knows-reach star-lookup types-grouped minus not-exists subquery-agg distinct-join)
exec 9> "$HOME/lanes/.heavy.lock"; flock 9
Q="star-lookup values-star employee-docs path-plus star-join"
for s in 10m 1m; do
  n=$([ $s = 10m ] && echo 1000000 || echo 100000)
  W=$L/bench$s
  for round in 1 2; do
    for e in oxigraph fin base; do
      rm -rf $W/results
      if [ $e = oxigraph ]; then
        (cd $L/fin && ENGINES=oxigraph MODE=cold COLD_RUNS=5 SKIP_LOAD=1 QUERIES="$Q" PORT_BASE=43820 scripts/bench.sh $n $W) > $L/out/oxi-$s-$e-$round.log 2>&1
      else
        (cd $L/fin && ENGINES=sparkles MODE=cold COLD_RUNS=5 SKIP_LOAD=1 QUERIES="$Q" PORT_BASE=43820 SPARKLES=$L/sparkles-$e SPARKLES_DB=$L/db$s scripts/bench.sh $n $W) > $L/out/oxi-$s-$e-$round.log 2>&1
      fi
      mkdir -p $L/out/oxi-$s-$e-$round && cp -a $W/results/. $L/out/oxi-$s-$e-$round/
    done
  done
done
echo done > $L/plan3.done
