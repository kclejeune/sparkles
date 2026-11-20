#!/usr/bin/env bash
# Instruction-count ladder of the drift queries across the matrix builds (streaming, one thread).
set -uo pipefail
L=.artifacts/lanes/forge-backlog
W=.artifacts/worktrees/forge-backlog
cd $W
pairs="cc94ac7f:8baa5686 8baa5686:eda22fce eda22fce:bccc5f7b bccc5f7b:ffb18866 ffb18866:1cbc23cd 1cbc23cd:71c277b0"
for p in $pairs; do
  a=${p%:*}; b=${p#*:}
  echo "LADDER $a $b $(date -u +%T)"
  python3 scripts/request-insn.py run --a $L/bin/sparkles-$a-matrix --b $L/bin/sparkles-$b-matrix \
    --data $L/insn/people-100000.nt.zst --queries $L/insn/queries --separate-stores --execution streaming \
    --rounds 4 --reps 50 --server-cpu 2 --client-cpu 3 --out $L/insn/$a-$b > $L/insn/$a-$b.log 2>&1 \
    && python3 scripts/request-insn.py summarize $L/insn/$a-$b > $L/insn/$a-$b.summary 2>&1 \
    || echo "LADDER-PAIR-FAILED $a $b"
done
echo LADDER-DONE
