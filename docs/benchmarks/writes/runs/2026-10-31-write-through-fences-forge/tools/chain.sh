#!/usr/bin/env bash
# The campaigns of the 446 and 447 lane, in order. Each takes the heavy lock for itself.
cd ~/lanes/group-commit-446-447-20261031 || exit 1
TIP=3da475a9
AB="--binary bin/sparkles-base-8127961f --compare-binary bin/sparkles-tip-$TIP --write-bench bin/write-bench --source-commit 8127961f-vs-$TIP --port 42800 --provider-port 42801"
GATE="--binary bin/sparkles-tip-$TIP --write-bench bin/write-bench --source-commit $TIP --port 42800 --provider-port 42801"
./runlocked.sh runs/ab1.log $AB --blocks 5 --out runs/ab1-writers-1m --people 100000 --indexes geo text --scenarios writers
echo "done $?" > runs/ab1.done
./runlocked.sh runs/g1.log $GATE --blocks 5 --out runs/g1-writers-1m --people 100000 --indexes geo text --scenarios writers
echo "done $?" > runs/g1.done
./runlocked.sh runs/ab2.log $AB --blocks 3 --out runs/ab2-serial-1m --people 100000 --indexes geo --scenarios serial-literal
echo "done $?" > runs/ab2.done
