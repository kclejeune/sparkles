#!/usr/bin/env bash
# ab.sh NAME A B ROUNDS REPS: interleaved A,B,B,A rounds on forge, locks held by the caller
set -euo pipefail
W=${W:-$HOME/lanes/forge-backlog}
N=$1 A=$2 B=$3 ROUNDS=$4 REPS=$5
QS="${QS:-path-plus star-lookup minus distinct-obj count-all predicate-counts predicate-subjects}"
d=$W/runs/$N
rm -rf "$d"; mkdir -p "$d/queries"
P=$(sed -n "s/^P='\(.*\)'$/\1/p" "$W/bench.sh")
for q in $QS; do
  body=$(sed -n "s/^add $q '\(.*\)'$/\1/p" "$W/bench.sh")
  [ -n "$body" ] || { echo "no query $q"; exit 1; }
  printf '%s%s' "$P" "$body" > "$d/queries/$q.rq"
done
[ -f $W/data.nt.zst ] || python3 $W/gen-data.py ${PEOPLE:-100000} | zstd -q -T0 -10 -o $W/data.nt.zst
ln -s $W/bin/$A.zst $d/bin-A.zst
ln -s $W/bin/$B.zst $d/bin-B.zst
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
PORT=41911
ENV
echo "$A $B" > $d/revs.txt
uptime > $d/uptime-before.txt
bash $d/nsc-bench-remote.sh > /dev/null
uptime > $d/uptime-after.txt
rm -rf $d/stores $d/A $d/B $d/data.nt
