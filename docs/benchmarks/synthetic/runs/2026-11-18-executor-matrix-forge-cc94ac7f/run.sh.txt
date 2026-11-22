#!/usr/bin/env bash
# stage1.sh: under ~/lanes/.bench.lock and ~/lanes/.heavy.lock, the pinned post-push checks
# (24 rounds of A,B,B,A, 200 timed requests, 1 thread, 1.05M triples) and then the
# eager-against-cursor matrix of 8baa5686 with the scripts of 8baa5686.
set -euo pipefail
W=$HOME/lanes/forge-backlog
exec 8> "$HOME/lanes/.bench.lock"; flock 8
exec 9> "$HOME/lanes/.heavy.lock"; flock 9
echo "STAGE1 locks held $(date -u +%FT%TZ) load $(cat /proc/loadavg)"
if pgrep -x rustc >/dev/null || pgrep -x cargo >/dev/null || pgrep -x sparkles >/dev/null || pgrep -f 'qlever-server|fuseki-server|oxigraph serve' >/dev/null; then
  echo "STAGE1-REFUSED: something else is running"; ps -eo pid,args --sort=-pcpu | head -8 | cut -c1-150; exit 1
fi
M=8baa5686
for spec in path-plus:06ca04ef star-lookup:c2b5d7cf minus:adbe757c distinct-obj-1047:f854145b small-856:321508fe; do
  IFS=: read -r name a <<< "$spec"
  echo "pair $name $a $M $(date -u +%T) load $(cut -d' ' -f1-3 /proc/loadavg)"
  bash $W/ab.sh "$name" "$a" "$M" 24 200
  echo "pair $name done $(date -u +%T) $(cat $W/runs/$name/results/done)"
done
python3 $W/analyze.py $W/runs/{path-plus,star-lookup,minus,distinct-obj-1047,small-856} > $W/runs/pinned-summary.json 2> $W/runs/pinned-summary.txt
echo "PINNED-DONE $(date -u +%FT%TZ)"
cat $W/runs/pinned-summary.txt
S=$W/src-8baa5686
# the matrix of cc94ac7f (main before item 825, the fixed X14 baseline) and of 8baa5686, one after
# the other with the same scripts, data and host
for rev in cc94ac7f $M; do
  if [ ! -f $W/bin/$rev-matrix.zst ]; then echo "MATRIX-SKIPPED $rev: no binary"; continue; fi
  zstd -q -d -f $W/bin/$rev-matrix.zst -o $W/bin/sparkles-$rev-matrix; chmod +x $W/bin/sparkles-$rev-matrix || { echo "MATRIX-FAILED $rev"; continue; }
  echo "matrix $rev start $(date -u +%FT%TZ) load $(cat /proc/loadavg)"
  $S/scripts/bench-executors.sh --out $W/matrix-$rev --commit $rev --data-dir $W/data \
    --build-config "$(cat $W/bin/$rev-matrix.build-config.json)" $W/bin/sparkles-$rev-matrix || { echo "MATRIX-FAILED $rev"; continue; }
  echo "MATRIX-DONE $rev $(date -u +%FT%TZ) load $(cat /proc/loadavg)"
done
echo STAGE1-DONE
