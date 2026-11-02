#!/usr/bin/env bash
# exp2.sh SCALE ROUNDS TAG: first-request variants on forge under the bench lock
set -euo pipefail
cd ~/lanes/cold-sparse-4
s=$1 n=$2 tag=$3
exec 9> ~/lanes/.bench.lock 8> ~/lanes/.heavy.lock; flock 9; flock 8
B=$PWD/bin/sparkles-6d518b71 X=$PWD/bin/sparkles-exp1
W='SELECT (COUNT(*) AS ?c) WHERE { VALUES ?x { 1 } }'
timeout 3000 python3 coldprobe.py --db db$s --query ~/.cache/sparkles-bench/$s/queries/path-plus.rq --rounds $n --out probe-$tag-$s.json --port 43871 \
  base=$B exp1=$X thp0=$X,MIMALLOC_ALLOW_THP=0 pf=$X,XCOLD_PREFAULT=1 pq=$X,XCOLD_PROBE_QUIET=1 \
  "warm=$X,WARM=$W,WARM_DELAY_MS=15" "all=$X,XCOLD_PREFAULT=1,XCOLD_PROBE_QUIET=1,MIMALLOC_ALLOW_THP=0" > probe-$tag-$s.log 2>&1
python3 - probe-$tag-$s.json <<'PY'
import json,statistics as st,sys
d=json.load(open(sys.argv[1]))["results"]
for k,v in d.items():
    f=[x["first"][0] for x in v if x["first"]]; acc=[x["access"][0] for x in v if x["access"]]
    g=lambda key: st.median([a[key] for a in acc]) if acc else float('nan')
    fl=st.median([x["sched"]["minflt"] for x in v])
    print(f"{k:6} curl {st.median(f):.2f} [{min(f):.2f}-{max(f):.2f}] server {g('total_ms'):.2f} parse {g('parse_ms'):.2f} plan {g('plan_ms'):.2f} exec {g('exec_ms'):.2f} ser {g('serialize_ms'):.2f} faults {fl:.0f} ready {st.median([x['ready'] for x in v]):.0f}")
PY
