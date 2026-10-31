#!/usr/bin/env bash
set -euo pipefail
B=$HOME/lanes/forge-checks-440-432-20261031
exec 8> "$HOME/lanes/.bench.lock"; flock 8
exec 9> "$HOME/lanes/.heavy.lock"; flock 9
echo "locks held $(date -u +%FT%TZ)"
if pgrep -x rustc >/dev/null || pgrep -x cargo >/dev/null || pgrep -f 'sparkles serve|qlever-server|fuseki-server|oxigraph serve|fluree server' >/dev/null; then
  echo "refusing: something else is running"; ps -eo pid,args --sort=-pcpu | head -8 | cut -c1-150; exit 1
fi
sync
sleep 60
echo "start $(date -u +%T) load $(cut -d' ' -f1-3 /proc/loadavg)"
python3 $B/births_http.py ${ROUNDS:-4}
echo "all done $(date -u +%T)"
