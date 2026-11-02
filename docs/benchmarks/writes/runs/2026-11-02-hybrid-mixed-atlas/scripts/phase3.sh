#!/usr/bin/env bash
# phase3.sh: item 583 acceptance of the fix4 build on atlas
set -uo pipefail
L=~/lanes/hybrid-583
W=$L/mix
mkdir -p $L/out/accept4
BIN=fix6 ./affsnap.sh 3 > $L/affsnap4.log 2>&1 &
snap=$!
i=0
for a in "" "--request-cpus all" "--request-cpus all" "" "" "--request-cpus all" "--request-cpus all" "" "" "--request-cpus all"; do
  i=$((i+1))
  ( cd $L/wt && MODE=mixed SPARKLES=$L/bin/fix6 ENGINES=sparkles PORT_BASE=4720 SPARKLES_ARGS="$a" \
    bash -c 'eval "$(mise env -s bash 2>/dev/null)"; scripts/bench.sh 100000 '"$W" > $L/out/accept4/run-$i.log 2>&1 < /dev/null )
  cp $W/results/mixed.json $L/out/accept4/mixed-$i.json
  echo "load $(cut -d" " -f1-3 /proc/loadavg) others $(pgrep -fc "rustc|nextest|cargo" )"
  echo "accept $i [${a:-default}] $(python3 -c 'import json,sys; d=json.load(open(sys.argv[1]))["sparkles"]; print(d)' $W/results/mixed.json)"
done
kill $snap 2>/dev/null; wait $snap 2>/dev/null
cd $L
B="--bin fix6=$L/bin/fix6"
CONC=16 python3 mixab.py out/p3-c16 3 auto all $B
CONC=4 python3 mixab.py out/p3-c4 3 auto all $B
python3 placement583.py out/quick3 4 auto reqall --bin fix6=$L/bin/fix6 --bin p449=$L/bin/exp1
python3 summarize.py out/quick3/records.json server | head -12
echo PHASE3DONE
