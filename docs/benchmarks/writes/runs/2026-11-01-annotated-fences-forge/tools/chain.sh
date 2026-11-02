#!/usr/bin/env bash
# The campaigns of the 501 and 502 lane, in order, after the drive probes. Each takes the heavy lock.
cd ~/lanes/fence-sync-501-502 || exit 1
until grep -q "^exit" probe/run4.txt 2>/dev/null; do sleep 15; done
AB="--binary bin/sparkles-4bd70845 --compare-binary bin/sparkles-4082759b --write-bench bin/write-bench --source-commit 4bd70845-vs-4082759b --port 42900 --provider-port 42901 --message fence-sync"
./runlocked.sh runs/m1.log $AB --blocks 5 --out runs/m1-writers-1m --people 100000 --indexes geo text --scenarios writers
echo "done $?" > runs/m1.done
./runlocked.sh runs/m2.log $AB --blocks 3 --out runs/m2-serial-1m --people 100000 --indexes geo --scenarios serial-literal
echo "done $?" > runs/m2.done
