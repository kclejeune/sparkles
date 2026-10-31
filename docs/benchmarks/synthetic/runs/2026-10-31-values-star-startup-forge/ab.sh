#!/usr/bin/env bash
# ab.sh NAME A B ROUNDS REPS: interleaved A,B,B,A rounds on forge, locks held by the caller's flock
set -euo pipefail
W=$HOME/lanes/forge-checks-440-432-20261031
N=$1 A=$2 B=$3 ROUNDS=$4 REPS=$5
QS="values-star count-all star-lookup employee-docs minus path-plus star-join"
d=$W/runs/$N
rm -rf "$d"; mkdir -p "$d/queries"
P=$(sed -n "s/^P='\(.*\)'$/\1/p" "$W/bench.sh")
for q in $QS; do
  sed -n "s/^add $q '\(.*\)'$/\1/p" "$W/bench.sh" | { read -r body; printf '%s%s' "$P" "$body" > "$d/queries/$q.rq"; }
done
[ -f $W/data.nt.zst ] || python3 $W/gen-data.py 100000 | zstd -q -T0 -10 -o $W/data.nt.zst
zstd -q -3 -f $W/bin/$A -o $d/bin-A.zst
zstd -q -3 -f $W/bin/$B -o $d/bin-B.zst
ln -s $W/data.nt.zst $d/data.nt.zst
cp $W/nsc-bench-remote.sh $d/
cat > $d/run.env <<ENV
DYNAMIC_A=0
DYNAMIC_B=0
ROUNDS=$ROUNDS
REPS=$REPS
WARMUP=5
RAYON_THREADS=1
SERVER_CPU=2
CLIENT_CPU=4
SPIN=1
PORT=41901
ENV
echo "$A $B" > $d/revs.txt
uptime > $d/uptime-before.txt
bash $d/nsc-bench-remote.sh > /dev/null
rm -rf $d/stores $d/srv $d/data.nt $d/bin-*.zst
