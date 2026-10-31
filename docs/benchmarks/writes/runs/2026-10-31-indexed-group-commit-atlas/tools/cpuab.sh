#!/usr/bin/env bash
# usage: cpuab.sh SEED OUT ROUNDS OP [BINARY]  -- instructions and cycles per serial commit,
# ordinary and grouped, in O G G O order. Runs under the heavy lock held by the caller.
set -euo pipefail
seed=$1 out=$2 rounds=$3 op=$4 bin=${5:-$HOME/lanes/gc-86/bin/sparkles-main-a202e8f2}
perf=$(nix shell nixpkgs#perf -c which perf)
mkdir -p "$out"
port=41810
for r in $(seq 1 "$rounds"); do
  for pos in 0 1 2 3; do
    case $pos in 0|3) mode=ordinary; flag=--no-group-commit;; *) mode=grouped; flag=;; esac
    d="$out/r$r-$pos-$mode"; mkdir -p "$d"
    cp -a "$seed" "$d/store"; sync -f "$d/store"
    taskset -c 0-11 "$bin" --result-cache-mb 0 $flag serve --host 127.0.0.1 --data "$d/srv" --loc "bench=$d/store" --port $port --no-access-log --timeout 600 > "$d/server.log" 2>&1 &
    spid=$!
    for i in $(seq 1 600); do curl -sf "http://127.0.0.1:$port/\$/stats/bench" >/dev/null && break; sleep 0.1; done
    for i in $(seq 1 1200); do curl -sf "http://127.0.0.1:$port/\$/geo/bench" | grep -Eq '"state": ?"ready"' && break; sleep 0.1; done
    sleep 1
    "$perf" stat -x, -e instructions:u,cycles:u,task-clock -p $spid -o "$d/perf.csv" &
    ppid=$!
    sleep 0.2
    taskset -c 12-13 ~/lanes/gc-86/bin/write-bench --addr 127.0.0.1:$port --ds bench --run "cpu$r$pos" --people 1000000 --changes 2000 --op "$op" --literal-predicate http://xmlns.com/foaf/0.1/name serial --warmup 200 > "$d/client.json"
    kill -INT $ppid; wait $ppid || true
    curl -s "http://127.0.0.1:$port/\$/metrics" > "$d/metrics.txt"
    kill $spid; wait $spid || true
    rm -rf "$d/store" "$d/srv"
  done
done
