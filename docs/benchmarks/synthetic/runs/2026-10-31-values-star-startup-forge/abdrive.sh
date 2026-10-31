#!/usr/bin/env bash
# abdrive.sh ROUNDS REPS NAME:A:B...  runs the pairs one after another under both locks
set -euo pipefail
W=$HOME/lanes/forge-checks-440-432-20261031
exec 8> "$HOME/lanes/.bench.lock"; flock 8
exec 9> "$HOME/lanes/.heavy.lock"; flock 9
echo "locks held $(date -u +%FT%TZ)"
ROUNDS=$1 REPS=$2; shift 2
# nothing else may run
if pgrep -x rustc >/dev/null || pgrep -x cargo >/dev/null || pgrep -f 'sparkles serve|qlever-server|fuseki-server|oxigraph serve|fluree server' >/dev/null; then
  echo "refusing: something else is running"; ps -eo pid,args --sort=-pcpu | head -8 | cut -c1-150; exit 1
fi
echo "load: $(cat /proc/loadavg)"
for spec in "$@"; do
  IFS=: read -r name a b <<< "$spec"
  echo "pair $name $a $b $(date -u +%T) load $(cut -d' ' -f1-3 /proc/loadavg)"
  bash $W/ab.sh "$name" "$a" "$b" "$ROUNDS" "$REPS"
  echo "pair $name done $(date -u +%T)"
done
echo "all done"
