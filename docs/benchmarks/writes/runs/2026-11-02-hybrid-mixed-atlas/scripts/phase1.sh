#!/usr/bin/env bash
# phase1.sh: item 583 experiment matrix on atlas with the exp1 binary
set -uo pipefail
cd ~/lanes/hybrid-583
B=--bin; X=exp1=$HOME/lanes/hybrid-583/bin/exp1
export PLACE_ADAPT8=adaptive:8 PLACE_UPD=updates
CONC=16 python3 mixab.py out/p1-c16 5 auto all adapt8 upd $B $X
for c in 2 4 8; do CONC=$c python3 mixab.py out/p1-c$c 3 auto all $B $X; done
