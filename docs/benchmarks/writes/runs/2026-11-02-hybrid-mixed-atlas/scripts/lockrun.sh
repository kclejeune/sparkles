#!/usr/bin/env bash
# lockrun.sh <cmd...>: run under ~/lanes/.heavy.lock and ~/lanes/.bench.lock, printing load first
set -uo pipefail
exec 9> ~/lanes/.heavy.lock; flock 9
exec 8> ~/lanes/.bench.lock; flock 8
echo "locks held $(date -u +%FT%TZ) load $(cat /proc/loadavg)"
"$@"
echo "LOCKRUN-EXIT=$?"
