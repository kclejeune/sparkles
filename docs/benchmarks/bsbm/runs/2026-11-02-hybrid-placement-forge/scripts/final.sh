#!/usr/bin/env bash
# item 449 final measurements of FINAL (bin/sparkles-<commit>) against base 4bd70845 on forge
set -euo pipefail
L=~/lanes/thread-placement-449
S=$L/src-4bd70845
C=$1
F=$L/bin/sparkles-$C
exec 8> ~/lanes/.bench.lock; flock 8
exec 9> ~/lanes/.heavy.lock; flock 9
echo "locks held $(date -u +%FT%TZ) load $(cat /proc/loadavg)"
if pgrep -x rustc >/dev/null || pgrep -x cargo >/dev/null || pgrep -f "sparkles serve|qlever-server|fuseki-server|oxigraph serve|fluree server" >/dev/null; then
  echo "refusing: something else is running"; ps -eo pid,args --sort=-pcpu | head -8 | cut -c1-150; exit 1
fi
O=$L/final-$C; mkdir -p $O
preflight() { { date -u +%FT%TZ; cat /proc/loadavg; free -m; df -h /; for c in 0 12; do echo "cpu$c $(cat /sys/devices/system/cpu/cpu$c/cpufreq/scaling_governor) $(cat /sys/devices/system/cpu/cpu$c/cpufreq/energy_performance_preference)"; done; } > "$1"; }
sync; sleep 30
# placement harness: BI suite and births, then births alone over many processes
preflight $O/preflight-placement.txt
python3 -I $L/placement.py $O/placement 4 base/all20 final/auto final/reqall --bin base=$L/bin/sparkles-4bd70845 --bin final=$F
QUERIES=births-by-decade TIMED=20 python3 -I $L/placement.py $O/births-processes 12 base/all20 final/auto --bin base=$L/bin/sparkles-4bd70845 --bin final=$F
# births as an explicit cursor and eagerly
NOLOCK=1 EXP=final-$C/cursor NEW=sparkles-$C $L/exp10.sh
# births under the complete-HTTP timer against QLever
preflight $O/preflight-births.txt
(cd $O && NEW_BIN=$F python3 -I $L/births449.py 12)
# BSBM BI under the published contract
export SERVER_MEM_MAX=12G CLIENT_CPU=11 SKIP_LOAD=1 SKIP_QMPH=1 USECASE=bi WARMUP=10 RUNS=30 SPARKLES_ALLOW_GPL_BENCHMARK=1
export QSERVER=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-server
export QINDEX=/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-index
export JAVA=/nix/store/0fwxx5ym8irn3ik35ibpf2x7881p9q8v-openjdk-headless-17.0.20.1+1/bin/java
cd $S
for pass in "base-first 4bd70845 sparkles qlever" "final-first $C sparkles qlever" "final-second $C qlever sparkles"; do
  set -- $pass
  name=$1; commit=$2; bin=$L/bin/sparkles-$commit
  preflight $O/preflight-bsbm-$name.txt
  echo "pass $name $(date -u +%T)"
  SPARKLES=$bin SPARKLES_COMMIT=$commit ENGINES="$3 $4" scripts/bench-bsbm.sh 10m $L/bsbm-10m > $O/bsbm-$name.log 2>&1
  rm -rf $O/bsbm-$name; cp -a $L/bsbm-10m/results $O/bsbm-$name
done
(cd $O && python3 $L/bsbm_pooled.py base-first final-first final-second)
echo "FINAL-DONE $(date -u +%T)"
