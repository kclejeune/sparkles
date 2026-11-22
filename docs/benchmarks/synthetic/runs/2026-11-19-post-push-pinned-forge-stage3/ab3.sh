#!/usr/bin/env bash
# ab3.sh NAME A B ROUNDS REPS: interleaved A,B,B,A rounds on forge, locks held by the caller.
# Like ab.sh, with RAYON_THREADS, SERVER_CPU, CLIENT_CPU, SPIN, WARMUP and EXECUTION ("" for
# automatic, or "MODE" / "A_MODE,B_MODE") taken from the environment, and an order digest.
# DATA names another .nt.zst, such as $W/data/people-1000000.nt.zst for 10.5M triples.
set -euo pipefail
W=${W:-$HOME/lanes/forge-backlog}
N=$1 A=$2 B=$3 ROUNDS=$4 REPS=$5
: "${QS:?QS names the queries}"
d=$W/runs/$N
rm -rf "$d"; mkdir -p "$d/queries"
P=$(sed -n "s/^P='\(.*\)'$/\1/p" "$W/bench.sh")
for q in $QS; do
  body=$(sed -n "s/^add $q '\(.*\)'$/\1/p" "$W/bench.sh")
  [ -n "$body" ] || { echo "no query $q"; exit 1; }
  printf '%s%s' "$P" "$body" > "$d/queries/$q.rq"
done
ln -s $W/bin/$A.zst $d/bin-A.zst
ln -s $W/bin/$B.zst $d/bin-B.zst
ln -s ${DATA:-$W/data.nt.zst} $d/data.nt.zst
cp $W/nsc-bench-remote.sh $d/
cat > $d/run.env <<ENV
DYNAMIC_A=0
DYNAMIC_B=0
ROUNDS=$ROUNDS
REPS=$REPS
WARMUP=${WARMUP:-5}
RAYON_THREADS=${RAYON_THREADS:-1}
SERVER_CPU=${SERVER_CPU:-2}
CLIENT_CPU=${CLIENT_CPU:-4}
SPIN=${SPIN:-1}
PORT=41913
EXECUTION=${EXECUTION:-}
ORDER_DIGEST="python3 $W/src-8baa5686/scripts/bench-gate.py order-digest --data data.nt"
ENV
echo "$A $B" > $d/revs.txt
uptime > $d/uptime-before.txt
bash $d/nsc-bench-remote.sh > /dev/null
uptime > $d/uptime-after.txt
rm -rf $d/stores $d/A $d/B $d/data.nt
