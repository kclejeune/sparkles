#!/usr/bin/env bash
# billion.sh TAG BIN RUNS PAIRS QUERIES...: cold read-policy variants on the 1.01B store, under the bench lock.
# Variants: default hints, SPARKLES_IO_HINTS=off, both switches off, and read windows of 1, 16 MiB (4 MiB is the default).
set -euo pipefail
L=~/lanes/cold-sparse-4
tag=$1 bin=$2 runs=$3 pairs=$4; shift 4
QS=("$@")
exec 9> "$HOME/lanes/.bench.lock"; flock 9
out=$L/out/$tag; mkdir -p $out
{ date -u +%FT%TZ; hostname; uname -r; sha256sum $L/bin/sparkles-$bin; readelf -p .comment $L/bin/sparkles-$bin | grep -v '^$'; echo "queries=${QS[*]} runs=$runs pairs=$pairs"; } > $out/env.txt
declare -A ENVS=([on]="" [off]="SPARKLES_IO_HINTS=off" [bothoff]="SPARKLES_IO_HINTS=off SPARKLES_SPARSE_VOCAB=off" [w1]="SPARKLES_READ_WINDOW_KB=1024" [w16]="SPARKLES_READ_WINDOW_KB=16384")
ORDER=(${VARIANTS:-on off bothoff w1 w16})
i=0
for p in $(seq 1 $pairs); do
  if [ $((p % 2)) = 1 ]; then seq_=("${ORDER[@]}"); else seq_=($(printf '%s\n' "${ORDER[@]}" | tac)); fi
  for v in "${seq_[@]}"; do
    i=$((i+1))
    env ${ENVS[$v]} python3 $L/src/scripts/bench-regressions.py --binary $L/bin/sparkles-$bin --db $L/dbfull \
      --queries $L/src/scripts/bench-billion/queries --names "${QS[@]}" --out $out/$v-$i.json \
      --runs $runs --cold --keep-going --port 43800 >> $out/stdout.log 2>&1
    echo "$(date -u +%T) $v-$i" >> $out/progress.log
  done
done
python3 - $out <<'PY'
import json, os, statistics, sys, collections
d = sys.argv[1]; acc = collections.defaultdict(list); hashes = collections.defaultdict(set)
for f in sorted(os.listdir(d)):
    if not f.endswith('.json'): continue
    v = f.rsplit('-', 1)[0]
    for q, r in json.load(open(os.path.join(d, f))).items():
        if isinstance(r, dict) and 'ms' in r:
            acc[(q, v)] += r['ms']; hashes[q] |= set(r.get('hashes', []))
qs = sorted({q for q, _ in acc}); vs = sorted({v for _, v in acc})
print('query', *vs, 'hashes', sep='\t')
for q in qs:
    print(q, *[f"{statistics.median(acc[(q, v)]):.1f}" if acc[(q, v)] else '-' for v in vs], len(hashes[q]), sep='\t')
PY
echo BILLIONDONE
