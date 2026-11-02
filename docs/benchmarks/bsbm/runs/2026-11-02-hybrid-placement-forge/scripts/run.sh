#!/usr/bin/env bash
# run.sh OUT ROUNDS CONFIG... : placement.py under the heavy lock, alone on the host
set -euo pipefail
L=~/lanes/thread-placement-449
exec 9> ~/lanes/.heavy.lock; flock 9
echo "lock held $(date -u +%FT%TZ) load $(cat /proc/loadavg)"
if pgrep -x rustc >/dev/null || pgrep -x cargo >/dev/null || pgrep -f "sparkles serve|qlever-server|fuseki-server|oxigraph serve|fluree server" >/dev/null; then
  echo "refusing: something else is running"; ps -eo pid,args --sort=-pcpu | head -8 | cut -c1-150; exit 1
fi
sync; sleep ${SETTLE:-20}
python3 -I $L/placement.py "$@"
echo "RUN-DONE $(date -u +%T)"
