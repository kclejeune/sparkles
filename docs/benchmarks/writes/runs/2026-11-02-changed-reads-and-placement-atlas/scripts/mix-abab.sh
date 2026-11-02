#!/usr/bin/env bash
# mix-abab.sh <bin> <args-a> <args-b>: bench.sh mixed mode at 1.05M for Sparkles alone on atlas, five A,B pairs with two serve flag sets
set -uo pipefail
L=~/lanes/comp-452
W=$L/mix1m
mkdir -p $W
[ -e $W/data.nt ] || ln $L/syn1m/data.nt $W/data.nt
[ -d $W/sparkles.db ] || cp -r $L/syn1m/base.db $W/sparkles.db
exec 9> ~/lanes/.heavy.lock; flock 9
cat /proc/loadavg
i=0
for a in "$2" "$3" "$2" "$3" "$2" "$3" "$2" "$3" "$2" "$3"; do
  i=$((i+1))
  ( cd $L/wt && MODE=mixed SPARKLES=$L/bin/$1 ENGINES=sparkles PORT_BASE=4620 SPARKLES_ARGS="$a" \
    bash -c 'eval "$(mise env -s bash 2>/dev/null)"; scripts/bench.sh 100000 '"$W" > $L/mix1-$i.log 2>&1 < /dev/null )
  echo "[$a] $(python3 -c 'import json,sys; d=json.load(open(sys.argv[1]))["sparkles"]; print(d)' $W/results/mixed.json)"
done
echo MIXDONE
