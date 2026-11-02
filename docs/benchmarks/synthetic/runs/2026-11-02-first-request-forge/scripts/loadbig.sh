#!/usr/bin/env bash
# loadbig.sh: load the full DBpedia (1.01B) into a format 3 store under both forge locks
set -euo pipefail
L=~/lanes/cold-sparse-4
exec 8> "$HOME/lanes/.heavy.lock"; flock 8
exec 9> "$HOME/lanes/.bench.lock"; flock 9
df -h / | tail -1
rm -rf $L/dbfull
mapfile -t FILES < <(ls ~/.cache/sparkles-bench/billion/nt/*.nt.zst)
echo "${#FILES[@]} files"
/usr/bin/env time -v -o $L/load-full.time systemd-run --user --scope -q -p MemoryMax=13G -p MemorySwapMax=2G -p OOMPolicy=continue -- \
  $L/bin/sparkles-6d518b71 load --lenient --loc $L/dbfull "${FILES[@]}" > $L/load-full.log 2>&1
tail -2 $L/load-full.log; grep -E 'Elapsed|Maximum resident' $L/load-full.time; du -sh $L/dbfull
echo LOADDONE
