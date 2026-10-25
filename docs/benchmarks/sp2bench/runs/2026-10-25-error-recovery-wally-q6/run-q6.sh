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
export PORT_BASE=28480 SERVER_MEM_MAX=24G CLIENT_CPU=11 WARMUP=2 RUNS=3 MAX_TIME=600 ANSWERS_RAW_DIR=raw-answers QLEVER_MEMORY=20G JENA_HEAP=16G JENA_STACK=256m
unset SPARKLES_ARGS SKIP_LOAD QUERIES COLD_RUNS ANSWERS_ONLY RAYON_NUM_THREADS
receipt="$lane/sp2b-q6-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$receipt/data"
echo "$receipt" > "$lane/active-sp2b-q6"
(date -u +%FT%TZ; hostname; uname -a; uptime; free -m; df -h "$lane"; lscpu; ps -eo pid,pcpu,rss,comm --sort=-pcpu | awk 'NR<=18') > "$receipt/preflight.txt"
for p in /sys/class/power_supply/AC/online /sys/class/power_supply/BAT0/status /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor /sys/devices/system/cpu/intel_pstate/no_turbo; do
  if [ -f "$p" ]; then printf '%s ' "$p"; cat "$p"; fi
done > "$receipt/power-state.txt"
if pgrep -u "$(id -u)" -x 'cargo|rustc|qlever-index'; then echo 'Unrelated build active' >&2; exit 1; fi
for p in "$SPARKLES" "$FUSEKI" "$TDBLOADER" "$QSERVER" "$QINDEX" "$OXIGRAPH" "$FLUREE"; do test -x "$p"; done
sha256sum "$SPARKLES" "$FUSEKI" "$TDBLOADER" "$QSERVER" "$QINDEX" "$OXIGRAPH" "$FLUREE" "$SP2B/bin/sp2b_gen" > "$receipt/binaries.sha256"
test "$(sha256sum "$SPARKLES" | cut -d ' ' -f 1)" = cf6f4cf5fd754c8344fd0f0c02b81dce5016c8b8ccfcbe034f5fe7927cfd46c8
readelf -p .comment "$SPARKLES" > "$receipt/compiler-comment.txt"
sha256sum "$ROOT"/scripts/bench-{answers.py,lib.sh,sp2b.sh,suite-lib.sh,record.py,summary.py,probe.sh} > "$receipt/harness.sha256"
printf '%s\n' 03928c16265bb101495d49ea7a625ff27b94eb6c > "$receipt/harness-source-commit.txt"
prior=$(cat "$lane/active-sp2b")
ln -s "$prior/data/data.nt" "$receipt/data/data.nt"
sha256sum "$receipt/data/data.nt" > "$receipt/source.sha256"
test "$(cut -d ' ' -f 1 "$receipt/source.sha256")" = 4486653f3ea16825923eb8a5410137df7ba712aa918442531c33cbeb854eb814
sync
sudo -n fstrim -v / > "$receipt/trim.txt" 2>&1
for engine in sparkles qlever fluree jena oxigraph; do
  work="$receipt/$engine"
  mkdir -p "$work"
  ln -s "$receipt/data/data.nt" "$work/data.nt"
  case "$engine" in
    sparkles) store=sparkles.db;;
    jena) store=jena.db;;
    qlever) store=qlever-index;;
    fluree) store=fluree;;
    oxigraph) store=oxigraph.db;;
  esac
  cp -a --reflink=auto "$prior/$engine/$store" "$work/$store"
  sync
  ENGINES="$engine" QUERIES=q6 SKIP_LOAD=1 ANSWERS_ONLY=1 bash "$ROOT/scripts/bench-sp2b.sh" 10m "$work" > "$receipt/$engine-answer.log" 2>&1 || true
  cp -a "$work/results" "$receipt/$engine-answer-results"
  if python3 - "$work/results/answers.json" "$engine" "$receipt/$engine-eligible.json" <<'CHECK'
import json,pathlib,sys
path,engine,out=sys.argv[1:];name='jena-fuseki' if engine=='jena' else engine;a=json.loads(pathlib.Path(path).read_text()).get('q6',{}).get(name,{})
expected='row-sha256-v1:9fce8c07ef87937ec498e9cdca8047e2c9cfab085a95c4817de2081f4db52479';ok=a.get('rows')==906313 and a.get('value')==expected
pathlib.Path(out).write_text(json.dumps({'query':'q6','eligible_for_timing':ok,'expected_rows':906313,'actual_rows':a.get('rows'),'expected_value':expected,'actual_value':a.get('value'),'error':a.get('error')},indent=2)+'\n')
sys.exit(not ok)
CHECK
  then
    ENGINES="$engine" QUERIES=q6 SKIP_LOAD=1 RUNS=3 bash "$ROOT/scripts/bench-sp2b.sh" 10m "$work" > "$receipt/$engine-timing.log" 2>&1 || true
    cp -a "$work/results" "$receipt/$engine-timing-results"
  fi
  date -u +%FT%TZ > "$receipt/$engine-complete.txt"
done
date -u +%FT%TZ > "$receipt/complete.txt"
