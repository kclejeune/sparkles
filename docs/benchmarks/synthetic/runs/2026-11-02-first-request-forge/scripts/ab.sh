#!/usr/bin/env bash
# ab.sh TAG SCALE RUNS PAIRS QUERIES...
#   interleaved A/B under the bench and heavy locks: PAIRS repetitions of A B B A
#   A and B are "bin|env|extra serve args|cold or warm" from $A and $B
set -euo pipefail
L=~/lanes/cold-sparse-4
tag=$1 scale=$2 runs=$3 pairs=$4; shift 4
QS=("$@")
if [ -z "${LOCKS_HELD:-}" ]; then exec 9> "$HOME/lanes/.bench.lock" 8> "$HOME/lanes/.heavy.lock"; flock 9; flock 8; fi
out=$L/out/$tag; mkdir -p $out
{ date -u +%FT%TZ; hostname; uname -r; cat /proc/loadavg; echo "A=$A"; echo "B=$B"; echo "queries=$*"; echo "scale=$scale runs=$runs pairs=$pairs";
  for s in A B; do v=${!s}; b=${v%%|*}; sha256sum $L/bin/sparkles-$b; readelf -p .comment $L/bin/sparkles-$b | grep -v '^$'; done; } > $out/env.txt
run() { # label round
  local v=${!1} b e x m
  b=${v%%|*}; e=$(echo "$v" | cut -d'|' -f2); x=$(echo "$v" | cut -d'|' -f3)
  m=$(echo "$v" | cut -d'|' -f4)
  local sargs=(); for w in $x; do sargs+=(--serve-arg=$w); done
  env $e python3 $L/src/scripts/bench-regressions.py --binary $L/bin/sparkles-$b --db $L/db$scale \
    --queries ~/.cache/sparkles-bench/$scale/queries --names "${QS[@]}" --out $out/$1-$2.json \
    --runs $runs $( [ "$m" = warm ] && echo "--warmup 3" || echo "--cold") --keep-going --port 43800 --server-cpus 0,1,2,3,4,5,6,7,8,9,10,11 "${sargs[@]}" >> $out/stdout.log 2>&1
}
i=0
for p in $(seq 1 $pairs); do
  for s in A B B A; do i=$((i+1)); run $s $i; done
done
python3 $L/summ.py $out > $out/summary.txt
echo ABDONE >> $out/stdout.log
