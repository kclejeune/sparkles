#!/usr/bin/env bash
# the lane measurements on the final build, each step taking the heavy lock on its own
set -uo pipefail
cd ~/lanes/cold-lookups-4
./build.sh fin
Q=$(ls ~/.cache/sparkles-bench/10m/queries | grep "^[a-z].*\.rq$" | sed "s/\.rq$//" | tr "\n" " ")
QW=$(echo $Q | tr " " "\n" | grep -v export | tr "\n" " ")
P="star-lookup values-star employee-docs path-plus star-join"
A="base||||" B="fin||||" ./ab.sh suite10 10m 7 2 $Q
A="base||||" B="fin||||" ./ab.sh suite1 1m 7 1 $Q
A="pre3||||-u" B="fin||||" ./ab.sh unseg-fin 10m 11 2 star-join employee-docs star-lookup knows-reach
A="fin||||" B="fin|||--prewarm|" ./ab.sh prewarm10 10m 7 1 $P
A="fin||||" B="fin|||--prewarm|" ./ab.sh prewarm1 1m 7 1 $P
A="base|||warm|" B="fin|||warm|" ./ab.sh warm10 10m 21 2 $QW
echo done > plan.done
