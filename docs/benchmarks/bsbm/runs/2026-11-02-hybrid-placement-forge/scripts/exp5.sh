#!/usr/bin/env bash
# new binary with its own placement against base, BI suite and births
cd ~/lanes/thread-placement-449
./run.sh exp5 4 base/all20 base/split16 new/auto new/auto18 new/reqall --bin base=bin/sparkles-4bd70845 --bin new=bin/sparkles-wip1
# births alone over many processes
SETTLE=5 QUERIES=births-by-decade TIMED=20 ./run.sh exp6 12 base/all20 new/auto new/reqall --bin base=bin/sparkles-4bd70845 --bin new=bin/sparkles-wip1
