#!/usr/bin/env bash
# usage: runlocked.sh LOG harness-args...  Waits until the heavy lock is free and no other
# process uses a full CPU, then runs the harness, which holds the lock for the campaign.
# Exits with the harness status, or 1 when the wait expires, so chain.sh stops on a failure.
log=$1; shift
cd ~/lanes/gc-86
for i in $(seq 1 1440); do
  if flock -n ~/lanes/.heavy.lock true; then
    busy=$(ps -eo pcpu=,pid=,args= | awk "\$1 > 50" | grep -v -e bench-indexed -e runlocked)
    if [ -z "$busy" ]; then
      { date; uptime; ps -eo pid,pcpu,etime,args --sort=-pcpu | head -15; } > "$log.start"
      python3 src/scripts/bench-indexed-commit.py "$@" > "$log" 2>&1
      rc=$?
      echo "exit $rc" >> "$log"
      if [ $rc -ne 0 ] && grep -q BlockingIOError "$log"; then sleep 30; continue; fi
      exit $rc
    fi
    echo "$(date) busy: $busy" >> "$log.wait"
  fi
  sleep 30
done
echo "gave up waiting for the heavy lock" > "$log"
exit 1
