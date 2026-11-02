#!/usr/bin/env bash
# quiet.sh: wait until no compiler, test runner or test binary runs on this host and the 1-minute load is under 2
busy() { pgrep -x "rustc|cargo|cargo-nextest|ld|ld.lld|mold|java|node" >/dev/null || pgrep -f "target/(debug|release)/deps/" >/dev/null; }
while busy || awk '{exit !($1 >= 2)}' /proc/loadavg; do sleep 10; done
echo "quiet $(date -u +%T) $(cat /proc/loadavg)"
