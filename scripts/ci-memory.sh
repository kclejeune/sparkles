#!/usr/bin/env bash
# Print the runner's memory, swap and load, and the largest compiler and linker processes,
# every INTERVAL seconds until killed. CI's test build runs it in the background so that a
# build that runs out of memory leaves a record of what was running.
#
# Usage: scripts/ci-memory.sh [INTERVAL]
set -uo pipefail

interval=${1:-30}

# A stopped sampler also stops its sleep, which would otherwise hold the step's output open.
sleeper=
trap '[ -z "$sleeper" ] || kill "$sleeper" 2> /dev/null; exit 0' TERM INT

while :; do
  mem=$(awk '
    /^MemAvailable:/ { avail = $2 }
    /^SwapTotal:/ { swap_total = $2 }
    /^SwapFree:/ { swap_free = $2 }
    END { printf "available %d MiB, swap used %d MiB", avail / 1024, (swap_total - swap_free) / 1024 }
  ' /proc/meminfo)
  load=$(cut -d' ' -f1-3 /proc/loadavg)
  # The four largest processes by resident memory, named by their crate when they are
  # compilers and by their command otherwise.
  top=$(ps -eo rss=,args= --sort=-rss 2> /dev/null | awk '
    NR > 4 { exit }
    {
      name = $2
      sub(".*/", "", name)
      for (i = 3; i < NF; i++) {
        if ($i == "--crate-name") { name = $(i + 1) }
        if ($i == "--test") { kind = ":test" }
      }
      printf "%s%s %d MiB  ", name, kind, $1 / 1024
      kind = ""
    }
  ')
  echo "memory $(date -u +%H:%M:%S): $mem, load $load; $top"
  sleep "$interval" &
  sleeper=$!
  wait "$sleeper"
done
