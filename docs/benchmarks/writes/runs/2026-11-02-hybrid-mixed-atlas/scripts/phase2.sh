#!/usr/bin/env bash
# phase2.sh: item 583 acceptance on atlas. Five alternating pairs of scripts/bench.sh MODE=mixed at 1.05M with the
# fix build's default and --request-cpus all, then the mixab concurrency profile and the single-client BSBM BI check.
set -uo pipefail
L=~/lanes/hybrid-583
W=$L/mix
mkdir -p $L/out/accept
i=0
for a in "" "--request-cpus all" "--request-cpus all" "" "" "--request-cpus all" "--request-cpus all" "" "" "--request-cpus all"; do
  i=$((i+1))
  ( cd $L/wt && MODE=mixed SPARKLES=$L/bin/fix2 ENGINES=sparkles PORT_BASE=4720 SPARKLES_ARGS="$a" \
    bash -c 'eval "$(mise env -s bash 2>/dev/null)"; scripts/bench.sh 100000 '"$W" > $L/out/accept/run-$i.log 2>&1 < /dev/null )
  cp $W/results/mixed.json $L/out/accept/mixed-$i.json
  echo "accept $i [${a:-default}] $(python3 -c 'import json,sys; d=json.load(open(sys.argv[1]))["sparkles"]; print(d)' $W/results/mixed.json)"
done
cd $L
B="--bin fix2=$L/bin/fix2 --bin p449=$L/bin/exp1"
CONC=16 python3 mixab.py out/p2-c16 3 auto all $B
for c in 2 4 8; do CONC=$c python3 mixab.py out/p2-c$c 3 auto all $B; done
python3 placement583.py out/quick2 4 auto reqall --bin fix2=$L/bin/fix2 --bin p449=$L/bin/exp1
python3 summarize.py out/quick2/records.json server | head -12
echo PHASE2DONE
