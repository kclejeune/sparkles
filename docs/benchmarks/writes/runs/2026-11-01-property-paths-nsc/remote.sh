#!/usr/bin/env bash
# Interleaved A/B of the incremental benchmark on one Namespace instance (item 417).
# base = 0dc27215 with the path benchmark added, cand = the lane's operator, dred = the
# overdeletion experiment. Three rounds run the whole suite of base and cand in rotated
# order, then three rounds alternate the path test of dred and cand.
set -uo pipefail
cd ~/b
mkdir -p out
export RAYON_NUM_THREADS=6 INCR_BENCH_PEOPLE=200000
LD=/lib64/ld-linux-x86-64.so.2
run() { # name tag args...
  local v=$1 tag=$2; shift 2
  echo "$tag $v start $(date -u +%T) load $(cut -d' ' -f1-3 /proc/loadavg)"
  INCR_BENCH_JSON=~/b/out/$v-$tag.jsonl timeout 3000 taskset -c 2-7 $LD ./incr_bench-$v --ignored --nocapture --test-threads 1 "$@" > ~/b/out/$v-$tag.md 2> ~/b/out/$v-$tag.err
  echo "$tag $v exit $? $(date -u +%T)"
}
for r in 1 2 3; do
  if [ $((r % 2)) = 1 ]; then order="base cand"; else order="cand base"; fi
  for v in $order; do run $v full$r; done
done
while [ ! -x ./incr_bench-dred ]; do sleep 10; done
for r in 1 2 3; do
  if [ $((r % 2)) = 1 ]; then order="dred cand"; else order="cand dred"; fi
  for v in $order; do run $v path$r --exact path_pass_against_recompute; done
done
echo ALLDONE
