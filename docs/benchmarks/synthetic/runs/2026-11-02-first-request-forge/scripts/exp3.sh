#!/usr/bin/env bash
# exp3.sh SCALE ROUNDS TAG QUERY VARIANT...: first-request variants on forge under the bench lock.
# In a variant, @B is the base binary, @X the experiment binary and @W the warm-up query.
set -euo pipefail
cd ~/lanes/cold-sparse-4
s=$1 n=$2 tag=$3 q=$4; shift 4
exec 9> ~/lanes/.bench.lock 8> ~/lanes/.heavy.lock; flock 9; flock 8
B=$PWD/bin/sparkles-6d518b71 X=$PWD/bin/sparkles-${XBIN:-exp1}
W='SELECT (COUNT(*) AS ?c) WHERE { VALUES ?x { 1 } }'
vs=(); for v in "$@"; do v=${v//@B/$B}; v=${v//@X/$X}; v=${v//@W/$W}; vs+=("$v"); done
timeout 3000 python3 coldprobe.py --db db$s --query ~/.cache/sparkles-bench/$s/queries/$q.rq --rounds $n --out probe-$tag-$s.json --port 43871 "${vs[@]}" > probe-$tag-$s.log 2>&1
python3 - probe-$tag-$s.json <<'PY'
import json,statistics as st,sys
d=json.load(open(sys.argv[1]))["results"]
for k,v in d.items():
    f=[x["first"][0] for x in v if x["first"]]; acc=[x["access"][0] for x in v if x["access"]]
    g=lambda key: st.median([a[key] for a in acc]) if acc else float('nan')
    fl=st.median([x["sched"]["minflt"] for x in v])
    print(f"{k:8} curl {st.median(f):.2f} [{min(f):.2f}-{max(f):.2f}] server {g('total_ms'):.2f} parse {g('parse_ms'):.2f} plan {g('plan_ms'):.2f} exec {g('exec_ms'):.2f} ser {g('serialize_ms'):.2f} faults {fl:.0f} ready {st.median([x['ready'] for x in v]):.0f}")
PY
