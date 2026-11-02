#!/usr/bin/env bash
# affsnap.sh <n>: n times, wait for a fix2 server without --request-cpus all running >15 s, dump thread affinities
BIN=${BIN:-fix2}
for k in $(seq $1); do
  for i in $(seq 600); do
    pid=$(pgrep -af "bin/$BIN --result-cache-mb 0 serve" | grep -v "request-cpus all" | awk '{print $1}' | head -1)
    if [ -n "$pid" ] && [ "$(ps -o etimes= -p $pid | tr -d ' ')" -gt 15 ] 2>/dev/null; then
      echo "== pid=$pid $(date -u +%T)"
      for t in /proc/$pid/task/*; do echo "$(cat $t/comm) $(taskset -pc $(basename $t) 2>/dev/null | sed 's/.*: //')"; done | sort | uniq -c | sort -rn | grep -v " 0-19$" 
      while kill -0 $pid 2>/dev/null; do sleep 1; done
      break
    fi
    sleep 1
  done
done
echo SNAPDONE
