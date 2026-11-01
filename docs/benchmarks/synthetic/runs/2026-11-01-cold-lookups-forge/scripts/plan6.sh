#!/usr/bin/env bash
# the final build against base on the whole suite, cold and warm, and its instructions
set -uo pipefail
cd ~/lanes/cold-lookups-4
Q=$(ls ~/.cache/sparkles-bench/10m/queries | grep "^[a-z].*\.rq$" | sed "s/\.rq$//" | tr "\n" " ")
QW=$(echo $Q | tr " " "\n" | grep -v export | tr "\n" " ")
./insn.sh insn-att1 2 base att1
A="base||||" B="att1||||" ./ab.sh att1-suite10 10m 7 2 $Q
A="base|||warm|" B="att1|||warm|" ./ab.sh att1-warm10 10m 21 2 $QW
echo done > plan6.done
