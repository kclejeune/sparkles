#!/usr/bin/env bash
# births alone with the client on CPU 11 (a performance core), as the published timers place it
cd ~/lanes/thread-placement-449
SETTLE=5 CLIENT_CPU=11 QUERIES=births-by-decade TIMED=20 ./run.sh exp8 12 base/all20 new/auto new/reqall new/auto18 --bin base=bin/sparkles-4bd70845 --bin new=bin/sparkles-wip1
SETTLE=5 CLIENT_CPU=11 QUERIES="bi-q3 bi-q6 bi-q1" ./run.sh exp9 4 base/all20 new/auto new/reqall --bin base=bin/sparkles-4bd70845 --bin new=bin/sparkles-wip1
