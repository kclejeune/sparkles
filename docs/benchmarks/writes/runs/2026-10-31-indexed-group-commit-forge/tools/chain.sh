#!/usr/bin/env bash
set -e
cd ~/lanes/gc-86 || exit 1
B="--binary bin/sparkles-main-a202e8f2 --write-bench bin/write-bench --source-commit a202e8f2 --blocks 5 --port 41800 --provider-port 41801"
./runlocked.sh runs/f1.log $B --out runs/f1-geo10m-serial --people 1000000 --indexes geo --scenarios serial-literal serial-iri serial-delete
echo done > runs/f1.done
./runlocked.sh runs/f2.log $B --out runs/f2-writers-1m --people 100000 --indexes geo text --scenarios writers
echo done > runs/f2.done
./runlocked.sh runs/f3.log $B --out runs/f3-geo1m-mixed --people 100000 --indexes geo --scenarios mixed --slow-sync-ms 1.6
echo done > runs/f3.done
