#!/usr/bin/env bash
set -euo pipefail
exec 8> "$HOME/lanes/.bench.lock"
flock -n 8 || exit 75
exec 9> "$HOME/lanes/.heavy.lock"
flock -n 9 || exit 75
lane="$HOME/lanes/benchmark-error-recovery-20261025"
export ROOT="$lane/src-reviewed"
export SPARKLES="$HOME/lanes/competitive-births-release-20261025/target-D/release/sparkles" SPARKLES_COMMIT=6b7f32ce
export SP2B="$lane/sp2b-gen"
export FUSEKI=/nix/store/q0dscszid7jzs66xq9dip95z2hc1zmsh-apache-jena-fuseki-5.1.0/bin/fuseki-server
export TDBLOADER=/nix/store/3gczhyjf9wcjssfiklprz95aj17inm8z-apache-jena-6.2.0/bin/tdb2.tdbloader
export QSERVER=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-server QINDEX=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-index
export OXIGRAPH=/nix/store/g7ajfdk0g8bypdbsj23hnfq71izm3iz1-oxigraph-0.5.11/bin/oxigraph
export FLUREE="$lane/bin/fluree-4.2.2" FLUREE_VERSION=4.2.2
export PORT_BASE=28530 SERVER_MEM_MAX=26G CLIENT_CPU=11 WARMUP=2 RUNS=3 MAX_TIME=600 ANSWERS_RAW_DIR=raw-answers QLEVER_MEMORY=20G JENA_HEAP=16G JENA_STACK=256m
unset SPARKLES_ARGS SKIP_LOAD QUERIES COLD_RUNS ANSWERS_ONLY RAYON_NUM_THREADS ANSWERS_TSV_QUERIES
receipt="$lane/optional-chain-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$receipt/data/queries"
echo "$receipt" > "$lane/active-optional-chain"
(date -u +%FT%TZ; hostname; uname -a; uptime; free -m; df -h "$lane"; lscpu; ps -eo pid,pcpu,rss,comm --sort=-pcpu | awk 'NR<=18') > "$receipt/preflight.txt"
for p in /sys/class/power_supply/AC/online /sys/class/power_supply/BAT0/status /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor /sys/devices/system/cpu/intel_pstate/no_turbo; do
 if [ -f "$p" ]; then printf '%s ' "$p"; cat "$p"; fi
done > "$receipt/power-state.txt"
sha256sum "$SPARKLES" "$FUSEKI" "$TDBLOADER" "$QSERVER" "$QINDEX" "$OXIGRAPH" "$FLUREE" "$lane/extras-optional-chain/optional-chain-oracle.py" "$lane/extras-optional-chain/gen-data.py" > "$receipt/inputs.sha256"
readelf -p .comment "$SPARKLES" > "$receipt/compiler-comment.txt"
python3 "$lane/extras-optional-chain/gen-data.py" 1000000 > "$receipt/data/data.nt"
sha256sum "$receipt/data/data.nt" > "$receipt/source.sha256"
test "$(cut -d ' ' -f 1 "$receipt/source.sha256")" = 10daf6486c20f8dd4579d819ea73bb5e7836e054cd6d2d1b73f99f35233635f8
cp "$lane/extras-optional-chain/optional-chain.rq" "$receipt/data/queries/"
python3 "$lane/extras-optional-chain/optional-chain-oracle.py" "$receipt/data/data.nt" "$receipt/data/queries/optional-chain.rq" "$ROOT/scripts/bench-answers.py" "$receipt/oracle.json" > "$receipt/oracle.log"
python3 - "$receipt/oracle.json" <<'CHECK'
import json,sys
j=json.load(open(sys.argv[1]));assert j['rows']==300191 and j['value']=='eb7b447880a4ce42' and j['source_sha256']=='10daf6486c20f8dd4579d819ea73bb5e7836e054cd6d2d1b73f99f35233635f8'
CHECK
sync
sudo -n fstrim -v / > "$receipt/trim.txt" 2>&1
for engine in sparkles qlever jena oxigraph fluree; do
 work="$receipt/$engine"
 mkdir -p "$work/results" "$work/logs" "$work/queries"
 ln -s "$receipt/data/data.nt" "$work/data.nt"
 cp "$receipt/data/queries/optional-chain.rq" "$work/queries/"
 (
  export WORK="$work" ENGINES="$engine" SCALE=10.5m SUITE=synthetic
  cd "$WORK"
  source "$ROOT/scripts/bench-lib.sh"
  source "$ROOT/scripts/bench-suite-lib.sh"
  NAMES=(optional-chain)
  COUNT_ONLY="" DATAFILE=data.nt
  DATA_SPECS=("synthetic-10.5m=$WORK/data.nt|generated|scripts/gen-data.py fixed seed42")
  export ANSWERS_ONLY=1
  suite_run
 ) > "$receipt/$engine-answer.log" 2>&1 || true
 cp -a "$work/results" "$receipt/$engine-answer-results"
 if python3 - "$work/results/answers.json" "$receipt/oracle.json" "$engine" "$receipt/$engine-eligible.json" <<'CHECK'
import json,pathlib,sys
p,o,e,out=sys.argv[1:];n='jena-fuseki' if e=='jena' else e;a=json.load(open(p)).get('optional-chain',{}).get(n,{});expected=json.load(open(o));ok=all(a.get(k)==expected[k] for k in ['rows','value','query_sha256']);pathlib.Path(out).write_text(json.dumps({'engine':e,'eligible_for_timing':ok,'expected':expected,'actual':a},indent=2)+'\n');sys.exit(not ok)
CHECK
 then
  (
   export WORK="$work" ENGINES="$engine" SCALE=10.5m SUITE=synthetic
   cd "$WORK"
   source "$ROOT/scripts/bench-lib.sh"
   source "$ROOT/scripts/bench-suite-lib.sh"
   NAMES=(optional-chain)
   COUNT_ONLY="" DATAFILE=data.nt SKIP_LOAD=1
   DATA_SPECS=("synthetic-10.5m=$WORK/data.nt|generated|scripts/gen-data.py fixed seed42")
   suite_run
  ) > "$receipt/$engine-timing.log" 2>&1 || true
  cp -a "$work/results" "$receipt/$engine-timing-results"
 fi
 date -u +%FT%TZ > "$receipt/$engine-complete.txt"
done
date -u +%FT%TZ > "$receipt/complete.txt"
