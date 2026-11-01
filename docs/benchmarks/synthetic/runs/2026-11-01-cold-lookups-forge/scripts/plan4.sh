#!/usr/bin/env bash
# placement attempts: build att1, front-end counters, then warm and cold A/B against base
set -uo pipefail
cd ~/lanes/cold-lookups-4
./build.sh att1
./fe.sh fe1 2 base fin att1
P="group-avg expr-agg-arg star-join star-lookup employee-docs"
A="base|||warm|" B="att1|||warm|" ./ab.sh att1-warm 10m 21 2 $P
A="base||||" B="att1||||" ./ab.sh att1-cold 10m 11 2 $P
echo done > plan4.done
