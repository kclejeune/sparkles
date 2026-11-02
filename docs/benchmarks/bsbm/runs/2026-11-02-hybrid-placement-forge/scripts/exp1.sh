#!/usr/bin/env bash
set -uo pipefail
cd ~/lanes/thread-placement-449
rm -rf smoke
TIMED=3 UNTIMED=1 SETTLE=1 QUERIES="bi-q3-1 births-by-decade" ./run.sh smoke 1 all20 e7 || exit 1
python3 check_smoke.py smoke/records.json || exit 1
./run.sh exp1 4 all20 all12 p12 e7 pe19 p6 p12e7x12
