#!/usr/bin/env bash
# Two more rounds of the whole suite, so each build has five processes as the gate does.
set -uo pipefail
cd /root/b
export HOME=/root RAYON_NUM_THREADS=6 INCR_BENCH_PEOPLE=200000
LD=/lib64/ld-linux-x86-64.so.2
run() {
  local v=$1 tag=$2; shift 2
  echo "$tag $v start $(date -u +%T) load $(cut -d' ' -f1-3 /proc/loadavg)"
  INCR_BENCH_JSON=/root/b/out/$v-$tag.jsonl timeout 3000 taskset -c 2-7 $LD ./incr_bench-$v --ignored --nocapture --test-threads 1 "$@" > /root/b/out/$v-$tag.md 2> /root/b/out/$v-$tag.err
  echo "$tag $v exit $? $(date -u +%T)"
}
run cand full4; run base full4; run base full5; run cand full5
echo ALLDONE
